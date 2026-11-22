//! Lightweight FormCraft document.
//!
//! The project file stores the title, the SQLite database path, the last SQL
//! query and a short history of recent queries. Database connections, result
//! grids and edit state are live session state and are deliberately not
//! persisted.

use serde::{Deserialize, Serialize};

/// Maximum number of recent queries kept in a project file.
pub const MAX_QUERY_HISTORY: usize = 20;

/// The application document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Document title shown in the window and project files.
    pub title: String,
    /// Path of the SQLite database associated with this project, if any.
    pub database_path: Option<String>,
    /// Last SQL query entered by the user.
    pub last_query: String,
    /// Recent SQL queries, oldest first.
    ///
    /// Schema v1 projects do not have this field; `serde(default)` keeps them
    /// readable during the migration to schema v2.
    #[serde(default)]
    pub query_history: Vec<String>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            title: "Untitled".into(),
            database_path: None,
            last_query: String::new(),
            query_history: Vec::new(),
        }
    }
}

impl Document {
    /// Built-in demonstration document.
    ///
    /// It references no file: the sample database is an in-memory SQLite
    /// session created by the application.
    #[must_use]
    pub fn sample() -> Self {
        Self {
            title: "FormCraft sample".into(),
            database_path: None,
            last_query: "SELECT id, name, email FROM people ORDER BY id;".into(),
            query_history: Vec::new(),
        }
    }

    /// Remember a successfully executed query, keeping the history bounded.
    ///
    /// An empty or duplicate query is ignored; repeating a query moves it to
    /// the end so the most recent entry is always last.
    pub fn remember_query(&mut self, sql: &str) {
        let query = sql.trim();
        if query.is_empty() {
            return;
        }
        self.query_history.retain(|previous| previous != query);
        self.query_history.push(query.to_owned());
        while self.query_history.len() > MAX_QUERY_HISTORY {
            self.query_history.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use craftkit_core::ProjectEnvelope;

    use super::{Document, MAX_QUERY_HISTORY};

    #[test]
    fn json_round_trip() {
        let document = Document {
            title: "People".into(),
            database_path: Some("data/people.sqlite".into()),
            last_query: "SELECT * FROM people;".into(),
            query_history: vec!["SELECT 1;".into(), "SELECT * FROM people;".into()],
        };
        let json = serde_json::to_string(&document).expect("serialise document");
        let restored: Document = serde_json::from_str(&json).expect("deserialise document");
        assert_eq!(document, restored);
    }

    #[test]
    fn default_document_has_no_database_and_no_query() {
        let document = Document::default();
        assert_eq!(document.title, "Untitled");
        assert_eq!(document.database_path, None);
        assert!(document.last_query.is_empty());
        assert!(document.query_history.is_empty());
    }

    #[test]
    fn sample_document_uses_an_in_memory_database() {
        let document = Document::sample();
        assert_eq!(document.database_path, None);
        assert!(document.last_query.contains("people"));
    }

    #[test]
    fn remembers_queries_without_duplicates_and_bounded() {
        let mut document = Document::default();
        document.remember_query("SELECT 1;");
        document.remember_query("SELECT 2;");
        document.remember_query("SELECT 1;");
        assert_eq!(document.query_history, vec!["SELECT 2;", "SELECT 1;"]);
        document.remember_query("  ");
        assert_eq!(document.query_history.len(), 2);
        for index in 0..MAX_QUERY_HISTORY + 5 {
            document.remember_query(&format!("SELECT {index};"));
        }
        assert_eq!(document.query_history.len(), MAX_QUERY_HISTORY);
        assert_eq!(
            document.query_history.last().map(String::as_str),
            Some(format!("SELECT {};", MAX_QUERY_HISTORY + 4).as_str())
        );
    }

    #[test]
    fn migrates_schema_v1_project_without_query_history() {
        let json = r#"{
            "format": "formcraft",
            "schema_version": 1,
            "app_version": "0.1.0",
            "document": {
                "title": "Legacy",
                "database_path": null,
                "last_query": "SELECT 1;"
            }
        }"#;
        let envelope: ProjectEnvelope<Document> =
            serde_json::from_str(json).expect("deserialise v1 project");
        assert_eq!(envelope.format, "formcraft");
        envelope
            .validate("formcraft", 2)
            .expect("older schema is accepted");
        assert_eq!(envelope.document.title, "Legacy");
        assert!(envelope.document.query_history.is_empty());
    }

    #[test]
    fn rejects_a_future_schema() {
        let envelope = ProjectEnvelope::new("formcraft", 3, "0.2.0", Document::default());
        assert!(envelope.validate("formcraft", 2).is_err());
    }
}
