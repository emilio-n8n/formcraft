# FormCraft

SQLite database browser and form builder

**FormCraft is an independent, community open-source project.** It is not an
official product of, endorsed by, or affiliated with any third-party vendor it
may interoperate with.

Built on [CraftKit](https://github.com/emilio-n8n/craftkit), a small native Rust
desktop foundation.

## Features

- Native Rust desktop UI (egui/eframe), no web runtime.
- Open a SQLite file and browse its tables in an editable grid.
- Double-click a cell to edit it; add or delete rows with confirmation for
  destructive operations. Each data edit is recorded in a session undo stack.
- Schema editor: create tables with typed columns, add a column, drop a table
  with confirmation, and inspect the current table schema.
- Generated form view for creating and updating a record.
- CSV import into an existing table (headers matched case-insensitively,
  values checked against declared types, single SQLite transaction).
- Read-only SQL query box with a Run button, inline error display and a bounded
  query history persisted in the project file.
- Text filter, row limit and refresh for large tables.
- CSV export of the currently displayed rows.
- `--sample` starts with an in-memory `people` database filled with demo rows.
- Versioned JSON project files with atomic saves.
- Bounded undo/redo through the shared document history.
- Recent files and persisted preferences.
- Offline-first: no account, no telemetry, no network required.

FormCraft does not read Microsoft Access `.accdb` files. Only SQLite is
supported. Free-form SQL stays read-only; all writes go through the controlled,
transactional UI actions.

## Run

```bash
cargo run --release -- --sample
cargo run --release -- --open project.json
```

Without arguments the application opens an empty document.

- `--sample` opens the built-in demonstration document.
- `--open <file>` opens a project.
- `--version` prints the version.
- `--help` prints usage.

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Linux windowing development packages are installed by the CI workflow.

## Project layout

```text
src/main.rs       CLI and window startup
src/app.rs        UI, menus and lifecycle
src/document.rs   application document model
src/...           domain modules
```

The application depends on pinned CraftKit crates from Git. Keep the tag in
`Cargo.toml` and `Cargo.lock` consistent when upgrading the foundation.

## File format

Projects are JSON envelopes:

```json
{
  "format": "formcraft",
  "schema_version": 2,
  "app_version": "0.1.0",
  "document": { }
}
```

The application owns the `document` schema and its migrations. Schema v2 adds
`query_history`; v1 projects omit it and are read with an empty history. A
newer `schema_version` is rejected instead of being silently misread.

## Licence

Licensed under either of:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT license (`LICENSE-MIT`)

at your option.

## Third-party notices

See [THIRD_PARTY.md](THIRD_PARTY.md).
