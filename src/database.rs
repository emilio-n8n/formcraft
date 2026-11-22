//! SQLite access layer for FormCraft.
//!
//! Everything in this module is UI-free and testable on temporary databases.
//! Free-form SQL is read-only; writes go through the explicit, transactional
//! helpers below so that the UI can validate and confirm them. User-visible
//! SQLite failures are returned as [`DbError`] and never panic.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use csv::{ReaderBuilder, StringRecord};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, params, params_from_iter};

/// Columns and rows returned by a query.
///
/// Values are stored as optional text so that any SQLite type can be rendered
/// in the grid and exported to CSV. `None` represents SQL `NULL`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryResult {
    /// Column names, in result order.
    pub columns: Vec<String>,
    /// Rows of values; `None` represents SQL `NULL`.
    pub rows: Vec<Vec<Option<String>>>,
}

impl QueryResult {
    /// Number of returned rows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// True when the grid has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// A table grid plus the SQLite `rowid` of each row, for controlled edits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableRows {
    /// Displayed columns and values.
    pub result: QueryResult,
    /// Stable edit key for each row, in the same order as `result.rows`.
    pub row_ids: Vec<i64>,
}

/// Errors produced by the database layer.
#[derive(Debug)]
pub enum DbError {
    /// SQLite reported an error.
    Sqlite(rusqlite::Error),
    /// The statement was not a read-only query.
    NotAQuery,
    /// A user-supplied name is not a safe SQLite identifier.
    InvalidName(String),
    /// A table definition is invalid (no columns, duplicates, wrong count).
    InvalidDefinition(String),
    /// Input could not be parsed or coerced to the requested type.
    Invalid(String),
    /// CSV parsing or import failed.
    Csv(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::NotAQuery => formatter.write_str(
                "Only read-only queries are allowed (SELECT, WITH, PRAGMA, EXPLAIN, VALUES)",
            ),
            Self::InvalidName(name) => write!(formatter, "'{name}' is not a valid name"),
            Self::InvalidDefinition(message) => write!(formatter, "Invalid definition: {message}"),
            Self::Invalid(message) => formatter.write_str(message),
            Self::Csv(message) => write!(formatter, "CSV: {message}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Column types offered by the schema editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlType {
    /// `INTEGER` affinity.
    Integer,
    /// `REAL` affinity.
    Real,
    /// `TEXT` affinity.
    Text,
}

impl SqlType {
    /// All types offered by the UI, in display order.
    pub const ALL: [Self; 3] = [Self::Text, Self::Integer, Self::Real];

    /// SQL declaration used in `CREATE TABLE` / `ALTER TABLE`.
    #[must_use]
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Integer => "INTEGER",
            Self::Real => "REAL",
            Self::Text => "TEXT",
        }
    }

    /// Human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.as_sql()
    }

    /// Best-effort conversion from a `PRAGMA table_info` declaration.
    #[must_use]
    pub fn from_declaration(declaration: &str) -> Self {
        declaration.parse().unwrap_or(Self::Text)
    }
}

impl fmt::Display for SqlType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_sql())
    }
}

impl FromStr for SqlType {
    type Err = DbError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_uppercase().as_str() {
            "INTEGER" | "INT" => Ok(Self::Integer),
            "REAL" | "FLOAT" | "DOUBLE" | "NUMERIC" => Ok(Self::Real),
            "TEXT" | "VARCHAR" | "CHAR" | "CLOB" => Ok(Self::Text),
            other => Err(DbError::Invalid(format!(
                "unsupported column type '{other}' (use TEXT, INTEGER or REAL)"
            ))),
        }
    }
}

/// A column definition supplied by the schema editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    /// Column name.
    pub name: String,
    /// Declared type.
    pub sql_type: SqlType,
}

/// A column as reported by SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// Column name.
    pub name: String,
    /// Declared type.
    pub sql_type: SqlType,
    /// Whether `NOT NULL` is declared.
    pub not_null: bool,
    /// Whether the column is part of the primary key.
    pub primary_key: bool,
}

/// One generated form field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// Column name.
    pub name: String,
    /// Declared type.
    pub sql_type: SqlType,
    /// Whether this column is a primary key.
    pub primary_key: bool,
    /// Current field value as text.
    pub value: String,
}

/// Parsed CSV input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CsvImport {
    /// Header names, empty when the file has no header row.
    pub headers: Vec<String>,
    /// Data rows.
    pub rows: Vec<Vec<String>>,
}

/// A reversible data edit used by the application-level undo stack.
#[derive(Debug, Clone, PartialEq)]
pub enum DataEdit {
    /// One cell changed in the grid.
    Cell {
        /// Table name.
        table: String,
        /// Row key (`rowid`).
        row_id: i64,
        /// Column name.
        column: String,
        /// Value before the edit.
        before: Option<String>,
        /// Value after the edit.
        after: Option<String>,
    },
    /// A new row was inserted.
    Insert {
        /// Table name.
        table: String,
        /// Columns written, in order.
        columns: Vec<String>,
        /// Values written, in order.
        values: Vec<Option<String>>,
        /// Generated row key.
        row_id: i64,
    },
    /// A row was deleted.
    Delete {
        /// Table name.
        table: String,
        /// Columns of the deleted row.
        columns: Vec<String>,
        /// Values of the deleted row.
        values: Vec<Option<String>>,
        /// Row key before deletion.
        row_id: i64,
    },
    /// Several edits performed as one user action, for example a form save.
    Batch(Vec<DataEdit>),
}

impl DataEdit {
    /// Short description for the status bar.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Cell { column, .. } => format!("edit {column}"),
            Self::Insert { table, .. } => format!("insert into {table}"),
            Self::Delete { table, .. } => format!("delete from {table}"),
            Self::Batch(edits) => format!("{} edits", edits.len()),
        }
    }

    /// Revert the edit.
    pub fn undo(&mut self, connection: &mut Connection) -> Result<(), DbError> {
        match self {
            Self::Cell {
                table,
                row_id,
                column,
                before,
                ..
            } => update_cell(connection, table, *row_id, column, before.as_deref()),
            Self::Insert { table, row_id, .. } => delete_row(connection, table, *row_id),
            Self::Delete {
                table,
                columns,
                values,
                row_id,
            } => {
                *row_id = insert_record(connection, table, columns, values)?;
                Ok(())
            }
            Self::Batch(edits) => {
                for edit in edits.iter_mut().rev() {
                    edit.undo(connection)?;
                }
                Ok(())
            }
        }
    }

    /// Re-apply the edit after an undo.
    pub fn redo(&mut self, connection: &mut Connection) -> Result<(), DbError> {
        match self {
            Self::Cell {
                table,
                row_id,
                column,
                after,
                ..
            } => update_cell(connection, table, *row_id, column, after.as_deref()),
            Self::Insert {
                table,
                columns,
                values,
                row_id,
            } => {
                *row_id = insert_record(connection, table, columns, values)?;
                Ok(())
            }
            Self::Delete { table, row_id, .. } => delete_row(connection, table, *row_id),
            Self::Batch(edits) => {
                for edit in edits.iter_mut() {
                    edit.redo(connection)?;
                }
                Ok(())
            }
        }
    }
}

/// Open an existing SQLite file read-write.
pub fn open_file(path: impl AsRef<Path>) -> Result<Connection, DbError> {
    Connection::open(path).map_err(DbError::from)
}

/// Build the in-memory sample database used by `--sample`.
pub fn create_memory_database() -> Result<Connection, DbError> {
    let connection = Connection::open_in_memory()?;
    populate_sample(&connection)?;
    Ok(connection)
}

/// Create the sample `people` table and fill it with a few rows.
pub fn populate_sample(connection: &Connection) -> Result<(), DbError> {
    create_people_table(connection)?;
    insert_person(connection, "Ada Lovelace", "ada@example.com")?;
    insert_person(connection, "Grace Hopper", "grace@example.com")?;
    insert_person(connection, "Alan Turing", "alan@example.com")?;
    Ok(())
}

/// Create the `people` demo table if it does not exist yet.
pub fn create_people_table(connection: &Connection) -> Result<(), DbError> {
    connection.execute(
        "CREATE TABLE IF NOT EXISTS people (\
         id INTEGER PRIMARY KEY, name TEXT, email TEXT)",
        [],
    )?;
    Ok(())
}

/// Insert one person and return the generated row id.
pub fn insert_person(connection: &Connection, name: &str, email: &str) -> Result<i64, DbError> {
    connection.execute(
        "INSERT INTO people (name, email) VALUES (?1, ?2)",
        params![name, email],
    )?;
    Ok(connection.last_insert_rowid())
}

/// List user tables, excluding SQLite's internal `sqlite_*` tables.
pub fn list_tables(connection: &Connection) -> Result<Vec<String>, DbError> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut tables = Vec::new();
    for row in rows {
        tables.push(row?);
    }
    Ok(tables)
}

/// Describe the columns of a table, in declaration order.
pub fn table_schema(connection: &Connection, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        let declaration: String = row.get(2)?;
        Ok(ColumnInfo {
            name: row.get(1)?,
            sql_type: SqlType::from_declaration(&declaration),
            not_null: row.get::<_, i64>(3)? != 0,
            primary_key: row.get::<_, i64>(5)? != 0,
        })
    })?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}

/// Build a safe `SELECT *` statement for a table, quoting its name.
#[must_use]
pub fn select_all_sql(table: &str) -> String {
    format!("SELECT * FROM {};", quote_identifier(table))
}

/// Build a safe `CREATE TABLE` statement, rejecting unsafe identifiers.
pub fn create_table_sql(name: &str, columns: &[(String, SqlType)]) -> Result<String, DbError> {
    if !is_valid_identifier(name) {
        return Err(DbError::InvalidName(name.to_owned()));
    }
    if columns.is_empty() {
        return Err(DbError::InvalidDefinition(
            "at least one column is required".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut definitions = Vec::with_capacity(columns.len());
    for (name, sql_type) in columns {
        if !is_valid_identifier(name) {
            return Err(DbError::InvalidName(name.clone()));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(DbError::InvalidDefinition(format!(
                "duplicate column '{name}'"
            )));
        }
        definitions.push(format!("{} {}", quote_identifier(name), sql_type.as_sql()));
    }
    Ok(format!(
        "CREATE TABLE {} ({})",
        quote_identifier(name),
        definitions.join(", ")
    ))
}

/// Build a safe `ALTER TABLE ... ADD COLUMN` statement.
pub fn add_column_sql(table: &str, column: &ColumnDef) -> Result<String, DbError> {
    if !is_valid_identifier(&column.name) {
        return Err(DbError::InvalidName(column.name.clone()));
    }
    Ok(format!(
        "ALTER TABLE {} ADD COLUMN {} {}",
        quote_identifier(table),
        quote_identifier(&column.name),
        column.sql_type.as_sql()
    ))
}

/// Build a safe `DROP TABLE` statement.
#[must_use]
pub fn drop_table_sql(table: &str) -> String {
    format!("DROP TABLE {}", quote_identifier(table))
}

/// Parse `name:TYPE` (or `name TYPE`) lines from the schema editor.
pub fn parse_column_defs(input: &str) -> Result<Vec<ColumnDef>, DbError> {
    let mut columns = Vec::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (name, declaration) = match line.split_once(':') {
            Some(parts) => parts,
            None => {
                let mut parts = line.split_whitespace();
                (parts.next().unwrap_or(""), parts.next().unwrap_or(""))
            }
        };
        columns.push(ColumnDef {
            name: name.trim().to_owned(),
            sql_type: declaration.trim().parse()?,
        });
    }
    if columns.is_empty() {
        return Err(DbError::InvalidDefinition(
            "at least one column is required".into(),
        ));
    }
    Ok(columns)
}

/// Create a table from the schema editor definitions.
pub fn create_table(
    connection: &Connection,
    name: &str,
    columns: &[(String, SqlType)],
) -> Result<(), DbError> {
    let sql = create_table_sql(name, columns)?;
    connection.execute_batch(&sql)?;
    Ok(())
}

/// Add a column to an existing table.
pub fn add_column(
    connection: &Connection,
    table: &str,
    name: &str,
    sql_type: SqlType,
) -> Result<(), DbError> {
    let sql = add_column_sql(
        table,
        &ColumnDef {
            name: name.to_owned(),
            sql_type,
        },
    )?;
    connection.execute_batch(&sql)?;
    Ok(())
}

/// Drop a table. Callers must ask the user for confirmation first.
pub fn drop_table(connection: &Connection, table: &str) -> Result<(), DbError> {
    connection.execute_batch(&drop_table_sql(table))?;
    Ok(())
}

/// Read a table with its `rowid`, applying an optional case-insensitive filter.
///
/// The limit is bounded by the caller and passed as a bound SQL parameter.
pub fn table_rows(
    connection: &Connection,
    table: &str,
    search: Option<&str>,
    limit: u32,
) -> Result<TableRows, DbError> {
    let columns = table_schema(connection, table)?;
    let mut values: Vec<String> = Vec::new();
    let mut conditions = Vec::new();
    if let Some(needle) = search.map(str::trim).filter(|value| !value.is_empty()) {
        let pattern = format!("%{needle}%");
        for column in &columns {
            conditions.push(format!(
                "CAST({} AS TEXT) LIKE ?{}",
                quote_identifier(&column.name),
                values.len() + 1
            ));
            values.push(pattern.clone());
        }
    }
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" OR "))
    };
    values.push(limit.max(1).to_string());
    let sql = format!(
        "SELECT rowid, * FROM {}{} LIMIT ?{}",
        quote_identifier(table),
        where_clause,
        values.len()
    );

    let mut statement = connection.prepare(&sql)?;
    let names = statement
        .column_names()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut cursor = statement.query(params_from_iter(values.iter().map(String::as_str)))?;
    let mut result = QueryResult::default();
    let mut row_ids = Vec::new();
    while let Some(row) = cursor.next()? {
        row_ids.push(row.get::<_, i64>(0)?);
        let mut cells = Vec::with_capacity(names.len().saturating_sub(1));
        for index in 1..names.len() {
            cells.push(cell_text(row.get_ref(index)?));
        }
        result.rows.push(cells);
    }
    if let Some(visible) = names.get(1..) {
        result.columns = visible.to_vec();
    }
    Ok(TableRows { result, row_ids })
}

/// Run a read-only SQL query and collect its result.
///
/// Destructive statements are rejected before reaching SQLite so that the
/// query box cannot modify the opened database by accident.
pub fn run_query(connection: &Connection, sql: &str) -> Result<QueryResult, DbError> {
    if !is_read_only(sql) {
        return Err(DbError::NotAQuery);
    }
    let mut statement = connection.prepare(sql)?;
    collect_rows(&mut statement, [])
}

/// Return the values of one row, or `None` when it no longer exists.
pub fn record_values(
    connection: &Connection,
    table: &str,
    row_id: i64,
) -> Result<Option<Vec<Option<String>>>, DbError> {
    let sql = format!("SELECT * FROM {} WHERE rowid = ?1", quote_identifier(table));
    let mut statement = connection.prepare(&sql)?;
    let width = statement.column_count();
    let mut cursor = statement.query(params![row_id])?;
    let Some(row) = cursor.next()? else {
        return Ok(None);
    };
    let mut values = Vec::with_capacity(width);
    for index in 0..width {
        values.push(cell_text(row.get_ref(index)?));
    }
    Ok(Some(values))
}

/// Update one cell, identified by its `rowid`, in one transaction.
pub fn update_cell(
    connection: &mut Connection,
    table: &str,
    row_id: i64,
    column: &str,
    value: Option<&str>,
) -> Result<(), DbError> {
    if !is_valid_identifier(column) {
        return Err(DbError::InvalidName(column.to_owned()));
    }
    let sql = format!(
        "UPDATE {} SET {} = ?1 WHERE rowid = ?2",
        quote_identifier(table),
        quote_identifier(column)
    );
    let transaction = connection.transaction()?;
    transaction.execute(&sql, params![value, row_id])?;
    transaction.commit()?;
    Ok(())
}

/// Insert a row whose values follow `table_schema` order.
pub fn insert_record(
    connection: &mut Connection,
    table: &str,
    columns: &[String],
    values: &[Option<String>],
) -> Result<i64, DbError> {
    if columns.is_empty() || columns.len() != values.len() {
        return Err(DbError::InvalidDefinition(format!(
            "expected {} columns for {} values",
            columns.len(),
            values.len()
        )));
    }
    let names = columns
        .iter()
        .map(|name| quote_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_identifier(table),
        names,
        placeholders
    );
    let bound: Vec<Value> = values
        .iter()
        .map(|value| {
            value
                .as_deref()
                .map_or(Value::Null, |text| Value::Text(text.to_owned()))
        })
        .collect();
    let transaction = connection.transaction()?;
    transaction.execute(&sql, params_from_iter(bound))?;
    let row_id = transaction.last_insert_rowid();
    transaction.commit()?;
    Ok(row_id)
}

/// Update every column of one row in one transaction.
pub fn update_record(
    connection: &mut Connection,
    table: &str,
    row_id: i64,
    values: &[Option<String>],
) -> Result<(), DbError> {
    let columns = table_schema(connection, table)?;
    if columns.len() != values.len() {
        return Err(DbError::InvalidDefinition(format!(
            "table '{}' has {} columns, got {} values",
            table,
            columns.len(),
            values.len()
        )));
    }
    let assignments = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{} = ?{}", quote_identifier(&column.name), index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE {} SET {} WHERE rowid = ?{}",
        quote_identifier(table),
        assignments,
        columns.len() + 1
    );
    let mut bound: Vec<Value> = values
        .iter()
        .map(|value| {
            value
                .as_deref()
                .map_or(Value::Null, |text| Value::Text(text.to_owned()))
        })
        .collect();
    bound.push(Value::Integer(row_id));
    let transaction = connection.transaction()?;
    transaction.execute(&sql, params_from_iter(bound))?;
    transaction.commit()?;
    Ok(())
}

/// Delete one row by `rowid` in one transaction.
pub fn delete_row(connection: &mut Connection, table: &str, row_id: i64) -> Result<(), DbError> {
    let sql = format!("DELETE FROM {} WHERE rowid = ?1", quote_identifier(table));
    let transaction = connection.transaction()?;
    transaction.execute(&sql, params![row_id])?;
    transaction.commit()?;
    Ok(())
}

/// Build form fields from a table schema and, optionally, an existing row.
#[must_use]
pub fn build_form_fields(
    columns: &[ColumnInfo],
    values: Option<&[Option<String>]>,
) -> Vec<FormField> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| FormField {
            name: column.name.clone(),
            sql_type: column.sql_type,
            primary_key: column.primary_key,
            value: values
                .and_then(|row| row.get(index))
                .and_then(Option::clone)
                .unwrap_or_default(),
        })
        .collect()
}

/// Convert a form or CSV text value to a typed SQLite value.
pub fn coerce_value(value: Option<&str>, sql_type: SqlType) -> Result<Value, DbError> {
    match value.map(str::trim) {
        None | Some("") => Ok(Value::Null),
        Some(text) => match sql_type {
            SqlType::Integer => text
                .parse::<i64>()
                .map(Value::Integer)
                .map_err(|_| DbError::Invalid(format!("'{text}' is not an integer"))),
            SqlType::Real => text
                .parse::<f64>()
                .map(Value::Real)
                .map_err(|_| DbError::Invalid(format!("'{text}' is not a number"))),
            SqlType::Text => Ok(Value::Text(text.to_owned())),
        },
    }
}

/// Parse CSV text, optionally treating the first record as a header.
pub fn parse_csv(input: &str, has_headers: bool) -> Result<CsvImport, DbError> {
    let mut reader = ReaderBuilder::new()
        .has_headers(false)
        .flexible(false)
        .from_reader(input.as_bytes());
    let mut records: Vec<StringRecord> = Vec::new();
    for record in reader.records() {
        records.push(record.map_err(|error| DbError::Csv(error.to_string()))?);
    }
    let Some(first) = records.first() else {
        return Err(DbError::Csv("the file is empty".into()));
    };
    let width = first.len();
    if records.iter().any(|record| record.len() != width) {
        return Err(DbError::Csv(format!(
            "expected {width} fields in every row"
        )));
    }
    let to_strings = |record: &StringRecord| record.iter().map(str::to_owned).collect::<Vec<_>>();
    let (headers, rows) = if has_headers {
        (
            to_strings(first),
            records.iter().skip(1).map(to_strings).collect::<Vec<_>>(),
        )
    } else {
        (
            Vec::new(),
            records.iter().map(to_strings).collect::<Vec<_>>(),
        )
    };
    Ok(CsvImport { headers, rows })
}

/// Import CSV rows into an existing table inside a single transaction.
///
/// Header names are matched case-insensitively. Without a header, records are
/// mapped positionally to the table columns. Empty fields become SQL `NULL`.
pub fn import_csv(
    connection: &mut Connection,
    table: &str,
    data: &CsvImport,
) -> Result<usize, DbError> {
    let columns = table_schema(connection, table)?;
    let mapping: Vec<usize> = if data.headers.is_empty() {
        if data
            .rows
            .first()
            .is_some_and(|row| row.len() != columns.len())
        {
            return Err(DbError::Csv(format!(
                "expected {} fields, found {}",
                columns.len(),
                data.rows.first().map_or(0, Vec::len)
            )));
        }
        (0..columns.len()).collect()
    } else {
        data.headers
            .iter()
            .map(|header| {
                columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(header))
                    .ok_or_else(|| {
                        DbError::Csv(format!("unknown column '{header}' in table '{table}'"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if data.rows.iter().any(|row| row.len() != mapping.len()) {
        return Err(DbError::Csv(format!(
            "expected {} fields in every row",
            mapping.len()
        )));
    }

    let names = columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_identifier(table),
        names
            .iter()
            .map(|name| quote_identifier(name))
            .collect::<Vec<_>>()
            .join(", "),
        placeholders
    );

    let transaction = connection.transaction()?;
    for (row_number, row) in data.rows.iter().enumerate() {
        let mut bound = vec![Value::Null; columns.len()];
        for (field_index, text) in row.iter().enumerate() {
            let column_index = mapping[field_index];
            bound[column_index] = coerce_value(Some(text), columns[column_index].sql_type)
                .map_err(|error| {
                    DbError::Csv(format!(
                        "row {}, column '{}': {error}",
                        row_number + 1,
                        columns[column_index].name
                    ))
                })?;
        }
        transaction.execute(&sql, params_from_iter(bound))?;
    }
    transaction.commit()?;
    Ok(data.rows.len())
}

/// Render a query result as RFC 4180-style CSV, with a header row.
#[must_use]
pub fn export_csv(result: &QueryResult) -> String {
    let mut output = String::new();
    for (index, column) in result.columns.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(&csv_field(column));
    }
    output.push('\n');
    for row in &result.rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            if let Some(value) = value {
                output.push_str(&csv_field(value));
            }
        }
        output.push('\n');
    }
    output
}

fn collect_rows(
    statement: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<QueryResult, DbError> {
    let columns = statement
        .column_names()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let width = columns.len();
    let mut cursor = statement.query(params)?;
    let mut result = QueryResult {
        columns,
        rows: Vec::new(),
    };
    while let Some(row) = cursor.next()? {
        let mut values = Vec::with_capacity(width);
        for index in 0..width {
            values.push(cell_text(row.get_ref(index)?));
        }
        result.rows.push(values);
    }
    Ok(result)
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn is_valid_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if name.len() > 64 || name.starts_with("sqlite_") {
        return false;
    }
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn cell_text(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Null => None,
        ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Integer(number) => Some(number.to_string()),
        ValueRef::Real(number) => Some(number.to_string()),
        ValueRef::Blob(bytes) => Some(format!("<{} bytes>", bytes.len())),
    }
}

fn is_read_only(sql: &str) -> bool {
    let keyword = first_keyword(sql);
    matches!(
        keyword.to_ascii_uppercase().as_str(),
        "SELECT" | "WITH" | "PRAGMA" | "EXPLAIN" | "VALUES"
    )
}

/// Return the first SQL keyword, skipping whitespace and SQL comments.
fn first_keyword(sql: &str) -> &str {
    let bytes = sql.as_bytes();
    let mut index = 0;
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index + 1 < bytes.len() && bytes[index] == b'-' && bytes[index + 1] == b'-' {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
            continue;
        }
        break;
    }
    let rest = &sql[index..];
    let end = rest
        .find(|character: char| !character.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    &rest[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        Connection::open_in_memory().expect("in-memory SQLite must open")
    }

    fn people_columns() -> Vec<(String, SqlType)> {
        vec![
            ("id".to_owned(), SqlType::Integer),
            ("name".to_owned(), SqlType::Text),
            ("email".to_owned(), SqlType::Text),
        ]
    }

    #[test]
    fn creates_alters_and_drops_schema() {
        let connection = memory();
        create_table(&connection, "people", &people_columns()).expect("create table");
        let schema = table_schema(&connection, "people").expect("schema");
        assert_eq!(schema.len(), 3);
        assert_eq!(schema[0].name, "id");
        assert_eq!(schema[1].sql_type, SqlType::Text);

        add_column(&connection, "people", "active", SqlType::Integer).expect("add column");
        let schema = table_schema(&connection, "people").expect("schema after alter");
        assert_eq!(
            schema.last().map(|column| column.name.as_str()),
            Some("active")
        );
        assert_eq!(
            schema.last().map(|column| column.sql_type),
            Some(SqlType::Integer)
        );

        drop_table(&connection, "people").expect("drop table");
        assert!(list_tables(&connection).expect("tables").is_empty());
    }

    #[test]
    fn sql_builders_escape_and_reject_unsafe_input() {
        assert_eq!(
            create_table_sql("people", &people_columns()).expect("sql"),
            "CREATE TABLE \"people\" (\"id\" INTEGER, \"name\" TEXT, \"email\" TEXT)"
        );
        assert_eq!(
            add_column_sql(
                "people",
                &ColumnDef {
                    name: "phone".into(),
                    sql_type: SqlType::Text,
                }
            )
            .expect("sql"),
            "ALTER TABLE \"people\" ADD COLUMN \"phone\" TEXT"
        );
        assert_eq!(drop_table_sql("people"), "DROP TABLE \"people\"");
        assert_eq!(
            select_all_sql("odd\"name"),
            "SELECT * FROM \"odd\"\"name\";"
        );

        assert!(create_table_sql("people\"; DROP TABLE people; --", &people_columns()).is_err());
        assert!(create_table_sql("people", &[]).is_err());
        assert!(create_table_sql("1people", &people_columns()).is_err());
        assert!(create_table_sql("sqlite_people", &people_columns()).is_err());
        assert!(
            add_column_sql(
                "people",
                &ColumnDef {
                    name: "bad name".into(),
                    sql_type: SqlType::Text,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn parses_column_definitions() {
        let columns = parse_column_defs("id:INTEGER\nname:TEXT\nscore real").expect("defs");
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0].sql_type, SqlType::Integer);
        assert_eq!(columns[2].name, "score");
        assert_eq!(columns[2].sql_type, SqlType::Real);
        assert!(parse_column_defs("   ").is_err());
        assert!(parse_column_defs("name:BOGUS").is_err());
    }

    #[test]
    fn crud_operations_and_undo() {
        let mut connection = memory();
        create_table(&connection, "people", &people_columns()).expect("create table");
        let names = vec!["id".to_owned(), "name".to_owned(), "email".to_owned()];
        let first = insert_record(
            &mut connection,
            "people",
            &names,
            &[None, Some("Ada".into()), Some("ada@example.com".into())],
        )
        .expect("insert");
        let second = insert_record(
            &mut connection,
            "people",
            &names,
            &[None, Some("Grace".into()), Some("grace@example.com".into())],
        )
        .expect("insert");

        let grid = table_rows(&connection, "people", None, 100).expect("grid");
        assert_eq!(grid.result.columns, names);
        assert_eq!(grid.result.rows.len(), 2);
        assert_eq!(grid.row_ids, vec![first, second]);

        let mut cell = DataEdit::Cell {
            table: "people".into(),
            row_id: first,
            column: "name".into(),
            before: Some("Ada".into()),
            after: Some("Ada Lovelace".into()),
        };
        cell.redo(&mut connection).expect("apply cell edit");
        assert_eq!(
            record_values(&connection, "people", first)
                .expect("row")
                .expect("existing")[1]
                .as_deref(),
            Some("Ada Lovelace")
        );
        cell.undo(&mut connection).expect("undo cell edit");
        assert_eq!(
            record_values(&connection, "people", first)
                .expect("row")
                .expect("existing")[1]
                .as_deref(),
            Some("Ada")
        );

        let mut insert = DataEdit::Insert {
            table: "people".into(),
            columns: names.clone(),
            values: vec![None, Some("Lin".into()), Some("lin@example.com".into())],
            row_id: 0,
        };
        insert.redo(&mut connection).expect("apply insert");
        assert_eq!(
            table_rows(&connection, "people", None, 100)
                .expect("grid")
                .result
                .rows
                .len(),
            3
        );
        insert.undo(&mut connection).expect("undo insert");
        assert_eq!(
            table_rows(&connection, "people", None, 100)
                .expect("grid")
                .result
                .rows
                .len(),
            2
        );

        update_record(
            &mut connection,
            "people",
            second,
            &[
                None,
                Some("Grace H.".into()),
                Some("grace@example.com".into()),
            ],
        )
        .expect("update record");
        assert_eq!(
            record_values(&connection, "people", second)
                .expect("row")
                .expect("existing")[1]
                .as_deref(),
            Some("Grace H.")
        );

        delete_row(&mut connection, "people", second).expect("delete row");
        assert!(
            record_values(&connection, "people", second)
                .expect("row")
                .is_none()
        );
        assert!(update_cell(&mut connection, "people", first, "bad name", None).is_err());
    }

    #[test]
    fn batch_edit_is_one_undo_step() {
        let mut connection = memory();
        create_table(&connection, "people", &people_columns()).expect("create table");
        let names = vec!["id".to_owned(), "name".to_owned(), "email".to_owned()];
        let first = insert_record(
            &mut connection,
            "people",
            &names,
            &[None, Some("Ada".into()), Some("ada@example.com".into())],
        )
        .expect("insert");
        let mut batch = DataEdit::Batch(vec![DataEdit::Cell {
            table: "people".into(),
            row_id: first,
            column: "name".into(),
            before: Some("Ada".into()),
            after: Some("Ada L.".into()),
        }]);
        batch.redo(&mut connection).expect("apply batch");
        assert_eq!(
            record_values(&connection, "people", first)
                .expect("row")
                .expect("existing")[1]
                .as_deref(),
            Some("Ada L.")
        );
        batch.undo(&mut connection).expect("undo batch");
        assert_eq!(
            record_values(&connection, "people", first)
                .expect("row")
                .expect("existing")[1]
                .as_deref(),
            Some("Ada")
        );
        assert_eq!(batch.label(), "1 edits");
    }

    #[test]
    fn filters_limits_and_builds_forms() {
        let mut connection = memory();
        create_table(&connection, "people", &people_columns()).expect("create table");
        let names = vec!["id".to_owned(), "name".to_owned(), "email".to_owned()];
        for (name, email) in [("Ada", "ada@example.com"), ("Grace", "grace@example.com")] {
            insert_record(
                &mut connection,
                "people",
                &names,
                &[None, Some(name.into()), Some(email.into())],
            )
            .expect("insert");
        }
        let filtered = table_rows(&connection, "people", Some("grace"), 100).expect("filter");
        assert_eq!(filtered.result.rows.len(), 1);
        assert_eq!(filtered.result.rows[0][1].as_deref(), Some("Grace"));
        let limited = table_rows(&connection, "people", None, 1).expect("limit");
        assert_eq!(limited.result.rows.len(), 1);

        let schema = table_schema(&connection, "people").expect("schema");
        let fields = build_form_fields(&schema, Some(&limited.result.rows[0]));
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[1].name, "name");
        assert!(!fields[1].value.is_empty());
        let empty = build_form_fields(&schema, None);
        assert!(empty.iter().all(|field| field.value.is_empty()));
    }

    #[test]
    fn coerces_form_values_to_sqlite_types() {
        assert_eq!(
            coerce_value(None, SqlType::Integer).expect("null"),
            Value::Null
        );
        assert_eq!(
            coerce_value(Some(" 42 "), SqlType::Integer).expect("integer"),
            Value::Integer(42)
        );
        assert_eq!(
            coerce_value(Some("2.5"), SqlType::Real).expect("real"),
            Value::Real(2.5)
        );
        assert_eq!(
            coerce_value(Some("Ada"), SqlType::Text).expect("text"),
            Value::Text("Ada".into())
        );
        assert!(coerce_value(Some("nope"), SqlType::Integer).is_err());
        assert!(coerce_value(Some("nope"), SqlType::Real).is_err());
    }

    #[test]
    fn imports_csv_with_headers_and_rolls_back_on_error() {
        let mut connection = memory();
        create_table(
            &connection,
            "scores",
            &[
                ("label".to_owned(), SqlType::Text),
                ("points".to_owned(), SqlType::Integer),
            ],
        )
        .expect("create table");
        let data = parse_csv("Label,Points\nA,10\nB,20\n", true).expect("parse csv");
        assert_eq!(data.headers, vec!["Label", "Points"]);
        let count = import_csv(&mut connection, "scores", &data).expect("import");
        assert_eq!(count, 2);
        let grid = table_rows(&connection, "scores", None, 100).expect("grid");
        assert_eq!(grid.result.rows[0][1].as_deref(), Some("10"));

        let invalid = parse_csv("label,points\nC,not-a-number\n", true).expect("parse csv");
        assert!(import_csv(&mut connection, "scores", &invalid).is_err());
        assert_eq!(
            table_rows(&connection, "scores", None, 100)
                .expect("grid")
                .result
                .rows
                .len(),
            2
        );

        let unknown = parse_csv("wrong,points\nC,3\n", true).expect("parse csv");
        assert!(import_csv(&mut connection, "scores", &unknown).is_err());
        let short = parse_csv("label,points\nC\n", true).expect_err("short row");
        assert!(short.to_string().contains("field") || short.to_string().contains("record"));
    }

    #[test]
    fn imports_csv_without_headers() {
        let mut connection = memory();
        create_table(&connection, "items", &[("name".to_owned(), SqlType::Text)]).expect("create");
        let data = parse_csv("A\nB\n", false).expect("parse csv");
        assert!(data.headers.is_empty());
        assert_eq!(
            import_csv(&mut connection, "items", &data).expect("import"),
            2
        );
        assert_eq!(
            table_rows(&connection, "items", None, 100)
                .expect("grid")
                .result
                .rows
                .len(),
            2
        );
        assert!(parse_csv("", false).is_err());
    }

    #[test]
    fn keeps_read_only_query_guard_and_csv_export() {
        let connection = create_memory_database().expect("sample database");
        assert!(matches!(
            run_query(&connection, "DROP TABLE people"),
            Err(DbError::NotAQuery)
        ));
        let result = run_query(
            &connection,
            "-- comment\nSELECT name FROM people WHERE id = 2",
        )
        .expect("select");
        assert_eq!(result.rows, vec![vec![Some("Grace Hopper".to_owned())]]);

        let exported = export_csv(&QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![
                vec![Some("x,y".into()), None],
                vec![Some("a\"b".into()), Some("1".into())],
            ],
        });
        assert_eq!(exported, "a,b\n\"x,y\",\n\"a\"\"b\",1\n");
        assert_eq!(
            export_csv(&QueryResult {
                columns: vec!["a".into()],
                rows: Vec::new(),
            }),
            "a\n"
        );
    }
}
