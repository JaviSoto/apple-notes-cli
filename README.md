# apple-notes-cli

A fast, scriptable CLI for Apple Notes (read/write + backups).

It talks to `Notes.app` via Apple Events (`osascript`). It is meant to be run on macOS.

No browser, no Accessibility/UI scripting.

## Status

This is new and evolving. It’s being built with open-sourcing in mind.

Vibe-coded with GPT 5.2 in Codex. Use with caution.

## Install (dev)

```bash
cargo build
```

## Install (Homebrew)

Once a release is published, you can install via Homebrew:

```bash
brew tap JaviSoto/tap
brew install apple-notes-cli
apple-notes --version
```

## Install (Cargo)

Until this is published on crates.io, you can install from Git:

```bash
cargo install --git https://github.com/JaviSoto/apple-notes-cli.git --bin apple-notes
apple-notes --version
```

## First-run permissions (macOS)

The first time you run commands that touch Notes, macOS may prompt with an “Automation” dialog (e.g. `osascript` → “Notes”). You must allow it once on the target Mac.

## Backends

By default the CLI uses `--backend auto`, which prefers a fast SQLite/CoreData read path (Apple Notes’ `NoteStore.sqlite`) when available, and falls back to `osascript` otherwise.

- `apple-notes --backend db …` — fast reads (list/index) from the local Notes database; writes + full note reads still use `osascript`.
- `apple-notes --backend osascript …` — everything via `osascript` (slower for large accounts, but doesn’t depend on DB schema).

## Usage

### Accounts / folders

```bash
apple-notes accounts list
apple-notes folders list
apple-notes folders list --tree
apple-notes folders create --parent "Personal" --name "My New Folder"
```

By default, list commands render **pretty tables**. Use `--json` for machine-readable output.

### Notes

List notes (TSV):

```bash
apple-notes notes list --folder "Personal > Archive"
apple-notes notes list --query "meeting"
apple-notes notes list --limit 20
```

Show a note (renders Markdown to your terminal by default):

```bash
apple-notes notes show x-coredata://...
apple-notes notes show x-coredata://... --markdown
apple-notes notes show x-coredata://... --html
```

Create a note:

```bash
apple-notes notes create --folder "Personal > Archive" --title "Hello" --body "Hi!"
echo '# Title' | apple-notes notes create --folder "Personal > Archive" --title "From stdin" --stdin --markdown
```

Edit a note:

```bash
apple-notes notes rename x-coredata://... --title "New title"
apple-notes notes set-body x-coredata://... --body "New body"
apple-notes notes append x-coredata://... --body "Extra lines"
apple-notes notes move x-coredata://... --folder "Personal > Archive"
apple-notes notes delete x-coredata://... --yes
```

### Backup / export

Exports *every* note in the selected account under an output directory:

```bash
apple-notes export --out ./notes-backup
apple-notes export --out ./notes-backup --jobs 6
```

Directory structure mirrors Notes folder structure (e.g. `notes-backup/Personal/Archive/...`).

- Each note becomes a folder containing:
  - `metadata.json` (id, title, folder, dates)
  - `contents.md` (best-effort extracted Markdown/plain text)

By default (`--backend auto`), export prefers the fast DB path and falls back to `osascript` if needed.

Notes:
- DB export uses Apple Notes’ current local DB schema and a best-effort text extraction for note bodies.
- `--jobs` parallelizes decode/render + IO. (When using the `osascript` backend, note fetching is intentionally serialized for safety.)

## Design notes

- Reads are done via JXA (`osascript -l JavaScript`) and emitted as JSON for robust parsing.
- Writes are done via AppleScript (JXA “make” can be unreliable).

## Maintainer notes

### Homebrew tap automation

The release workflow updates the `JaviSoto/homebrew-tap` formula automatically.

Repo secret required:
- `HOMEBREW_TAP_TOKEN`: fine-grained PAT with read/write access to `JaviSoto/homebrew-tap`.
