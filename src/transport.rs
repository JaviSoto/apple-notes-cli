use crate::model::{Account, Folder, Note, NoteSummary};
use crate::{cli, db};
use anyhow::{Context, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn osascript_bin() -> OsString {
    std::env::var_os("APPLE_NOTES_OSASCRIPT_BIN").unwrap_or_else(|| OsString::from("osascript"))
}

pub trait NotesBackend: Send + Sync {
    fn list_accounts(&self) -> anyhow::Result<Vec<Account>>;
    fn list_folders(&self, account: &str) -> anyhow::Result<Vec<Folder>>;
    fn list_notes(&self, account: &str) -> anyhow::Result<Vec<NoteSummary>>;
    fn list_notes_in_folder(
        &self,
        account: &str,
        folder_path: &[String],
    ) -> anyhow::Result<Vec<NoteSummary>>;

    /// Streams note summaries, invoking `on_note` for every note found.
    ///
    /// This exists primarily to support better UX (progress counters) when `osascript` is slow.
    fn stream_note_summaries(
        &self,
        account: &str,
        folder_path: Option<&[String]>,
        on_note: &mut dyn FnMut(NoteSummary),
    ) -> anyhow::Result<()>;

    fn get_note(&self, id: &str) -> anyhow::Result<Note>;

    fn create_note_html(
        &self,
        account: &str,
        folder_path: &[String],
        title: &str,
        body_html: &str,
    ) -> anyhow::Result<String>;

    fn set_note_title(&self, id: &str, title: &str) -> anyhow::Result<()>;
    fn set_note_body_html(&self, id: &str, body_html: &str) -> anyhow::Result<()>;
    fn append_note_body_html(&self, id: &str, body_html: &str) -> anyhow::Result<()>;
    fn delete_note(&self, id: &str) -> anyhow::Result<()>;

    fn move_note(&self, id: &str, account: &str, folder_path: &[String]) -> anyhow::Result<()>;

    fn create_folder(
        &self,
        account: &str,
        parent_path: &[String],
        name: &str,
    ) -> anyhow::Result<String>;
    fn rename_folder(
        &self,
        account: &str,
        folder_path: &[String],
        name: &str,
    ) -> anyhow::Result<()>;
    fn delete_folder(&self, account: &str, folder_path: &[String]) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Default)]
pub struct OsascriptBackend;

#[derive(Debug)]
pub struct HybridBackend {
    db: db::NotesDb,
    osascript: OsascriptBackend,
}

impl HybridBackend {
    pub fn new(db: db::NotesDb) -> Self {
        Self {
            db,
            osascript: OsascriptBackend,
        }
    }
}

impl OsascriptBackend {
    fn run_osascript_jxa(&self, script: &str) -> anyhow::Result<String> {
        self.run_osascript(&["-l", "JavaScript", "-"], script)
    }

    fn run_osascript_applescript(&self, script: &str) -> anyhow::Result<String> {
        self.run_osascript(&["-"], script)
    }

    fn run_osascript(&self, osascript_args: &[&str], stdin: &str) -> anyhow::Result<String> {
        if std::env::var_os("APPLE_NOTES_DEBUG_SCRIPT").is_some() {
            eprintln!(
                "DEBUG apple-notes: running osascript {:?} with stdin:\n{}\n---",
                osascript_args, stdin
            );
        }

        let mut cmd = Command::new(osascript_bin());
        cmd.args(osascript_args);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn osascript (are you on macOS?)")?;

        {
            let mut stdin_pipe = child.stdin.take().context("stdin was not piped")?;
            stdin_pipe
                .write_all(stdin.as_bytes())
                .context("failed writing osascript stdin")?;
        }

        let out = child.wait_with_output().context("osascript failed")?;
        if !out.status.success() {
            return Err(anyhow!(
                "osascript failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }

        // In some environments, osascript emits output on stderr even on success.
        if out.stdout.is_empty() && !out.stderr.is_empty() {
            Ok(String::from_utf8_lossy(&out.stderr).to_string())
        } else {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        }
    }

    fn run_osascript_streaming(
        &self,
        osascript_args: &[&str],
        stdin: &str,
        mut on_stderr_line: impl FnMut(&str),
    ) -> anyhow::Result<()> {
        if std::env::var_os("APPLE_NOTES_DEBUG_SCRIPT").is_some() {
            eprintln!(
                "DEBUG apple-notes: streaming osascript {:?} with stdin:\n{}\n---",
                osascript_args, stdin
            );
        }

        let mut cmd = Command::new(osascript_bin());
        cmd.args(osascript_args);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn osascript (are you on macOS?)")?;

        {
            let mut stdin_pipe = child.stdin.take().context("stdin was not piped")?;
            stdin_pipe
                .write_all(stdin.as_bytes())
                .context("failed writing osascript stdin")?;
        }

        let mut stdout = child.stdout.take().context("stdout was not piped")?;
        let stdout_thread = std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            let _ = stdout.read_to_string(&mut s);
            s
        });

        let mut stderr_buf = String::new();
        {
            let stderr = child.stderr.take().context("stderr was not piped")?;
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader
                .read_line(&mut line)
                .context("read osascript stderr")?
                > 0
            {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                stderr_buf.push_str(trimmed);
                stderr_buf.push('\n');
                on_stderr_line(trimmed);
                line.clear();
            }
        }

        let status = child.wait().context("osascript failed")?;
        let stdout_buf = stdout_thread.join().unwrap_or_default();
        if !status.success() {
            return Err(anyhow!(
                "osascript failed ({}): {}{}",
                status,
                if stderr_buf.trim().is_empty() {
                    String::new()
                } else {
                    stderr_buf
                },
                if stdout_buf.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n{}", stdout_buf)
                }
            ));
        }

        Ok(())
    }

    fn jxa_json<T: DeserializeOwned>(&self, script: &str) -> anyhow::Result<T> {
        let out = self
            .run_osascript_jxa(script)
            .context("osascript (JXA) failed")?;
        let out = out.trim();
        serde_json::from_str(out)
            .with_context(|| format!("failed to parse osascript JSON output: {out}"))
    }

    fn build_jxa(action: &str, payload: &impl Serialize) -> anyhow::Result<String> {
        let payload_json = serde_json::to_string(payload)?;
        Ok(format!(
            r#"
const Notes = Application("Notes");
Notes.includeStandardAdditions = true;

const input = {payload_json};

function folderPathFor(folder, accountId) {{
  const parts = [folder.name()];
  const seen = {{}};
  let current = folder;
  while (true) {{
    const c = current.container();
    if (!c) break;
    let cid = null;
    try {{ cid = c.id(); }} catch (e) {{ break; }}
    if (cid === accountId) break;
    if (seen[cid]) break;
    seen[cid] = true;
    parts.push(c.name());
    current = c;
  }}
  parts.reverse();
  return parts;
}}

function listFolders(accountName) {{
  const acct = Notes.accounts().find(a => a.name() === accountName);
  if (!acct) throw new Error("account not found: " + accountName);
  const accountId = acct.id();
  const byId = {{}};
  acct.folders().forEach(f => {{
    const id = f.id();
    const path = folderPathFor(f, accountId);
    const existing = byId[id];
    if (!existing || path.length < existing.path.length) {{
      byId[id] = {{
        id: id,
        name: f.name(),
        account: accountName,
        path: path,
      }};
    }}
  }});
  return Object.values(byId);
}}

function resolveFolderIds(accountName, wantParts) {{
  const acct = Notes.accounts().find(a => a.name() === accountName);
  if (!acct) throw new Error("account not found: " + accountName);
  const accountId = acct.id();
  const want = wantParts.join(" > ");
  const last = wantParts[wantParts.length - 1];
  const candidates = acct.folders().filter(f => f.name() === last);
  const matches = candidates
    .filter(f => folderPathFor(f, accountId).join(" > ") === want)
    .map(f => f.id());
  return matches;
}}

function main() {{
  switch ({action:?}) {{
    case "accounts.list": {{
      return Notes.accounts().map(a => ({{ name: a.name() }}));
    }}
    case "folders.list": {{
      return listFolders(input.account);
    }}
    case "folders.resolve": {{
      return {{ matches: resolveFolderIds(input.account, input.path) }};
    }}
    case "notes.get": {{
      const n = Notes.notes.byId(input.id);
      return {{
        id: n.id(),
        title: n.name(),
        folder_id: n.container().id(),
        created_at: n.creationDate().toISOString(),
        modified_at: n.modificationDate().toISOString(),
        body_html: String(n.body()),
      }};
    }}
    default:
      throw new Error("unknown action: " + {action:?});
  }}
}}

console.log(JSON.stringify(main()));
"#
        ))
    }

    fn resolve_folder_id(&self, account: &str, folder_path: &[String]) -> anyhow::Result<String> {
        #[derive(Serialize)]
        struct Payload<'a> {
            account: &'a str,
            path: &'a [String],
        }
        #[derive(serde::Deserialize)]
        struct Out {
            matches: Vec<String>,
        }

        let script = Self::build_jxa(
            "folders.resolve",
            &Payload {
                account,
                path: folder_path,
            },
        )?;
        let out: Out = self.jxa_json(&script)?;
        match out.matches.len() {
            0 => Err(anyhow!("folder not found: {}", folder_path.join(" > "))),
            1 => Ok(out.matches[0].clone()),
            n => Err(anyhow!(
                "folder path is ambiguous ({} matches): {}",
                n,
                folder_path.join(" > ")
            )),
        }
    }

    fn extract_osascript_log_payload(line: &str) -> &str {
        // In some environments, `osascript` prefixes log output. Be permissive.
        if let Some(idx) = line.find("log:") {
            return line[idx + "log:".len()..].trim();
        }
        line.trim()
    }

    fn stream_note_summaries_applescript(
        &self,
        script: &str,
        on_note: &mut dyn FnMut(NoteSummary),
    ) -> anyhow::Result<()> {
        let mut seen_ids: HashSet<String> = HashSet::new();
        self.run_osascript_streaming(&["-"], script, |line| {
            let payload = Self::extract_osascript_log_payload(line);
            if payload.is_empty() || !payload.contains('\t') {
                return;
            }
            if let Ok(mut parsed) = parse_note_summaries_tsv(payload)
                && let Some(first) = parsed.pop()
                && seen_ids.insert(first.id.clone())
            {
                on_note(first);
            }
        })
    }
}

impl NotesBackend for HybridBackend {
    fn list_accounts(&self) -> anyhow::Result<Vec<Account>> {
        self.db.list_accounts()
    }

    fn list_folders(&self, account: &str) -> anyhow::Result<Vec<Folder>> {
        self.db.list_folders(account)
    }

    fn list_notes(&self, account: &str) -> anyhow::Result<Vec<NoteSummary>> {
        self.db.list_notes(account)
    }

    fn list_notes_in_folder(
        &self,
        account: &str,
        folder_path: &[String],
    ) -> anyhow::Result<Vec<NoteSummary>> {
        self.db.list_notes_in_folder(account, folder_path)
    }

    fn stream_note_summaries(
        &self,
        account: &str,
        folder_path: Option<&[String]>,
        on_note: &mut dyn FnMut(NoteSummary),
    ) -> anyhow::Result<()> {
        let notes = if let Some(folder_path) = folder_path {
            self.list_notes_in_folder(account, folder_path)?
        } else {
            self.list_notes(account)?
        };
        for n in notes {
            on_note(n);
        }
        Ok(())
    }

    fn get_note(&self, id: &str) -> anyhow::Result<Note> {
        self.osascript.get_note(id)
    }

    fn create_note_html(
        &self,
        account: &str,
        folder_path: &[String],
        title: &str,
        body_html: &str,
    ) -> anyhow::Result<String> {
        self.osascript
            .create_note_html(account, folder_path, title, body_html)
    }

    fn set_note_title(&self, id: &str, title: &str) -> anyhow::Result<()> {
        self.osascript.set_note_title(id, title)
    }

    fn set_note_body_html(&self, id: &str, body_html: &str) -> anyhow::Result<()> {
        self.osascript.set_note_body_html(id, body_html)
    }

    fn append_note_body_html(&self, id: &str, body_html: &str) -> anyhow::Result<()> {
        self.osascript.append_note_body_html(id, body_html)
    }

    fn delete_note(&self, id: &str) -> anyhow::Result<()> {
        self.osascript.delete_note(id)
    }

    fn move_note(&self, id: &str, account: &str, folder_path: &[String]) -> anyhow::Result<()> {
        self.osascript.move_note(id, account, folder_path)
    }

    fn create_folder(
        &self,
        account: &str,
        parent_path: &[String],
        name: &str,
    ) -> anyhow::Result<String> {
        self.osascript.create_folder(account, parent_path, name)
    }

    fn rename_folder(
        &self,
        account: &str,
        folder_path: &[String],
        name: &str,
    ) -> anyhow::Result<()> {
        self.osascript.rename_folder(account, folder_path, name)
    }

    fn delete_folder(&self, account: &str, folder_path: &[String]) -> anyhow::Result<()> {
        self.osascript.delete_folder(account, folder_path)
    }
}

impl NotesBackend for OsascriptBackend {
    fn list_accounts(&self) -> anyhow::Result<Vec<Account>> {
        #[derive(Serialize)]
        struct Payload {}
        let script = Self::build_jxa("accounts.list", &Payload {})?;
        self.jxa_json(&script)
    }

    fn list_folders(&self, account: &str) -> anyhow::Result<Vec<Folder>> {
        #[derive(Serialize)]
        struct Payload<'a> {
            account: &'a str,
        }
        let script = Self::build_jxa("folders.list", &Payload { account })?;
        self.jxa_json(&script)
    }

    fn list_notes(&self, account: &str) -> anyhow::Result<Vec<NoteSummary>> {
        let mut out = Vec::new();
        self.stream_note_summaries(account, None, &mut |n| out.push(n))?;
        Ok(out)
    }

    fn list_notes_in_folder(
        &self,
        account: &str,
        folder_path: &[String],
    ) -> anyhow::Result<Vec<NoteSummary>> {
        let mut out = Vec::new();
        self.stream_note_summaries(account, Some(folder_path), &mut |n| out.push(n))?;
        Ok(out)
    }

    fn stream_note_summaries(
        &self,
        account: &str,
        folder_path: Option<&[String]>,
        on_note: &mut dyn FnMut(NoteSummary),
    ) -> anyhow::Result<()> {
        // AppleScript is significantly faster/reliable for listing metadata across large accounts.
        // We stream via `log` to avoid building giant return strings and to enable progress counts.
        let folder_id = if let Some(folder_path) = folder_path {
            Some(self.resolve_folder_id(account, folder_path)?)
        } else {
            None
        };

        let script = if let Some(folder_id) = folder_id {
            format!(
                r#"
on replace_chars(s, find, repl)
  set AppleScript's text item delimiters to find
  set parts to every text item of s
  set AppleScript's text item delimiters to repl
  set s2 to parts as text
  set AppleScript's text item delimiters to ""
  return s2
end replace_chars

tell application "Notes"
  set f to folder id {folder_id:?}
  set folderId to (id of f as text)
  set ns to every note of f
  repeat with n in ns
    set t to (name of n as text)
    set t to my replace_chars(t, tab, " ")
    set t to my replace_chars(t, return, " ")
    log (id of n as text) & tab & t & tab & folderId
  end repeat
  return "OK"
end tell
"#
            )
        } else {
            format!(
                r#"
on replace_chars(s, find, repl)
  set AppleScript's text item delimiters to find
  set parts to every text item of s
  set AppleScript's text item delimiters to repl
  set s2 to parts as text
  set AppleScript's text item delimiters to ""
  return s2
end replace_chars

tell application "Notes"
  tell account {account:?}
    repeat with f in folders
      set folderId to (id of f as text)
      set ns to every note of f
      repeat with n in ns
        set t to (name of n as text)
        set t to my replace_chars(t, tab, " ")
        set t to my replace_chars(t, return, " ")
        log (id of n as text) & tab & t & tab & folderId
      end repeat
    end repeat
    return "OK"
  end tell
end tell
"#
            )
        };

        self.stream_note_summaries_applescript(&script, on_note)
    }

    fn get_note(&self, id: &str) -> anyhow::Result<Note> {
        #[derive(Serialize)]
        struct Payload<'a> {
            id: &'a str,
        }
        let script = Self::build_jxa("notes.get", &Payload { id })?;
        self.jxa_json(&script)
    }

    fn create_note_html(
        &self,
        account: &str,
        folder_path: &[String],
        title: &str,
        body_html: &str,
    ) -> anyhow::Result<String> {
        // Use AppleScript for write operations (JXA make is unreliable on some systems).
        let folder_id = self.resolve_folder_id(account, folder_path)?;
        let script = format!(
            r#"
tell application "Notes"
  set targetFolder to folder id {folder_id:?}
  set n to make new note at targetFolder with properties {{name:{title:?}, body:{body_html:?}}}
  return id of n as text
end tell
"#
        );
        let out = self.run_osascript_applescript(&script)?;
        Ok(out.trim().to_string())
    }

    fn set_note_title(&self, id: &str, title: &str) -> anyhow::Result<()> {
        let script = format!(
            r#"
tell application "Notes"
  set n to note id {id:?}
  set name of n to {title:?}
end tell
"#
        );
        self.run_osascript_applescript(&script)?;
        Ok(())
    }

    fn set_note_body_html(&self, id: &str, body_html: &str) -> anyhow::Result<()> {
        let script = format!(
            r#"
tell application "Notes"
  set n to note id {id:?}
  set body of n to {body_html:?}
end tell
"#
        );
        self.run_osascript_applescript(&script)?;
        Ok(())
    }

    fn append_note_body_html(&self, id: &str, body_html: &str) -> anyhow::Result<()> {
        let script = format!(
            r#"
tell application "Notes"
  set n to note id {id:?}
  set body of n to (body of n as text) & {body_html:?}
end tell
"#
        );
        self.run_osascript_applescript(&script)?;
        Ok(())
    }

    fn delete_note(&self, id: &str) -> anyhow::Result<()> {
        let script = format!(
            r#"
tell application "Notes"
  set n to note id {id:?}
  delete n
end tell
"#
        );
        self.run_osascript_applescript(&script)?;
        Ok(())
    }

    fn move_note(&self, id: &str, account: &str, folder_path: &[String]) -> anyhow::Result<()> {
        let folder_id = self.resolve_folder_id(account, folder_path)?;
        let script = format!(
            r#"
tell application "Notes"
  set n to note id {id:?}
  set targetFolder to folder id {folder_id:?}
  move n to targetFolder
end tell
"#
        );
        self.run_osascript_applescript(&script)?;
        Ok(())
    }

    fn create_folder(
        &self,
        account: &str,
        parent_path: &[String],
        name: &str,
    ) -> anyhow::Result<String> {
        let parent_id = self.resolve_folder_id(account, parent_path)?;
        let script = format!(
            r#"
tell application "Notes"
  set parentFolder to folder id {parent_id:?}
  set f to make new folder at parentFolder with properties {{name:{name:?}}}
  return id of f as text
end tell
"#
        );
        let out = self.run_osascript_applescript(&script)?;
        Ok(out.trim().to_string())
    }

    fn rename_folder(
        &self,
        account: &str,
        folder_path: &[String],
        name: &str,
    ) -> anyhow::Result<()> {
        let folder_id = self.resolve_folder_id(account, folder_path)?;
        let script = format!(
            r#"
tell application "Notes"
  set f to folder id {folder_id:?}
  set name of f to {name:?}
end tell
"#
        );
        self.run_osascript_applescript(&script)?;
        Ok(())
    }

    fn delete_folder(&self, account: &str, folder_path: &[String]) -> anyhow::Result<()> {
        let folder_id = self.resolve_folder_id(account, folder_path)?;
        let script = format!(
            r#"
tell application "Notes"
  set f to folder id {folder_id:?}
  delete f
end tell
"#
        );
        self.run_osascript_applescript(&script)?;
        Ok(())
    }
}

pub fn make_backend(
    fixture: Option<std::path::PathBuf>,
    backend: cli::Backend,
) -> anyhow::Result<Box<dyn NotesBackend>> {
    if let Some(path) = fixture.or_else(|| std::env::var_os("APPLE_NOTES_FIXTURE").map(Into::into))
    {
        return Ok(Box::new(crate::fixture::FixtureBackend::from_path(path)?));
    }

    match backend {
        cli::Backend::Osascript => Ok(Box::new(OsascriptBackend)),
        cli::Backend::Db => Ok(Box::new(HybridBackend::new(db::NotesDb::open_default()?))),
        cli::Backend::Auto => match db::NotesDb::open_default() {
            Ok(db) => Ok(Box::new(HybridBackend::new(db))),
            Err(_) => Ok(Box::new(OsascriptBackend)),
        },
    }
}

fn parse_note_summaries_tsv(s: &str) -> anyhow::Result<Vec<NoteSummary>> {
    let mut out = Vec::new();
    for (idx, line) in s.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let id = parts
            .next()
            .ok_or_else(|| anyhow!("invalid notes TSV on line {}: missing id", idx + 1))?;
        let title = parts
            .next()
            .ok_or_else(|| anyhow!("invalid notes TSV on line {}: missing title", idx + 1))?;
        let folder_id = parts
            .next()
            .ok_or_else(|| anyhow!("invalid notes TSV on line {}: missing folder id", idx + 1))?;
        out.push(NoteSummary {
            id: id.to_string(),
            title: title.to_string(),
            folder_id: folder_id.to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn with_stub_osascript<T>(stub_mode: &str, f: impl FnOnce() -> T) -> T {
        let _guard = lock_env();
        let dir = tempdir().unwrap();
        let stub_path = dir.path().join("osascript-stub");

        let stub = r#"#!/usr/bin/env bash
set -euo pipefail
MODE="${APPLE_NOTES_STUB_MODE:-ok}"
ARGS="$*"
SCRIPT="$(cat)"

if [[ "$ARGS" == *"-l JavaScript"* ]]; then
  FLAT="$(printf '%s' "$SCRIPT" | tr '\n' ' ')"
  ACTION=""
  if [[ "$FLAT" == *'switch ("'* ]]; then
    TMP="${FLAT#*switch (\"}"
    ACTION="${TMP%%\"*}"
  elif [[ "$FLAT" == *'switch("'* ]]; then
    TMP="${FLAT#*switch(\"}"
    ACTION="${TMP%%\"*}"
  fi
  if [[ -z "$ACTION" ]]; then
    ACTION="$(printf '%s' "$FLAT" | sed -nE 's/.*unknown action: \" \\+ \"([^\"]+)\".*/\\1/p')"
  fi
  case "$ACTION" in
    accounts.list)
      echo '[{"name":"iCloud"}]'
      exit 0
      ;;
    folders.list)
      echo '[{"id":"x-coredata://UUID/ICFolder/p10","name":"Personal","account":"iCloud","path":["Personal"]},{"id":"x-coredata://UUID/ICFolder/p11","name":"Archive","account":"iCloud","path":["Personal","Archive"]}]'
      exit 0
      ;;
    folders.resolve)
      if [[ "$MODE" == "resolve_empty" ]]; then
        echo '{"matches":[]}' ; exit 0
      fi
      if [[ "$MODE" == "resolve_ambiguous" ]]; then
        echo '{"matches":["id1","id2"]}' ; exit 0
      fi
      echo '{"matches":["x-coredata://UUID/ICFolder/p10"]}'
      exit 0
      ;;
    notes.get)
      echo '{"id":"x-coredata://UUID/ICNote/p20","title":"Hello","folder_id":"x-coredata://UUID/ICFolder/p10","created_at":"2025-12-20T00:00:00Z","modified_at":"2025-12-20T01:00:00Z","body_html":"<div>Hi</div>"}'
      exit 0
      ;;
  esac

  echo "unknown JXA stub action" >&2
  exit 1
fi

# AppleScript streaming path (stderr logs)
printf 'log: id1\ttitle1\tfolder1\n' >&2
printf 'log: id1\ttitle1\tfolder1\n' >&2
printf 'log: id2\ttitle2\tfolder2\n' >&2
exit 0
"#;

        std::fs::write(&stub_path, stub).unwrap();
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();

        let old_bin = env::var_os("APPLE_NOTES_OSASCRIPT_BIN");
        let old_mode = env::var_os("APPLE_NOTES_STUB_MODE");
        // Safety: environment variables are process-global; we serialize these tests with ENV_LOCK.
        unsafe {
            env::set_var("APPLE_NOTES_OSASCRIPT_BIN", &stub_path);
            env::set_var("APPLE_NOTES_STUB_MODE", stub_mode);
        }

        let res = f();

        match old_bin {
            Some(v) => unsafe { env::set_var("APPLE_NOTES_OSASCRIPT_BIN", v) },
            None => unsafe { env::remove_var("APPLE_NOTES_OSASCRIPT_BIN") },
        }
        match old_mode {
            Some(v) => unsafe { env::set_var("APPLE_NOTES_STUB_MODE", v) },
            None => unsafe { env::remove_var("APPLE_NOTES_STUB_MODE") },
        }
        res
    }

    #[test]
    fn parse_note_summaries_tsv_parses_lines() {
        let parsed =
            parse_note_summaries_tsv("id1\ttitle1\tfolder1\nid2\ttitle2\tfolder2\n").unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "id1");
        assert_eq!(parsed[1].folder_id, "folder2");
    }

    #[test]
    fn extract_osascript_log_payload_strips_prefix() {
        assert_eq!(
            OsascriptBackend::extract_osascript_log_payload("log: a\tb\tc"),
            "a\tb\tc"
        );
        assert_eq!(
            OsascriptBackend::extract_osascript_log_payload("a\tb\tc"),
            "a\tb\tc"
        );
    }

    #[test]
    fn build_jxa_includes_action_literal() {
        #[derive(Serialize)]
        struct Payload {}
        let s = OsascriptBackend::build_jxa("accounts.list", &Payload {}).unwrap();
        assert!(
            s.contains("switch (\"accounts.list\")") || s.contains("switch(\"accounts.list\")"),
            "script missing expected switch(action) literal"
        );
        assert!(s.contains("unknown action"));
    }

    #[test]
    fn osascript_backend_list_accounts_works_with_stub() {
        with_stub_osascript("ok", || {
            let b = OsascriptBackend;
            let accounts = b.list_accounts().unwrap();
            assert_eq!(accounts.len(), 1);
            assert_eq!(accounts[0].name, "iCloud");
        });
    }

    #[test]
    fn osascript_backend_get_note_works_with_stub() {
        with_stub_osascript("ok", || {
            let b = OsascriptBackend;
            let note = b.get_note("x-coredata://UUID/ICNote/p20").unwrap();
            assert_eq!(note.title, "Hello");
            assert!(note.body_html.contains("Hi"));
        });
    }

    #[test]
    fn osascript_backend_stream_note_summaries_dedups() {
        with_stub_osascript("ok", || {
            let b = OsascriptBackend;
            let mut out = Vec::new();
            b.stream_note_summaries("iCloud", None, &mut |n| out.push(n))
                .unwrap();
            assert_eq!(out.len(), 2);
            assert_eq!(out[0].id, "id1");
            assert_eq!(out[1].id, "id2");
        });
    }

    #[test]
    fn resolve_folder_id_errors_on_no_matches() {
        with_stub_osascript("resolve_empty", || {
            let b = OsascriptBackend;
            let err = b
                .resolve_folder_id("iCloud", &["Personal".into()])
                .unwrap_err();
            assert!(err.to_string().contains("folder not found"));
        });
    }

    #[test]
    fn resolve_folder_id_errors_on_multiple_matches() {
        with_stub_osascript("resolve_ambiguous", || {
            let b = OsascriptBackend;
            let err = b
                .resolve_folder_id("iCloud", &["Personal".into()])
                .unwrap_err();
            assert!(err.to_string().contains("ambiguous"));
        });
    }
}
