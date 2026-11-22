//! SQLite access layer for FormCraft.
//!
//! Everything in this module is UI-free and testable on temporary databases.
//! User-visible SQLite failures are returned as [`DbError`] and handled by the
//! caller; this module never panics on external input.

use std::fmt;
use std::path::Path;

use rusqlite::Connection;
use rusqlite::params;
use rusqlite::types::ValueRef;

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

/// Errors produced by the database layer.
#[derive(Debug)]
pub enum DbError {
    /// SQLite reported an error.
    Sqlite(rusqlite::Error),
    /// The statement was not a read-only query.
    NotAQuery,
}

impl fmt::Display for DbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::NotAQuery => formatter.write_str(
                "Only read-only queries are allowed (SELECT, WITH, PRAGMA, EXPLAIN, VALUES)",
            ),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::NotAQuery => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
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

/// Build a `SELECT *` statement for a table, quoting its name as an identifier.
#[must_use]
pub fn select_all_sql(table: &str) -> String {
    format!("SELECT * FROM {};", quote_identifier(table))
}

/// Select every row of a table, quoting its name as an SQL identifier.
pub fn query_table(connection: &Connection, table: &str) -> Result<QueryResult, DbError> {
    run_query(connection, &select_all_sql(table))
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
    let columns = statement
        .column_names()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let width = columns.len();
    let mut cursor = statement.query([])?;
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

/// Render a query result as RFC 4180-style CSV, with a header row.
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

    #[test]
    fn creates_table_and_reads_inserted_rows() {
        let connection = memory();
        create_people_table(&connection).expect("create table");
        assert_eq!(
            insert_person(&connection, "Ada Lovelace", "ada@example.com").expect("insert first"),
            1
        );
        assert_eq!(
            insert_person(&connection, "Grace Hopper", "grace@example.com").expect("insert second"),
            2
        );

        let result = query_table(&connection, "people").expect("select people");
        assert_eq!(result.columns, vec!["id", "name", "email"]);
        assert_eq!(result.row_count(), 2);
        assert_eq!(result.rows[0][1].as_deref(), Some("Ada Lovelace"));
        assert_eq!(result.rows[1][2].as_deref(), Some("grace@example.com"));
    }

    #[test]
    fn creates_table_in_temporary_file_database() {
        let mut path = std::env::temp_dir();
        path.push(format!("formcraft-test-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);

        {
            let connection = open_file(&path).expect("open temporary database");
            create_people_table(&connection).expect("create table");
            insert_person(&connection, "Temp Person", "temp@example.com").expect("insert");
            let result = query_table(&connection, "people").expect("select people");
            assert_eq!(result.row_count(), 1);
            assert_eq!(result.rows[0][1].as_deref(), Some("Temp Person"));
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runs_select_query_with_predicate() {
        let connection = create_memory_database().expect("sample database");
        let result = run_query(&connection, "SELECT name FROM people WHERE id = 2")
            .expect("select with predicate");
        assert_eq!(result.columns, vec!["name"]);
        assert_eq!(result.rows, vec![vec![Some("Grace Hopper".to_owned())]]);

        let filtered = run_query(&connection, "SELECT id FROM people WHERE name = 'Nobody'")
            .expect("select without match");
        assert_eq!(filtered.row_count(), 0);
    }

    #[test]
    fn lists_only_user_tables() {
        let connection = create_memory_database().expect("sample database");
        let tables = list_tables(&connection).expect("list tables");
        assert_eq!(tables, vec!["people"]);
    }

    #[test]
    fn rejects_non_read_only_statements() {
        let connection = create_memory_database().expect("sample database");
        assert!(matches!(
            run_query(&connection, "DROP TABLE people"),
            Err(DbError::NotAQuery)
        ));
        assert!(matches!(
            run_query(&connection, "INSERT INTO people (name) VALUES ('x')"),
            Err(DbError::NotAQuery)
        ));
        assert!(run_query(&connection, "   ").is_err());
        let tables = list_tables(&connection).expect("list tables");
        assert_eq!(tables, vec!["people"]);
    }

    #[test]
    fn allows_comments_before_select() {
        let connection = create_memory_database().expect("sample database");
        let result = run_query(
            &connection,
            "-- count people\nSELECT COUNT(*) AS total FROM people",
        )
        .expect("select after comment");
        assert_eq!(result.columns, vec!["total"]);
        assert_eq!(result.rows[0][0].as_deref(), Some("3"));

        let result = run_query(
            &connection,
            "/* block */ SELECT name FROM people ORDER BY id LIMIT 1",
        )
        .expect("select after block comment");
        assert_eq!(result.rows[0][0].as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn exports_csv_with_escaping_and_nulls() {
        let result = QueryResult {
            columns: vec!["id".into(), "name".into(), "email".into()],
            rows: vec![
                vec![
                    Some("1".into()),
                    Some("Ada, \"A\"".into()),
                    Some("ada@example.com".into()),
                ],
                vec![Some("2".into()), Some("Line1\nLine2".into()), None],
            ],
        };
        let expected = "id,name,email\n1,\"Ada, \"\"A\"\"\",ada@example.com\n2,\"Line1\nLine2\",\n";
        assert_eq!(export_csv(&result), expected);
    }

    #[test]
    fn select_all_sql_quotes_identifiers() {
        assert_eq!(select_all_sql("people"), "SELECT * FROM \"people\";");
        assert_eq!(
            select_all_sql("odd\"name"),
            "SELECT * FROM \"odd\"\"name\";"
        );
    }

    #[test]
    fn empty_result_exports_header_only() {
        let result = QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: Vec::new(),
        };
        assert_eq!(export_csv(&result), "a,b\n");
    }
}
