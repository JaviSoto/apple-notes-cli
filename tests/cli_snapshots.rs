use insta::assert_snapshot;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.json")
}

fn run_ok(args: &[&str]) -> String {
    let mut cmd = assert_cmd::cargo_bin_cmd!("apple-notes");
    cmd.arg("--fixture")
        .arg(fixture_path())
        .env("NO_COLOR", "1")
        .env("NO_PROGRESS", "1")
        .env("COLUMNS", "120")
        .args(args);

    let out = cmd.assert().success().get_output().stdout.clone();
    String::from_utf8(out).expect("utf8 stdout")
}

fn run_err(args: &[&str]) -> String {
    let mut cmd = assert_cmd::cargo_bin_cmd!("apple-notes");
    cmd.arg("--fixture")
        .arg(fixture_path())
        .env("NO_COLOR", "1")
        .env("NO_PROGRESS", "1")
        .env("COLUMNS", "120")
        .args(args);

    let out = cmd.assert().failure().get_output().stderr.clone();
    String::from_utf8(out).expect("utf8 stderr")
}

#[test]
fn snapshot_help() {
    let out = run_ok(&["--help"]);
    assert_snapshot!("help", out);
}

#[test]
fn snapshot_accounts_list_table() {
    let out = run_ok(&["accounts", "list"]);
    assert_snapshot!("accounts_list", out);
}

#[test]
fn snapshot_folders_list_table() {
    let out = run_ok(&["folders", "list"]);
    assert_snapshot!("folders_list", out);
}

#[test]
fn snapshot_notes_list_table() {
    let out = run_ok(&["notes", "list"]);
    assert_snapshot!("notes_list", out);
}

#[test]
fn snapshot_notes_list_folder_table() {
    let out = run_ok(&["notes", "list", "--folder", "Personal > Archive"]);
    assert_snapshot!("notes_list_folder", out);
}

#[test]
fn snapshot_notes_show_markdown() {
    let out = run_ok(&["notes", "show", "n2", "--markdown"]);
    assert_snapshot!("notes_show_markdown", out);
}

#[test]
fn snapshot_notes_create_prints_id() {
    let out = run_ok(&[
        "notes",
        "create",
        "--folder",
        "Personal > Archive",
        "--title",
        "Hello",
        "--body",
        "Hi",
    ]);
    assert_snapshot!("notes_create_id", out);
}

#[test]
fn snapshot_folders_create_prints_id() {
    let out = run_ok(&["folders", "create", "--parent", "Personal", "--name", "New"]);
    assert_snapshot!("folders_create_id", out);
}

#[test]
fn snapshot_notes_delete_requires_yes() {
    let out = run_err(&["notes", "delete", "n2"]);
    assert_snapshot!("notes_delete_requires_yes", out);
}

#[test]
fn snapshot_folders_delete_requires_yes() {
    let out = run_err(&["folders", "delete", "--folder", "Personal > Archive"]);
    assert_snapshot!("folders_delete_requires_yes", out);
}

#[test]
fn backup_export_writes_all_notes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out_dir = dir.path().join("backup");

    let mut cmd = assert_cmd::cargo_bin_cmd!("apple-notes");
    cmd.arg("--fixture")
        .arg(fixture_path())
        .env("NO_COLOR", "1")
        .env("NO_PROGRESS", "1")
        .env("COLUMNS", "120")
        .args(["backup", "export", "--out"])
        .arg(&out_dir);

    cmd.assert().success();

    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&out_dir) {
        let entry = entry.expect("walkdir entry");
        if entry.file_type().is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(&out_dir)
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    files.sort();
    assert!(
        files.iter().any(|p| p.ends_with("/metadata.json")),
        "expected metadata.json files"
    );
    assert!(
        files.iter().any(|p| p.ends_with("/contents.md")),
        "expected contents.md files"
    );
    assert_eq!(files.len(), 6, "expected 2 files per note (3 notes)");
    assert_snapshot!("backup_files", files.join("\n"));
}

#[test]
fn backup_export_writes_all_notes_jobs_1() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out_dir = dir.path().join("backup");

    let mut cmd = assert_cmd::cargo_bin_cmd!("apple-notes");
    cmd.arg("--fixture")
        .arg(fixture_path())
        .env("NO_COLOR", "1")
        .env("NO_PROGRESS", "1")
        .env("COLUMNS", "120")
        .args(["backup", "export", "--out"])
        .arg(&out_dir)
        .args(["--jobs", "1"]);

    cmd.assert().success();

    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&out_dir) {
        let entry = entry.expect("walkdir entry");
        if entry.file_type().is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(&out_dir)
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    files.sort();
    assert_eq!(files.len(), 6, "expected 2 files per note (3 notes)");
    assert_snapshot!("backup_files_jobs_1", files.join("\n"));
}
