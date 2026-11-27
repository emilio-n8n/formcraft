# Changelog — FormCraft

All notable changes to FormCraft are documented here.

## [Unreleased]

### Fixed

- Enforce read-only SQL with SQLite `query_only`, blocking writable common
  table expressions and mutating PRAGMAs while restoring the connection mode.
- Make batched data undo/redo atomic and restore the in-memory edit when a
  transaction fails.
- Keep query results read-only instead of exposing row actions without row IDs.
- Bind cell edits to a stable row ID and column, and bind delete confirmations
  to the table that opened them.
- Clear stale database sessions when opening a project without an attached
  database, and reset the form after creating a record.
- Reject duplicate CSV headers, accept a UTF-8 BOM, preserve quoted newlines and
  report the failing data row during transactional imports.
- Browse `WITHOUT ROWID` tables and tables that shadow `rowid` safely, disabling
  CRUD when no unambiguous internal key exists.

### Added

- Inline form validation for required fields, integers and finite real numbers.
- Scrollable data, form, table and schema panels, plus explicit empty-table,
  empty-filter and empty-query states.
- A bounded 100-entry data undo/redo stack and schema dialogs that remain open
  when validation fails.

## [0.2.0] - 2026-11-22

### Added (productisation slice, planned v0.2.0)

- Editable grid: double-click a cell to edit it, add a row, delete a row with a
  confirmation dialog, with a per-edit undo/redo stack for SQLite data.
- Schema editor: create a table from typed columns, add a column, drop a table
  after confirmation, and display the current schema.
- Generated form view to create or update a record from the table schema.
- CSV import into an existing table with type checking, header mapping and
  transactional rollback on error.
- Query history persisted in the project (schema v2, v1 files still load).
- Text filter, row limit and refresh for the grid.
- Tests for CRUD, schema changes, form generation, CSV import, SQL identifier
  escaping and project round-trip/migration.

### Added (tranche 1)

- SQLite browser slice: open a database file, list tables and show rows in a
  grid, run read-only SQL queries, export the displayed rows as CSV and report
  row counts in the status bar.
- In-memory `people` sample database behind `--sample`.

## [0.1.0] - 2026-11-22

### Added

- Initial CraftKit-based application shell.
- Versioned project files with atomic save.
- Undo/redo through the shared document history.
- Background jobs with progress and cancellation.
- Application-specific document model and UI.
