//! Lightweight FormCraft document.
//!
//! The project file stores only the user's title, the SQLite database path and
//! the last SQL query. Database connections and result grids are live session
//! state and are deliberately not persisted.

use serde::{Deserialize, Serialize};

/// The application document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Document title shown in the window and project files.
    pub title: String,
    /// Path of the SQLite database associated with this project, if any.
    pub database_path: Option<String>,
    /// Last SQL query entered by the user.
    pub last_query: String,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            title: "Untitled".into(),
            database_path: None,
            last_query: String::new(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Document;

    #[test]
    fn json_round_trip() {
        let document = Document {
            title: "People".into(),
            database_path: Some("data/people.sqlite".into()),
            last_query: "SELECT * FROM people;".into(),
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
    }

    #[test]
    fn sample_document_uses_an_in_memory_database() {
        let document = Document::sample();
        assert_eq!(document.database_path, None);
        assert!(document.last_query.contains("people"));
    }
}
