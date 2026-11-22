# Changelog — FormCraft

All notable changes to FormCraft are documented here.

## [Unreleased]

### Added

- SQLite browser slice: open a database file, list tables and show rows in a
  grid, run read-only SQL queries, export the displayed rows as CSV and report
  row counts in the status bar.
- In-memory `people` sample database behind `--sample`.
- Temporary-database tests for table creation, inserts, SELECT, CSV escaping
  and document JSON round-trip.

## [0.1.0] - 2026-11-22

### Added

- Initial CraftKit-based application shell.
- Versioned project files with atomic save.
- Undo/redo through the shared document history.
- Background jobs with progress and cancellation.
- Application-specific document model and UI.
