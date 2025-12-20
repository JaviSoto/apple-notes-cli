# Contributing

Thanks for helping make `apple-notes` better.

## Development

Requirements:

- Rust stable
- macOS for real Notes integration (CI runs on Linux using fixtures)

Common commands:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

## Backends

- `--backend osascript` talks to Notes via Apple Events (`osascript`).
- `--backend db` reads list/index data from Apple Notes’ local database (`NoteStore.sqlite`), but still uses `osascript` for writes and full note bodies.

The DB backend is intentionally “best effort” and may need updates when Apple changes the schema.

## Release process (maintainers)

- Tag a release: `git tag vX.Y.Z && git push origin vX.Y.Z`
- GitHub Actions builds and uploads artifacts, and updates the Homebrew tap formula (requires `HOMEBREW_TAP_TOKEN` repo secret).

