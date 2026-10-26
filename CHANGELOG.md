# Changelog

## Unreleased

## 0.1.3 — 2026-10-26

- Address Notes through `/System/Applications/Notes.app`; the bundle-ID Apple Events target used in 0.1.2 fails on this Mac.

## 0.1.2 — 2026-08-04

- Attempt to resolve Notes Apple Events by bundle identifier.
- Add explicit `--read-only` mode for assistant lookups; it blocks folder and note writes and deletes before invoking a write backend.

## 0.1.1 — 2026-06-17

- Initial release.
