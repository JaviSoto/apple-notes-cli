use crate::model::{BackupNoteMetadata, Folder, NoteSummary};
use crate::progress;
use crate::render;
use crate::transport::NotesBackend;
use anyhow::{Context, anyhow};
use crossbeam_channel as channel;
use flate2::read::GzDecoder;
use rusqlite::OptionalExtension;
use sanitize_filename::sanitize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct FolderIndex {
    by_id: HashMap<String, Folder>,
}

impl FolderIndex {
    pub fn new(folders: &[Folder]) -> anyhow::Result<Self> {
        let mut by_id = HashMap::new();
        for f in folders {
            if by_id.insert(f.id.clone(), f.clone()).is_some() {
                return Err(anyhow!("duplicate folder id: {}", f.id));
            }
        }
        Ok(Self { by_id })
    }

    pub fn folder_path(&self, folder_id: &str) -> Option<Vec<String>> {
        self.by_id.get(folder_id).map(|f| f.path.clone())
    }

    pub fn folder_path_string(&self, folder_id: &str) -> Option<String> {
        self.by_id.get(folder_id).map(|f| f.path_string())
    }
}

pub fn export_all(
    backend: &dyn NotesBackend,
    account: &str,
    out_dir: String,
    jobs: usize,
    include_html: bool,
) -> anyhow::Result<()> {
    if jobs == 0 {
        return Err(anyhow!("--jobs must be >= 1"));
    }
    let jobs = jobs.min(16);

    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir).with_context(|| format!("create {out_dir:?}"))?;

    let spinner = progress::spinner("Loading folders…");
    let folders = backend.list_folders(account)?;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
    let folder_index = FolderIndex::new(&folders)?;

    let spinner = progress::spinner("Indexing notes…");
    let notes = backend.list_notes(account)?;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let total = notes.len() as u64;
    let pb = progress::bar(total, "Exporting notes…");

    // Note content is still sourced from Notes via Apple Events (`osascript`).
    // We intentionally serialize `get_note` calls, and only parallelize render+IO.
    let exported = if jobs == 1 {
        let mut exported = 0u64;
        let mut started = 0u64;
        for n in notes {
            started += 1;
            if let Some(pb) = &pb {
                pb.set_message(format!(
                    "Fetching {}/{}: {}",
                    started,
                    total,
                    truncate_title(&n.title)
                ));
            }
            let item = build_item(
                backend,
                account,
                &out_dir,
                &folder_index,
                n,
                pb.as_ref(),
                include_html,
            )?;
            write_item(&item)?;
            if let Some(pb) = &pb {
                pb.inc(1);
            }
            exported += 1;
        }
        exported
    } else {
        let (work_tx, work_rx) = channel::bounded::<WorkItem>(jobs * 2);
        let (done_tx, done_rx) = channel::unbounded::<anyhow::Result<()>>();
        let stop = AtomicBool::new(false);

        std::thread::scope(|scope| -> anyhow::Result<u64> {
            for _ in 0..jobs {
                let work_rx = work_rx.clone();
                let done_tx = done_tx.clone();
                let stop = &stop;
                scope.spawn(move || {
                    while let Ok(item) = work_rx.recv() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let res = write_item(&item);
                        if res.is_err() {
                            stop.store(true, Ordering::Relaxed);
                        }
                        let _ = done_tx.send(res);
                    }
                });
            }

            drop(done_tx);
            drop(work_rx);

            let mut sent = 0u64;
            for n in notes {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(pb) = &pb {
                    pb.set_message(format!(
                        "Fetching {}/{}: {}",
                        sent + 1,
                        total,
                        truncate_title(&n.title)
                    ));
                }
                let item = build_item(
                    backend,
                    account,
                    &out_dir,
                    &folder_index,
                    n,
                    pb.as_ref(),
                    include_html,
                )?;
                work_tx.send(item).ok();
                sent += 1;
            }
            drop(work_tx);

            let mut completed = 0u64;
            while completed < sent {
                let res = done_rx.recv().context("worker hung up")?;
                res?;
                completed += 1;
                if let Some(pb) = &pb {
                    pb.inc(1);
                }
            }
            Ok(completed)
        })?
    };

    if let Some(pb) = pb {
        pb.finish_with_message(format!(
            "Exported {}/{} notes to {}",
            exported,
            total,
            out_dir.display()
        ));
    }

    Ok(())
}

fn truncate_title(title: &str) -> String {
    let t = title.trim();
    let max = 60usize;
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out = String::new();
    for (i, c) in t.chars().enumerate() {
        if i >= max {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

fn export_path(
    root: &Path,
    folder_path: &[String],
    title: &str,
    note_id: &str,
) -> anyhow::Result<PathBuf> {
    let mut dir = root.to_path_buf();
    for part in folder_path {
        dir.push(sanitize(part));
    }
    let note_dir = note_dir_name(title, note_id);
    Ok(dir.join(note_dir))
}

#[derive(Debug, Clone)]
struct WorkItem {
    note_dir: PathBuf,
    metadata_json: String,
    contents_md: String,
    contents_html: Option<String>,
}

fn build_item(
    backend: &dyn NotesBackend,
    account: &str,
    out_dir: &Path,
    folder_index: &FolderIndex,
    n: NoteSummary,
    _pb: Option<&indicatif::ProgressBar>,
    include_html: bool,
) -> anyhow::Result<WorkItem> {
    let note = backend.get_note(&n.id)?;
    let folder_path = folder_index.folder_path(&note.folder_id).ok_or_else(|| {
        anyhow!(
            "note {} references unknown folder id {}",
            note.id,
            note.folder_id
        )
    })?;

    let contents_md = render::note_to_markdown(&note);
    let contents_html = if include_html {
        Some(note.body_html.clone())
    } else {
        None
    };
    let metadata = BackupNoteMetadata {
        id: note.id.clone(),
        title: note.title.clone(),
        account: account.to_string(),
        folder_path: folder_path.clone(),
        created_at: note.created_at,
        modified_at: note.modified_at,
    };

    let note_dir = export_path(out_dir, &folder_path, &note.title, &note.id)?;
    let metadata_json = serde_json::to_string_pretty(&metadata)?;
    Ok(WorkItem {
        note_dir,
        metadata_json,
        contents_md,
        contents_html,
    })
}

fn write_item(item: &WorkItem) -> anyhow::Result<()> {
    std::fs::create_dir_all(&item.note_dir)
        .with_context(|| format!("create {:?}", item.note_dir))?;

    let meta_path = item.note_dir.join("metadata.json");
    std::fs::write(&meta_path, &item.metadata_json)
        .with_context(|| format!("write {meta_path:?}"))?;

    let contents_path = item.note_dir.join("contents.md");
    std::fs::write(&contents_path, &item.contents_md)
        .with_context(|| format!("write {contents_path:?}"))?;

    if let Some(html) = &item.contents_html {
        let html_path = item.note_dir.join("contents.html");
        std::fs::write(&html_path, html).with_context(|| format!("write {html_path:?}"))?;
    }

    Ok(())
}

fn note_dir_name(title: &str, note_id: &str) -> String {
    let mut base = title.trim().to_string();
    if base.is_empty() {
        base = "Untitled".to_string();
    }
    if base.len() > 80 {
        base.truncate(80);
    }
    let base = sanitize(&base);
    let short_id = note_id.rsplit('/').next().unwrap_or(note_id);
    format!("{base}-{short_id}")
}

pub fn export_all_db(
    account: &str,
    out_dir: String,
    jobs: usize,
    include_html: bool,
) -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(anyhow!("db export is supported on macOS only"));
    }
    if jobs == 0 {
        return Err(anyhow!("--jobs must be >= 1"));
    }
    let jobs = jobs.min(16);

    let db = crate::db::NotesDb::open_default()?;
    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir).with_context(|| format!("create {out_dir:?}"))?;

    let spinner = progress::spinner("Loading folders…");
    let folders = db.list_folders(account)?;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
    let folder_index = FolderIndex::new(&folders)?;

    let spinner = progress::spinner("Indexing notes…");
    let note_rows = list_db_notes(account, include_html)?;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let total = note_rows.len() as u64;
    let pb = progress::bar(total, "Exporting notes…");

    let (task_tx, task_rx) = channel::bounded::<DbNoteRow>(jobs * 2);
    let (done_tx, done_rx) = channel::unbounded::<anyhow::Result<()>>();
    let stop = AtomicBool::new(false);

    let exported = std::thread::scope(|scope| -> anyhow::Result<u64> {
        for _ in 0..jobs {
            let task_rx = task_rx.clone();
            let done_tx = done_tx.clone();
            let folder_index = &folder_index;
            let out_dir = &out_dir;
            let account = account.to_string();
            let pb = pb.clone();
            let stop = &stop;

            scope.spawn(move || {
                let conn = match open_notes_db_readonly() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = done_tx.send(Err(e));
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                };
                while let Ok(row) = task_rx.recv() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let res =
                        export_one_db(&account, out_dir, folder_index, &row, &conn, pb.as_ref());
                    if res.is_err() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    let _ = done_tx.send(res);
                }
            });
        }

        drop(done_tx);
        drop(task_rx);

        let mut queued = 0u64;
        for row in note_rows {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            queued += 1;
            if let Some(pb) = &pb {
                pb.set_message(format!(
                    "Queued {}/{}: {}",
                    queued,
                    total,
                    truncate_title(&row.title)
                ));
            }
            if task_tx.send(row).is_err() {
                break;
            }
        }
        drop(task_tx);

        let mut completed = 0u64;
        while let Ok(res) = done_rx.recv() {
            res?;
            completed += 1;
            if let Some(pb) = &pb {
                pb.inc(1);
            }
            if completed >= total || stop.load(Ordering::Relaxed) {
                break;
            }
        }

        Ok(completed)
    })?;

    if let Some(pb) = pb {
        pb.finish_with_message(format!(
            "Exported {}/{} notes to {}",
            exported,
            total,
            out_dir.display()
        ));
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct DbNoteRow {
    id: String,
    title: String,
    folder_id: String,
    created_at: OffsetDateTime,
    modified_at: OffsetDateTime,
    body_html: Option<String>,
}

fn list_db_notes(account: &str, include_html: bool) -> anyhow::Result<Vec<DbNoteRow>> {
    let db = crate::db::NotesDb::open_default()?;
    let notes = db.list_notes(account)?;

    // Hydrate dates from DB. Best-effort; schema-specific.
    let store_uuid = db_store_uuid()?;
    let conn = open_notes_db_readonly()?;

    let mut out = Vec::new();
    for n in notes {
        let pk = parse_coredata_pk(&n.id)?;
        let (created, modified) = select_note_dates(&conn, pk)?;
        out.push(DbNoteRow {
            id: format!("x-coredata://{}/ICNote/p{}", store_uuid, pk),
            title: n.title,
            folder_id: n.folder_id,
            created_at: created,
            modified_at: modified,
            body_html: None,
        });
    }
    if include_html {
        // Fetch the raw HTML via Apple Events (Notes.app). This is slower, but preserves exact styling.
        let osascript = crate::transport::OsascriptBackend;
        for row in &mut out {
            let note = osascript.get_note(&row.id)?;
            row.body_html = Some(note.body_html);
        }
    }

    Ok(out)
}

fn export_one_db(
    account: &str,
    out_dir: &Path,
    folder_index: &FolderIndex,
    row: &DbNoteRow,
    conn: &rusqlite::Connection,
    pb: Option<&indicatif::ProgressBar>,
) -> anyhow::Result<()> {
    if let Some(pb) = pb {
        pb.set_message(format!("Decoding: {}", truncate_title(&row.title)));
    }
    let pk = parse_coredata_pk(&row.id)?;
    let data = load_note_data(conn, pk)?;
    let contents_md = decode_note_markdown(&data).unwrap_or_else(|_| String::new());
    let contents_html = row.body_html.clone();

    let folder_path = folder_index
        .folder_path(&row.folder_id)
        .unwrap_or_else(|| vec!["Unknown".to_string()]);

    let metadata = BackupNoteMetadata {
        id: row.id.clone(),
        title: row.title.clone(),
        account: account.to_string(),
        folder_path: folder_path.clone(),
        created_at: row.created_at,
        modified_at: row.modified_at,
    };

    let note_dir = export_path(out_dir, &folder_path, &row.title, &row.id)?;
    let metadata_json = serde_json::to_string_pretty(&metadata)?;

    write_item(&WorkItem {
        note_dir,
        metadata_json,
        contents_md,
        contents_html,
    })
}

fn open_notes_db_readonly() -> anyhow::Result<rusqlite::Connection> {
    let db_path = std::path::PathBuf::from(std::env::var("HOME").context("HOME not set")?)
        .join("Library/Group Containers/group.com.apple.notes/NoteStore.sqlite");

    rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_SHARED_CACHE,
    )
    .context("open Notes DB")
}

fn db_store_uuid() -> anyhow::Result<String> {
    let conn = open_notes_db_readonly()?;
    conn.query_row(
        "SELECT Z_UUID FROM Z_METADATA WHERE Z_VERSION = 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .context("read Z_METADATA.Z_UUID")
}

fn parse_coredata_pk(coredata_id: &str) -> anyhow::Result<i64> {
    let Some(last) = coredata_id.rsplit('/').next() else {
        return Err(anyhow!("invalid coredata id: {coredata_id}"));
    };
    let Some(pk) = last.strip_prefix('p') else {
        return Err(anyhow!("invalid coredata id: {coredata_id}"));
    };
    pk.parse::<i64>()
        .with_context(|| format!("invalid coredata pk in id: {coredata_id}"))
}

fn select_note_dates(
    conn: &rusqlite::Connection,
    note_pk: i64,
) -> anyhow::Result<(OffsetDateTime, OffsetDateTime)> {
    // Apple Notes uses an Apple epoch (seconds since 2001-01-01). Best effort.
    struct Raw {
        c1: Option<f64>,
        c2: Option<f64>,
        c3: Option<f64>,
        m1: Option<f64>,
        m2: Option<f64>,
    }

    let raw: Raw = conn
        .query_row(
            "SELECT ZCREATIONDATE1, ZCREATIONDATE2, ZCREATIONDATE3, ZMODIFICATIONDATE1, ZMODIFICATIONDATEATIMPORT FROM ZICCLOUDSYNCINGOBJECT WHERE Z_ENT = 12 AND Z_PK = ?",
            [note_pk],
            |row| {
                Ok(Raw {
                    c1: row.get(0)?,
                    c2: row.get(1)?,
                    c3: row.get(2)?,
                    m1: row.get(3)?,
                    m2: row.get(4)?,
                })
            },
        )
        .with_context(|| format!("read note dates for pk {note_pk}"))?;

    let created = raw.c3.or(raw.c2).or(raw.c1).unwrap_or(0.0);
    let modified = raw.m1.or(raw.m2).unwrap_or(created);
    Ok((apple_epoch_seconds(created), apple_epoch_seconds(modified)))
}

fn apple_epoch_seconds(secs: f64) -> OffsetDateTime {
    let base = OffsetDateTime::from_unix_timestamp(978307200).unwrap(); // 2001-01-01T00:00:00Z
    base + time::Duration::milliseconds((secs * 1000.0) as i64)
}

fn load_note_data(conn: &rusqlite::Connection, note_pk: i64) -> anyhow::Result<Vec<u8>> {
    let data: Option<Vec<u8>> = conn
        .query_row(
            "SELECT ZDATA FROM ZICNOTEDATA WHERE ZNOTE = ? LIMIT 1",
            [note_pk],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .with_context(|| format!("read ZICNOTEDATA.ZDATA for note pk {note_pk}"))?;

    Ok(data.unwrap_or_default())
}

fn decode_note_markdown(data: &[u8]) -> anyhow::Result<String> {
    let decoded = if data.starts_with(&[0x1f, 0x8b]) {
        gunzip(data).context("gunzip note blob")?
    } else {
        data.to_vec()
    };

    if let Ok(s) = std::str::from_utf8(&decoded) {
        let s = s.trim_matches('\0').trim();
        if looks_like_human_text(s) {
            return Ok(normalize_text(s));
        }
    }

    let text = best_effort_extract_text(&decoded);
    if text.trim().is_empty() {
        return Err(anyhow!("could not extract text from note blob"));
    }
    Ok(text)
}

fn gunzip(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut dec = GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).context("read gzip")?;
    Ok(out)
}

fn looks_like_human_text(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut printable = 0usize;
    let mut weird = 0usize;
    for c in s.chars().take(2048) {
        if c.is_control() && c != '\n' && c != '\r' && c != '\t' {
            weird += 1;
        } else {
            printable += 1;
        }
    }
    printable > 0 && weird * 20 < printable
}

fn normalize_text(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn best_effort_extract_text(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);

    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in s.chars() {
        if (ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t') || ch == '\u{FFFD}' {
            if !current.trim().is_empty() {
                blocks.push(current.trim().to_string());
            }
            current.clear();
            continue;
        }
        current.push(ch);
    }
    if !current.trim().is_empty() {
        blocks.push(current.trim().to_string());
    }

    blocks.sort_by_key(|b| std::cmp::Reverse(score_block(b)));
    let best = blocks
        .into_iter()
        .find(|b| score_block(b) > 20)
        .unwrap_or_default();
    normalize_text(&best)
}

fn score_block(s: &str) -> usize {
    let alnum = s.chars().filter(|c| c.is_alphanumeric()).count();
    let ws = s.chars().filter(|c| c.is_whitespace()).count();
    let len = s.chars().count();
    let dense = alnum.saturating_sub(len / 4);
    dense + ws.min(200)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    #[test]
    fn export_path_uses_folder_structure_and_safe_filename() {
        let root = Path::new("/tmp/out");
        let p = export_path(
            root,
            &["Personal".into(), "Archive".into()],
            "Hello/World",
            "x-coredata://abc/ICNote/p123",
        )
        .unwrap();
        assert!(p.to_string_lossy().contains("Personal"));
        assert!(p.to_string_lossy().contains("Archive"));
        assert!(p.to_string_lossy().contains("HelloWorld-p123"));
    }

    #[test]
    fn decode_note_markdown_extracts_text_from_gzip_blob() {
        let payload = b"\0\0Title\0\0Hello from Notes!\nSecond line.\0\0";
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(payload).unwrap();
        let gz = enc.finish().unwrap();

        let out = decode_note_markdown(&gz).unwrap();
        assert!(out.contains("Hello from Notes!"));
        assert!(out.contains("Second line."));
    }

    #[test]
    fn decode_note_markdown_accepts_plain_utf8() {
        let out = decode_note_markdown(b"Hi\r\nThere").unwrap();
        assert_eq!(out, "Hi\nThere");
    }

    #[test]
    fn truncate_title_shortens() {
        let long = "a".repeat(200);
        let t = truncate_title(&long);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 61);
    }

    #[test]
    fn note_dir_name_includes_short_id_and_sanitizes() {
        let name = note_dir_name("Hello/World", "x-coredata://UUID/ICNote/p123");
        assert!(name.contains("HelloWorld"));
        assert!(name.ends_with("p123"));
    }
}
