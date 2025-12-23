use crate::backup;
use crate::model::{Folder, NoteSummary};
use crate::progress;
use crate::render;
use crate::tables;
use crate::transport::NotesBackend;
use anyhow::{Context, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::Cell;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "apple-notes",
    about = "A fast, scriptable CLI for Apple Notes (read/write + backups).",
    version,
    disable_help_subcommand = false,
    arg_required_else_help = true,
    after_help = r#"Examples:
  apple-notes folders list
  apple-notes notes list --folder "Personal > Archive"
  apple-notes notes show x-coredata://... --markdown
  apple-notes notes create --folder "Personal > Archive" --title "Hello" --body "Hi!"
  apple-notes export --out ./notes-backup

First run on macOS may prompt for Automation permission (osascript → Notes).
"#
)]
pub struct Args {
    /// Notes account to target (default: iCloud).
    #[arg(long, default_value = "iCloud", global = true)]
    pub account: String,

    /// Backend for reads (writes always use `osascript`).
    #[arg(long, default_value = "auto", global = true)]
    pub backend: Backend,

    /// Output JSON for machine consumption.
    #[arg(long, global = true)]
    pub json: bool,

    /// Use a local fixture backend instead of `osascript` (for tests/dev only).
    #[arg(long, global = true, value_name = "PATH", hide = true)]
    pub fixture: Option<PathBuf>,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Backend {
    /// Auto-detect the fastest available backend (prefers DB when present).
    Auto,
    /// Use `osascript` for all reads and writes.
    Osascript,
    /// Use the Apple Notes database for reads (macOS only); writes still use `osascript`.
    Db,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Accounts {
        #[command(subcommand)]
        cmd: AccountsCmd,
    },
    Folders {
        #[command(subcommand)]
        cmd: FoldersCmd,
    },
    Notes {
        #[command(subcommand)]
        cmd: NotesCmd,
    },
    /// Export all notes to a folder structure on disk.
    Export {
        /// Output directory. Created if it doesn't exist.
        #[arg(long)]
        out: String,
        /// Number of export worker threads (decode/render + IO).
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },

    /// Deprecated: use `apple-notes export ...`.
    #[command(hide = true)]
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountsCmd {
    List,
}

#[derive(Debug, Subcommand)]
pub enum FoldersCmd {
    List {
        /// Print as a simple tree.
        #[arg(long)]
        tree: bool,
    },
    Create {
        /// Parent folder path (e.g. "Personal" or "Personal > Archive").
        #[arg(long)]
        parent: String,
        /// New folder name.
        #[arg(long)]
        name: String,
    },
    Rename {
        /// Folder path to rename.
        #[arg(long)]
        folder: String,
        /// New folder name.
        #[arg(long)]
        name: String,
    },
    Delete {
        /// Folder path to delete.
        #[arg(long)]
        folder: String,
        /// Required to actually delete.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum NotesCmd {
    List {
        /// Filter notes to a folder path (e.g. "Personal > Archive").
        #[arg(long)]
        folder: Option<String>,
        /// Filter notes by title substring (case-insensitive).
        #[arg(long)]
        query: Option<String>,
        /// Limit number of rows printed (after filters).
        #[arg(long, short = 'n')]
        limit: Option<usize>,
    },
    Show {
        /// Note id (e.g. x-coredata://...).
        id: String,
        /// Output markdown (not ANSI-rendered).
        #[arg(long)]
        markdown: bool,
        /// Print raw HTML body.
        #[arg(long)]
        html: bool,
    },
    Create {
        /// Folder path (e.g. "Personal > Archive").
        #[arg(long)]
        folder: String,
        #[arg(long)]
        title: String,
        /// Plain text body.
        #[arg(long, conflicts_with_all = ["body_file", "stdin"])]
        body: Option<String>,
        /// Read body from a file.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["body", "stdin"])]
        body_file: Option<String>,
        /// Read body from stdin.
        #[arg(long, conflicts_with_all = ["body", "body_file"])]
        stdin: bool,
        /// Treat body as Markdown (stored as HTML).
        #[arg(long, conflicts_with = "html")]
        markdown: bool,
        /// Treat body as raw HTML (stored as-is).
        #[arg(long, conflicts_with = "markdown")]
        html: bool,
    },
    Rename {
        id: String,
        #[arg(long)]
        title: String,
    },
    SetBody {
        id: String,
        #[arg(long, conflicts_with_all = ["body_file", "stdin"])]
        body: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with_all = ["body", "stdin"])]
        body_file: Option<String>,
        #[arg(long, conflicts_with_all = ["body", "body_file"])]
        stdin: bool,
        #[arg(long, conflicts_with = "html")]
        markdown: bool,
        /// Treat body as raw HTML (stored as-is).
        #[arg(long, conflicts_with = "markdown")]
        html: bool,
    },
    Append {
        id: String,
        #[arg(long, conflicts_with_all = ["body_file", "stdin"])]
        body: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with_all = ["body", "stdin"])]
        body_file: Option<String>,
        #[arg(long, conflicts_with_all = ["body", "body_file"])]
        stdin: bool,
        #[arg(long, conflicts_with = "html")]
        markdown: bool,
        /// Treat body as raw HTML (stored as-is).
        #[arg(long, conflicts_with = "markdown")]
        html: bool,
    },
    Move {
        id: String,
        #[arg(long)]
        folder: String,
    },
    Delete {
        id: String,
        /// Required to actually delete.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum BackupCmd {
    Export {
        /// Output directory. Created if it doesn't exist.
        #[arg(long)]
        out: String,
        /// Number of export worker threads (render + IO). Note fetching is serialized for safety.
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
}

pub fn dispatch(args: Args, backend: Box<dyn NotesBackend>) -> anyhow::Result<()> {
    let json = args.json;
    let account = args.account.clone();
    let backend_mode = args.backend;
    let fixture = args.fixture.clone();
    let cmd = args.cmd;

    match cmd {
        Command::Accounts { cmd } => match cmd {
            AccountsCmd::List => {
                let accounts = backend.list_accounts()?;
                if json {
                    print_json(&accounts)
                } else {
                    #[derive(Debug)]
                    struct AccountRow {
                        name: String,
                    }
                    impl tables::TableRow for AccountRow {
                        const HEADERS: &'static [&'static str] = &["Account"];
                        fn cells(&self) -> Vec<Cell> {
                            vec![Cell::new(self.name.as_str())]
                        }
                    }

                    tables::render_table(
                        accounts
                            .into_iter()
                            .map(|a| AccountRow { name: a.name })
                            .collect(),
                    );
                    Ok(())
                }
            }
        },
        Command::Folders { cmd } => match cmd {
            FoldersCmd::List { tree } => {
                let spinner = progress::spinner("Loading folders…");
                let folders = backend.list_folders(&account)?;
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }
                if json {
                    print_json(&folders)
                } else if tree {
                    print_folder_tree(&folders)
                } else {
                    print_folders_table(&folders)
                }
            }
            FoldersCmd::Create { parent, name } => {
                let parent_path = split_folder_path(&parent)?;
                let id = backend.create_folder(&account, &parent_path, &name)?;
                if json {
                    print_json(&serde_json::json!({ "id": id }))
                } else {
                    println!("{id}");
                    Ok(())
                }
            }
            FoldersCmd::Rename { folder, name } => {
                let folder_path = split_folder_path(&folder)?;
                backend.rename_folder(&account, &folder_path, &name)?;
                Ok(())
            }
            FoldersCmd::Delete { folder, yes } => {
                if !yes {
                    return Err(anyhow!("refusing to delete without --yes"));
                }
                let folder_path = split_folder_path(&folder)?;
                backend.delete_folder(&account, &folder_path)?;
                Ok(())
            }
        },
        Command::Notes { cmd } => dispatch_notes(json, &account, backend, cmd),
        Command::Export { out, jobs } => {
            if fixture.is_some() {
                return backup::export_all(&*backend, &account, out, jobs);
            }
            match backend_mode {
                Backend::Osascript => backup::export_all(&*backend, &account, out, jobs),
                Backend::Db => backup::export_all_db(&account, out, jobs),
                Backend::Auto => backup::export_all_db(&account, out.clone(), jobs)
                    .or_else(|_| backup::export_all(&*backend, &account, out, jobs)),
            }
        }
        Command::Backup { cmd } => match cmd {
            BackupCmd::Export { out, jobs } => {
                if fixture.is_some() {
                    return backup::export_all(&*backend, &account, out, jobs);
                }
                match backend_mode {
                    Backend::Osascript => backup::export_all(&*backend, &account, out, jobs),
                    Backend::Db => backup::export_all_db(&account, out, jobs),
                    Backend::Auto => backup::export_all_db(&account, out.clone(), jobs)
                        .or_else(|_| backup::export_all(&*backend, &account, out, jobs)),
                }
            }
        },
    }
}

fn dispatch_notes(
    json: bool,
    account: &str,
    backend: Box<dyn NotesBackend>,
    cmd: NotesCmd,
) -> anyhow::Result<()> {
    match cmd {
        NotesCmd::List {
            folder,
            query,
            limit,
        } => {
            let (mut notes, folder_hint, folder_index) = if let Some(folder) = folder {
                let folder_path = split_folder_path(&folder)?;
                let spinner = progress::spinner("Loading notes… 0 loaded");
                let mut notes = Vec::new();
                let mut loaded = 0usize;
                backend.stream_note_summaries(account, Some(&folder_path), &mut |n| {
                    loaded += 1;
                    if let Some(spinner) = &spinner
                        && (loaded == 1 || loaded.is_multiple_of(25))
                    {
                        spinner.set_message(format!("Loading notes… {loaded} loaded"));
                    }
                    notes.push(n);
                })?;
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }
                (notes, Some(folder), None)
            } else {
                let spinner = progress::spinner("Loading folders…");
                let folders = backend.list_folders(account)?;
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }
                let folder_index = backup::FolderIndex::new(&folders)?;

                let spinner = progress::spinner("Loading notes… 0 loaded");
                let mut notes = Vec::new();
                let mut loaded = 0usize;
                backend.stream_note_summaries(account, None, &mut |n| {
                    loaded += 1;
                    if let Some(spinner) = &spinner
                        && (loaded == 1 || loaded.is_multiple_of(25))
                    {
                        spinner.set_message(format!("Loading notes… {loaded} loaded"));
                    }
                    notes.push(n);
                })?;
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }

                (notes, None, Some(folder_index))
            };

            if let Some(q) = query {
                let q = q.to_lowercase();
                notes.retain(|n| n.title.to_lowercase().contains(&q));
            }

            if json {
                if let Some(limit) = limit {
                    notes.truncate(limit);
                }
                print_json(&notes)
            } else if let Some(folder_hint) = folder_hint {
                print_note_summaries_folder_hint(&notes, &folder_hint, limit)
            } else {
                print_note_summaries(
                    &notes,
                    folder_index.as_ref().expect("folder index missing"),
                    limit,
                )
            }
        }
        NotesCmd::Show { id, markdown, html } => {
            let spinner = progress::spinner("Loading note…");
            let note = backend.get_note(&id)?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            if json {
                print_json(&note)
            } else if html {
                println!("{}", note.body_html);
                Ok(())
            } else {
                let md = render::note_to_markdown(&note);
                if markdown || !io::stdout().is_terminal() {
                    println!("{}", md);
                    return Ok(());
                }
                let rendered = render::render_markdown(&md);
                print!("{rendered}");
                Ok(())
            }
        }
        NotesCmd::Create {
            folder,
            title,
            body,
            body_file,
            stdin,
            markdown,
            html,
        } => {
            let body = read_body(body, body_file, stdin)?;
            let body_html = if html {
                body
            } else if markdown {
                render::markdown_to_html(&body)
            } else {
                render::text_to_html(&body)
            };
            let folder_path = split_folder_path(&folder)?;
            let spinner = progress::spinner("Creating note…");
            let id = backend.create_note_html(account, &folder_path, &title, &body_html)?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            if json {
                print_json(&serde_json::json!({ "id": id }))
            } else {
                println!("{id}");
                Ok(())
            }
        }
        NotesCmd::Rename { id, title } => {
            let spinner = progress::spinner("Renaming note…");
            backend.set_note_title(&id, &title)?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            Ok(())
        }
        NotesCmd::SetBody {
            id,
            body,
            body_file,
            stdin,
            markdown,
            html,
        } => {
            let body = read_body(body, body_file, stdin)?;
            let body_html = if html {
                body
            } else if markdown {
                render::markdown_to_html(&body)
            } else {
                render::text_to_html(&body)
            };
            let spinner = progress::spinner("Updating note body…");
            backend.set_note_body_html(&id, &body_html)?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            Ok(())
        }
        NotesCmd::Append {
            id,
            body,
            body_file,
            stdin,
            markdown,
            html,
        } => {
            let body = read_body(body, body_file, stdin)?;
            let body_html = if html {
                body
            } else if markdown {
                render::markdown_to_html(&body)
            } else {
                render::text_to_html(&body)
            };
            let spinner = progress::spinner("Appending to note…");
            backend.append_note_body_html(&id, &body_html)?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            Ok(())
        }
        NotesCmd::Move { id, folder } => {
            let folder_path = split_folder_path(&folder)?;
            let spinner = progress::spinner("Moving note…");
            backend.move_note(&id, account, &folder_path)?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            Ok(())
        }
        NotesCmd::Delete { id, yes } => {
            if !yes {
                return Err(anyhow!("refusing to delete without --yes"));
            }
            let spinner = progress::spinner("Deleting note…");
            backend.delete_note(&id)?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            Ok(())
        }
    }
}

fn read_body(
    body: Option<String>,
    body_file: Option<String>,
    stdin: bool,
) -> anyhow::Result<String> {
    if let Some(body) = body {
        return Ok(body);
    }
    if let Some(path) = body_file {
        return std::fs::read_to_string(&path).with_context(|| format!("read {path}"));
    }
    if stdin {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).context("read stdin")?;
        return Ok(s);
    }
    Ok(String::new())
}

fn split_folder_path(path: &str) -> anyhow::Result<Vec<String>> {
    let parts: Vec<String> = path
        .split('>')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    if parts.is_empty() {
        return Err(anyhow!("folder path is empty"));
    }
    Ok(parts)
}

fn print_folders_table(folders: &[Folder]) -> anyhow::Result<()> {
    #[derive(Debug)]
    struct FolderRow {
        path: String,
        id: String,
    }
    impl tables::TableRow for FolderRow {
        const HEADERS: &'static [&'static str] = &["Folder", "Id"];
        fn cells(&self) -> Vec<Cell> {
            vec![
                Cell::new(self.path.as_str()),
                Cell::new(tables::shorten_id_for_table(self.id.as_str())),
            ]
        }
    }

    let mut rows: Vec<FolderRow> = folders
        .iter()
        .map(|f| FolderRow {
            path: f.path_string(),
            id: f.id.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));

    tables::render_table(rows);
    Ok(())
}

fn print_folder_tree(folders: &[Folder]) -> anyhow::Result<()> {
    let mut folders = folders.to_vec();
    folders.sort_by(|a, b| a.path.cmp(&b.path));
    for f in folders {
        let indent = "  ".repeat(f.path.len().saturating_sub(1));
        println!("{indent}{}", f.name);
    }
    Ok(())
}

fn print_note_summaries(
    notes: &[NoteSummary],
    folder_index: &backup::FolderIndex,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    #[derive(Debug)]
    struct NoteRow {
        id: String,
        folder: String,
        title: String,
    }
    impl tables::TableRow for NoteRow {
        const HEADERS: &'static [&'static str] = &["Id", "Folder", "Title"];
        fn cells(&self) -> Vec<Cell> {
            vec![
                Cell::new(tables::shorten_id_for_table(self.id.as_str())),
                Cell::new(self.folder.as_str()),
                Cell::new(self.title.as_str()),
            ]
        }
    }

    let mut rows: Vec<NoteRow> = notes
        .iter()
        .map(|n| NoteRow {
            id: n.id.clone(),
            folder: folder_index
                .folder_path_string(&n.folder_id)
                .unwrap_or_else(|| "?".to_string()),
            title: n.title.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.title.cmp(&b.title));
    if let Some(limit) = limit {
        rows.truncate(limit);
    }

    tables::render_table(rows);
    Ok(())
}

fn print_note_summaries_folder_hint(
    notes: &[NoteSummary],
    folder: &str,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    #[derive(Debug)]
    struct NoteRow {
        id: String,
        folder: String,
        title: String,
    }
    impl tables::TableRow for NoteRow {
        const HEADERS: &'static [&'static str] = &["Id", "Folder", "Title"];
        fn cells(&self) -> Vec<Cell> {
            vec![
                Cell::new(tables::shorten_id_for_table(self.id.as_str())),
                Cell::new(self.folder.as_str()),
                Cell::new(self.title.as_str()),
            ]
        }
    }

    let mut rows: Vec<NoteRow> = notes
        .iter()
        .map(|n| NoteRow {
            id: n.id.clone(),
            folder: folder.to_string(),
            title: n.title.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.title.cmp(&b.title));
    if let Some(limit) = limit {
        rows.truncate(limit);
    }

    tables::render_table(rows);
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_folder_path_parses_and_trims() {
        assert_eq!(
            split_folder_path("Personal > Archive").unwrap(),
            vec!["Personal".to_string(), "Archive".to_string()]
        );
        assert_eq!(
            split_folder_path("  Personal>Archive  ").unwrap(),
            vec!["Personal".to_string(), "Archive".to_string()]
        );
    }

    #[test]
    fn split_folder_path_rejects_empty() {
        assert!(split_folder_path("   ").is_err());
        assert!(split_folder_path(" > > ").is_err());
    }

    #[test]
    fn read_body_prefers_inline() {
        assert_eq!(
            read_body(Some("x".into()), Some("y".into()), true).unwrap(),
            "x"
        );
    }
}
