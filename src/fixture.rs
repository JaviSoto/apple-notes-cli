use crate::model::{Account, Folder, Note, NoteSummary};
use crate::transport::NotesBackend;
use anyhow::{Context, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, Deserialize)]
struct FixtureData {
    accounts: Vec<Account>,
    folders_by_account: HashMap<String, Vec<Folder>>,
    note_summaries_by_account: HashMap<String, Vec<NoteSummary>>,
    notes_by_id: HashMap<String, Note>,
}

#[derive(Debug)]
pub struct FixtureBackend {
    data: FixtureData,
    next_id: AtomicUsize,
}

impl FixtureBackend {
    pub fn from_path(path: PathBuf) -> anyhow::Result<Self> {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("read fixture file {}", path.display()))?;
        Self::from_str(&data).with_context(|| format!("parse fixture {}", path.display()))
    }

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let data: FixtureData = serde_json::from_str(s).context("invalid fixture JSON")?;
        Ok(Self {
            data,
            next_id: AtomicUsize::new(1),
        })
    }

    fn folders(&self, account: &str) -> anyhow::Result<Vec<Folder>> {
        self.data
            .folders_by_account
            .get(account)
            .cloned()
            .ok_or_else(|| anyhow!("fixture missing folders for account {account:?}"))
    }

    fn note_summaries(&self, account: &str) -> anyhow::Result<Vec<NoteSummary>> {
        self.data
            .note_summaries_by_account
            .get(account)
            .cloned()
            .ok_or_else(|| anyhow!("fixture missing notes for account {account:?}"))
    }
}

impl NotesBackend for FixtureBackend {
    fn list_accounts(&self) -> anyhow::Result<Vec<Account>> {
        Ok(self.data.accounts.clone())
    }

    fn list_folders(&self, account: &str) -> anyhow::Result<Vec<Folder>> {
        self.folders(account)
    }

    fn list_notes(&self, account: &str) -> anyhow::Result<Vec<NoteSummary>> {
        self.note_summaries(account)
    }

    fn list_notes_in_folder(
        &self,
        account: &str,
        folder_path: &[String],
    ) -> anyhow::Result<Vec<NoteSummary>> {
        let folders = self.folders(account)?;
        let want = folder_path.join(" > ");
        let folder = folders
            .into_iter()
            .find(|f| f.path.join(" > ") == want)
            .ok_or_else(|| anyhow!("fixture missing folder {want:?}"))?;

        let mut notes = self.note_summaries(account)?;
        notes.retain(|n| n.folder_id == folder.id);
        Ok(notes)
    }

    fn stream_note_summaries(
        &self,
        account: &str,
        folder_path: Option<&[String]>,
        on_note: &mut dyn FnMut(NoteSummary),
    ) -> anyhow::Result<()> {
        let mut notes = if let Some(folder_path) = folder_path {
            self.list_notes_in_folder(account, folder_path)?
        } else {
            self.list_notes(account)?
        };
        // Deterministic order for tests.
        notes.sort_by(|a, b| a.id.cmp(&b.id));
        for n in notes {
            on_note(n);
        }
        Ok(())
    }

    fn get_note(&self, id: &str) -> anyhow::Result<Note> {
        self.data
            .notes_by_id
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("fixture missing note id {id:?}"))
    }

    fn create_note_html(
        &self,
        _account: &str,
        _folder_path: &[String],
        _title: &str,
        _body_html: &str,
    ) -> anyhow::Result<String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(format!("fixture://note/{id}"))
    }

    fn set_note_title(&self, _id: &str, _title: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn set_note_body_html(&self, _id: &str, _body_html: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn append_note_body_html(&self, _id: &str, _body_html: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn delete_note(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn move_note(&self, _id: &str, _account: &str, _folder_path: &[String]) -> anyhow::Result<()> {
        Ok(())
    }

    fn create_folder(
        &self,
        _account: &str,
        _parent_path: &[String],
        _name: &str,
    ) -> anyhow::Result<String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(format!("fixture://folder/{id}"))
    }

    fn rename_folder(
        &self,
        _account: &str,
        _folder_path: &[String],
        _name: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn delete_folder(&self, _account: &str, _folder_path: &[String]) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_can_load_minimal() {
        let json = r#"
{
  "accounts": [{"name":"iCloud"}],
  "folders_by_account": {
    "iCloud": [{"id":"f1","name":"Personal","account":"iCloud","path":["Personal"]}]
  },
  "note_summaries_by_account": {
    "iCloud": [{"id":"n1","title":"Hello","folder_id":"f1"}]
  },
  "notes_by_id": {
    "n1": {
      "id":"n1",
      "title":"Hello",
      "folder_id":"f1",
      "created_at":"2025-12-20T00:00:00Z",
      "modified_at":"2025-12-20T00:00:00Z",
      "body_html":"<div>Hi</div>"
    }
  }
}
"#;
        let backend = FixtureBackend::from_str(json).unwrap();
        assert_eq!(backend.list_accounts().unwrap().len(), 1);
        assert_eq!(backend.list_notes("iCloud").unwrap().len(), 1);
        assert_eq!(backend.get_note("n1").unwrap().title, "Hello");
    }
}
