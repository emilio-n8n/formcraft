# Decisions — FormCraft

Short architecture decision records. Newest first.

## ADR-001 — Built on CraftKit

Date: 2026-11-22

Decision: use the pinned CraftKit crates for the native shell, project
lifecycle, history and background jobs.

Alternatives: build the desktop foundation from scratch; use another UI
framework.

Reasons: consistent behaviour across the portfolio, fewer duplicated bugs,
shared release and testing conventions.

Consequences: CraftKit upgrades are explicit tag changes; domain code stays in
this repository.

## ADR-002 — Lightweight document, session-held database

Date: 2026-11-22

Decision: the persisted `Document` holds only `title`, `database_path` and
`last_query`. The `rusqlite::Connection` is a session resource owned by the
application, not by the document or its undo history.

Alternatives: embed the whole database in the JSON project; serialize every
query result into the document.

Reasons: a `Connection` is neither `Clone` nor `PartialEq`, so it cannot live
inside `DocumentHost<T>` snapshots. Reopening from `database_path` is cheap and
keeps project files small.

Consequences: a saved project remembers which SQLite file to reopen but does
not copy its data. A missing or moved database file is reported as a user
notice on open. Query results are rebuilt from the database after undo/redo.

## ADR-003 — Read-only SQL query box

Date: 2026-11-22

Decision: the query box accepts only statements starting with `SELECT`, `WITH`,
`PRAGMA`, `EXPLAIN` or `VALUES` (leading whitespace and SQL comments allowed).

Alternatives: pass any statement to SQLite.

Reasons: browsing should never silently destroy data; a typo in the query box
must not drop a table.

Consequences: data-editing statements return a clear error instead of running.
Table browsing uses `SELECT *` from table names quoted as SQL identifiers.

## ADR-004 — Controlled writes with a session undo stack

Date: 2026-11-22

Decision: free-form SQL remains read-only. Data writes go through explicit
transactional helpers (`update_cell`, `insert_record`, `update_record`,
`delete_row`). Each user action records a `DataEdit` with its inverse in a
bounded, session-only undo/redo stack; CraftKit history still owns document
fields such as the title, last query and query history.

Alternatives: allow `UPDATE`/`DELETE` in the query box; put inverse SQL in the
persisted document.

Reasons: a browsing tool must not destroy data accidentally, and database
connections cannot live in `DocumentHost<T>` snapshots. A row `rowid` is a
stable edit key for SQLite tables created by the application.

Consequences: destructive actions (row and table deletion) ask for
confirmation. Undo of a deleted row re-inserts it with a new `rowid`; schema
changes are not part of the undo stack and are always confirmed.

## ADR-005 — Schema v2 and persisted query history

Date: 2026-11-22

Decision: `Document` gains `query_history: Vec<String>` with `#[serde(default)]`
and the project schema version is bumped from 1 to 2.

Alternatives: keep query history in memory only; silently reuse schema v1.

Reasons: a productised browser should remember recent queries across restarts.
The serde default makes v1 files load unchanged, and CraftKit rejects newer
schemas rather than misreading them.

Consequences: saved projects are written as `schema_version: 2`; older files
are migrated in memory on load. A migration test checks v1 compatibility.

## ADR-006 — CSV import and generated forms

Date: 2026-11-22

Decision: CSV import parses headers and values with the `csv` crate, matches
headers case-insensitively to table columns, coerces text to the declared
column type and inserts everything in one transaction. The form view is
generated from `PRAGMA table_info` rather than stored in the project.

Alternatives: hand-rolled CSV parsing; a fixed hardcoded form per table.

Reasons: malformed CSV and type errors must be reported to the user without
leaving a partially imported table; generated forms stay correct after schema
changes.

Consequences: BLOB columns are imported as text in this slice, and form fields
for typed columns are validated before being written.
