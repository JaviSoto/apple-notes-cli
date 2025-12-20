use crate::model::{Account, Folder, NoteSummary};
use anyhow::{Context, anyhow};
use rusqlite::{Connection, OpenFlags, Row};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct DbFolderRow {
    pk: i64,
    name: String,
    parent_pk: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NotesDb {
    path: PathBuf,
    store_uuid: String,
}

impl NotesDb {
    pub fn open_default() -> anyhow::Result<Self> {
        let path = default_notes_db_path().ok_or_else(|| anyhow!("unsupported platform"))?;
        Self::open(path)
    }

    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let conn = open_readonly(&path)?;
        let store_uuid: String = conn
            .query_row(
                "SELECT Z_UUID FROM Z_METADATA WHERE Z_VERSION = 1",
                [],
                |row| row.get(0),
            )
            .with_context(|| format!("read Z_METADATA from {}", path.display()))?;

        Ok(Self { path, store_uuid })
    }

    pub fn list_accounts(&self) -> anyhow::Result<Vec<Account>> {
        let conn = open_readonly(&self.path)?;
        let mut stmt = conn
            .prepare("SELECT ZNAME FROM ZICCLOUDSYNCINGOBJECT WHERE Z_ENT = 14 ORDER BY ZNAME")?;

        let iter = stmt.query_map([], |row| Ok(Account { name: row.get(0)? }))?;
        let mut out = Vec::new();
        for a in iter {
            out.push(a?);
        }
        Ok(out)
    }

    pub fn list_folders(&self, account: &str) -> anyhow::Result<Vec<Folder>> {
        let conn = open_readonly(&self.path)?;
        let account_pk = account_pk(&conn, account)?;
        let rows = folder_rows(&conn, account_pk)?;
        let mut by_pk: HashMap<i64, DbFolderRow> = HashMap::new();
        for r in rows {
            by_pk.insert(r.pk, r);
        }

        let mut out = Vec::new();
        for r in by_pk.values() {
            let path = folder_path(&by_pk, r.pk)?;
            out.push(Folder {
                id: self.folder_id(r.pk),
                name: r.name.clone(),
                account: account.to_string(),
                path,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    pub fn list_notes(&self, account: &str) -> anyhow::Result<Vec<NoteSummary>> {
        let conn = open_readonly(&self.path)?;
        let account_pk = account_pk(&conn, account)?;
        let mut stmt = conn.prepare(
            r#"
SELECT n.Z_PK, n.ZTITLE1, n.ZFOLDER
FROM ZICCLOUDSYNCINGOBJECT n
JOIN ZICCLOUDSYNCINGOBJECT f ON f.Z_PK = n.ZFOLDER
WHERE n.Z_ENT = 12
  AND IFNULL(n.ZMARKEDFORDELETION, 0) = 0
  AND f.Z_ENT = 15
  AND f.ZACCOUNT8 = ?
"#,
        )?;

        let iter = stmt.query_map([account_pk], |row| note_summary_row(self, row))?;
        let mut out = Vec::new();
        for n in iter {
            out.push(n?);
        }
        Ok(out)
    }

    pub fn list_notes_in_folder(
        &self,
        account: &str,
        folder_path: &[String],
    ) -> anyhow::Result<Vec<NoteSummary>> {
        let folders = self.list_folders(account)?;
        let want = folder_path.join(" > ");
        let folder = folders
            .iter()
            .find(|f| f.path_string() == want)
            .ok_or_else(|| anyhow!("folder not found: {want}"))?;

        let conn = open_readonly(&self.path)?;
        let folder_pk = parse_coredata_pk(&folder.id)
            .with_context(|| format!("unexpected folder id format: {}", folder.id))?;

        let mut stmt = conn.prepare(
            r#"
SELECT Z_PK, ZTITLE1, ZFOLDER
FROM ZICCLOUDSYNCINGOBJECT
WHERE Z_ENT = 12
  AND IFNULL(ZMARKEDFORDELETION, 0) = 0
  AND ZFOLDER = ?
"#,
        )?;
        let iter = stmt.query_map([folder_pk], |row| note_summary_row(self, row))?;
        let mut out = Vec::new();
        for n in iter {
            out.push(n?);
        }
        Ok(out)
    }

    pub fn note_id(&self, pk: i64) -> String {
        format!("x-coredata://{}/ICNote/p{}", self.store_uuid, pk)
    }

    pub fn folder_id(&self, pk: i64) -> String {
        format!("x-coredata://{}/ICFolder/p{}", self.store_uuid, pk)
    }
}

fn note_summary_row(db: &NotesDb, row: &Row<'_>) -> rusqlite::Result<NoteSummary> {
    let pk: i64 = row.get(0)?;
    let title: Option<String> = row.get(1)?;
    let folder_pk: i64 = row.get(2)?;
    Ok(NoteSummary {
        id: db.note_id(pk),
        title: title.unwrap_or_else(|| "Untitled".to_string()),
        folder_id: db.folder_id(folder_pk),
    })
}

fn account_pk(conn: &Connection, account: &str) -> anyhow::Result<i64> {
    conn.query_row(
        "SELECT Z_PK FROM ZICCLOUDSYNCINGOBJECT WHERE Z_ENT = 14 AND ZNAME = ?",
        [account],
        |row| row.get::<_, i64>(0),
    )
    .with_context(|| format!("account not found: {account}"))
}

fn folder_rows(conn: &Connection, account_pk: i64) -> anyhow::Result<Vec<DbFolderRow>> {
    let mut stmt = conn.prepare(
        r#"
SELECT Z_PK, COALESCE(ZNAME, ZTITLE2, 'Untitled'), ZPARENT
FROM ZICCLOUDSYNCINGOBJECT
WHERE Z_ENT = 15
  AND ZACCOUNT8 = ?
"#,
    )?;

    let iter = stmt.query_map([account_pk], |row| {
        Ok(DbFolderRow {
            pk: row.get(0)?,
            name: row.get(1)?,
            parent_pk: row.get(2)?,
        })
    })?;

    let mut out = Vec::new();
    for r in iter {
        out.push(r?);
    }
    Ok(out)
}

fn folder_path(by_pk: &HashMap<i64, DbFolderRow>, pk: i64) -> anyhow::Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = pk;
    let mut seen: HashSet<i64> = HashSet::new();
    loop {
        if !seen.insert(current) {
            return Err(anyhow!("folder parent cycle detected at pk {}", current));
        }
        let row = by_pk
            .get(&current)
            .ok_or_else(|| anyhow!("unknown folder pk {}", current))?;
        parts.push(row.name.clone());
        match row.parent_pk {
            None => break,
            Some(p) => {
                if !by_pk.contains_key(&p) {
                    break;
                }
                current = p;
            }
        }
    }
    parts.reverse();
    Ok(parts)
}

fn open_readonly(path: &Path) -> anyhow::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_SHARED_CACHE,
    )
    .with_context(|| format!("open notes db {}", path.display()))
}

fn default_notes_db_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home).join("Library/Group Containers/group.com.apple.notes/NoteStore.sqlite"),
    )
}

fn parse_coredata_pk(coredata_id: &str) -> anyhow::Result<i64> {
    // x-coredata://<uuid>/<Entity>/p123
    let Some(last) = coredata_id.rsplit('/').next() else {
        return Err(anyhow!("invalid coredata id: {coredata_id}"));
    };
    let Some(pk) = last.strip_prefix('p') else {
        return Err(anyhow!("invalid coredata id: {coredata_id}"));
    };
    pk.parse::<i64>()
        .with_context(|| format!("invalid coredata pk in id: {coredata_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn db_lists_folders_and_notes() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("NoteStore.sqlite");
        let conn = Connection::open(&db_path).unwrap();

        conn.execute(
            "CREATE TABLE Z_METADATA (Z_VERSION INTEGER PRIMARY KEY, Z_UUID VARCHAR(255), Z_PLIST BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO Z_METADATA(Z_VERSION, Z_UUID) VALUES (1, 'UUID')",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE ZICCLOUDSYNCINGOBJECT (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, ZNAME VARCHAR, ZTITLE1 VARCHAR, ZTITLE2 VARCHAR, ZFOLDER INTEGER, ZPARENT INTEGER, ZACCOUNT8 INTEGER, ZMARKEDFORDELETION INTEGER)",
            [],
        )
        .unwrap();

        // account
        conn.execute(
            "INSERT INTO ZICCLOUDSYNCINGOBJECT(Z_PK, Z_ENT, ZNAME) VALUES (1, 14, 'iCloud')",
            [],
        )
        .unwrap();

        // folders
        conn.execute(
            "INSERT INTO ZICCLOUDSYNCINGOBJECT(Z_PK, Z_ENT, ZNAME, ZPARENT, ZACCOUNT8) VALUES (10, 15, 'Personal', NULL, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZICCLOUDSYNCINGOBJECT(Z_PK, Z_ENT, ZNAME, ZPARENT, ZACCOUNT8) VALUES (11, 15, 'Archive', 10, 1)",
            [],
        )
        .unwrap();

        // notes
        conn.execute(
            "INSERT INTO ZICCLOUDSYNCINGOBJECT(Z_PK, Z_ENT, ZTITLE1, ZFOLDER, ZMARKEDFORDELETION) VALUES (20, 12, 'A', 10, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZICCLOUDSYNCINGOBJECT(Z_PK, Z_ENT, ZTITLE1, ZFOLDER, ZMARKEDFORDELETION) VALUES (21, 12, 'B', 11, 0)",
            [],
        )
        .unwrap();

        let db = NotesDb::open(db_path).unwrap();
        let accounts = db.list_accounts().unwrap();
        assert_eq!(
            accounts,
            vec![Account {
                name: "iCloud".into()
            }]
        );

        let folders = db.list_folders("iCloud").unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].path_string(), "Personal");
        assert_eq!(folders[1].path_string(), "Personal > Archive");

        let notes = db.list_notes("iCloud").unwrap();
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n.title == "A"));
        assert!(notes.iter().any(|n| n.title == "B"));

        let notes_in = db
            .list_notes_in_folder("iCloud", &["Personal".into(), "Archive".into()])
            .unwrap();
        assert_eq!(notes_in.len(), 1);
        assert_eq!(notes_in[0].title, "B");
        let pk = parse_coredata_pk(&notes_in[0].id).unwrap();
        assert_eq!(pk, 21);
    }

    #[test]
    fn parse_coredata_pk_parses() {
        assert_eq!(
            parse_coredata_pk("x-coredata://UUID/ICNote/p123").unwrap(),
            123
        );
    }
}
