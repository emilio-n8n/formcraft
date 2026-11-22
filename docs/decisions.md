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
