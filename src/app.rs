//! FormCraft UI, data editing and project lifecycle.

use std::path::PathBuf;

use craftkit_io::DocumentHost;
use craftkit_ui::cli::CliOptions;
use craftkit_ui::eframe::{self, egui};
use craftkit_ui::egui::{Align, Layout, RichText};
use craftkit_ui::{AppIdentity, AppShell, Tone, dialogs, theme, widgets};
use rusqlite::Connection;

use crate::database::{self, ColumnInfo, DataEdit, SqlType, TableRows};
use crate::document::Document;

const FORMAT: &str = "formcraft";
const SCHEMA_VERSION: u32 = 2;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_VISIBLE_ROWS: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    New,
    Open,
    OpenDatabase,
    Save,
    SaveAs,
    Undo,
    Redo,
    RunQuery,
    ExportCsv,
    ImportCsv,
    CreateTable,
    AddColumn,
    DropTable,
    ShowSchema,
    About,
    Settings,
}

#[derive(Debug, Clone)]
enum PendingAction {
    New,
    Open(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Grid,
    Form,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaKind {
    CreateTable,
    AddColumn,
}

#[derive(Debug, Clone)]
struct SchemaDialog {
    kind: SchemaKind,
    table_name: String,
    columns_text: String,
    column_name: String,
    column_type: SqlType,
}

#[derive(Debug, Clone)]
enum Confirmation {
    DeleteRow { row_id: i64 },
    DropTable { table: String },
}

#[derive(Debug, Clone)]
struct CellEdit {
    row: usize,
    column: usize,
    text: String,
}

#[derive(Debug, Clone)]
struct FormState {
    columns: Vec<ColumnInfo>,
    values: Vec<String>,
    original: Vec<Option<String>>,
    row_id: Option<i64>,
}

impl FormState {
    fn empty(columns: Vec<ColumnInfo>) -> Self {
        let width = columns.len();
        Self {
            columns,
            values: vec![String::new(); width],
            original: vec![None; width],
            row_id: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// Events collected during a central-panel frame and applied afterwards.
#[derive(Default)]
struct CentralActions {
    run: bool,
    gesture_gained: bool,
    gesture_lost: bool,
    export: bool,
    import: bool,
    settings: bool,
    refresh: bool,
    search_typed: bool,
    add_row: bool,
    delete_row: Option<i64>,
    edit_row: Option<i64>,
    save_form: bool,
    new_form: bool,
    start_edit: Option<(usize, usize, String)>,
    commit_edit: bool,
    cancel_edit: bool,
    choose_query: Option<String>,
}

/// The application state.
pub struct FormCraftApp {
    shell: AppShell,
    host: DocumentHost<Document>,
    session: Option<Connection>,
    tables: Vec<String>,
    selected_table: Option<String>,
    table_rows: TableRows,
    form: FormState,
    view: View,
    search: String,
    limit: u32,
    editing: Option<CellEdit>,
    schema_dialog: Option<SchemaDialog>,
    confirmation: Option<Confirmation>,
    show_schema: bool,
    undo_edits: Vec<DataEdit>,
    redo_edits: Vec<DataEdit>,
    error: Option<String>,
    gesture_active: bool,
    pending: Option<PendingAction>,
}

impl FormCraftApp {
    /// Build the application, applying `--sample` and `--open`.
    pub fn new(context: &eframe::CreationContext<'_>, options: CliOptions) -> Self {
        let identity = AppIdentity {
            id: "formcraft",
            name: "FormCraft",
            version: APP_VERSION,
            tagline: "SQLite database browser and form builder",
            repository: Some("https://github.com/emilio-n8n/formcraft"),
        };
        let shell = AppShell::new(identity);
        let document = if options.sample {
            Document::sample()
        } else {
            Document::default()
        };
        let mut app = Self {
            shell,
            host: DocumentHost::new(document, FORMAT, SCHEMA_VERSION, APP_VERSION),
            session: None,
            tables: Vec::new(),
            selected_table: None,
            table_rows: TableRows::default(),
            form: FormState::empty(Vec::new()),
            view: View::Grid,
            search: String::new(),
            limit: MAX_VISIBLE_ROWS,
            editing: None,
            schema_dialog: None,
            confirmation: None,
            show_schema: false,
            undo_edits: Vec::new(),
            redo_edits: Vec::new(),
            error: None,
            gesture_active: false,
            pending: None,
        };
        app.shell.install_theme(&context.egui_ctx);
        if options.sample {
            match database::create_memory_database() {
                Ok(connection) => match app.install_session(connection) {
                    Ok(()) => app.shell.set_status("Sample database loaded (in memory)"),
                    Err(error) => app
                        .shell
                        .notify(Tone::Error, "Sample database error", error),
                },
                Err(error) => app.shell.notify(
                    Tone::Error,
                    "Could not create sample database",
                    error.to_string(),
                ),
            }
        }
        if let Some(path) = options.open {
            app.load_path(path);
        }
        app
    }

    fn report_error(&mut self, title: &str, message: &str) {
        self.error = Some(message.to_owned());
        self.shell.notify(Tone::Error, title, message);
    }

    /// Install a new live session, checking the first table before replacing
    /// any existing state so a failed open leaves the previous data usable.
    fn install_session(&mut self, connection: Connection) -> Result<(), String> {
        let tables = database::list_tables(&connection).map_err(|error| error.to_string())?;
        if let Some(first) = tables.first() {
            database::table_rows(&connection, first, None, MAX_VISIBLE_ROWS)
                .map_err(|error| error.to_string())?;
        }
        self.session = Some(connection);
        self.tables = tables;
        self.selected_table = self.tables.first().cloned();
        self.undo_edits.clear();
        self.redo_edits.clear();
        self.editing = None;
        self.search.clear();
        self.refresh_rows();
        self.refresh_form();
        Ok(())
    }

    fn attach_database(&mut self, path: &str) -> Result<(), String> {
        let connection = database::open_file(path).map_err(|error| error.to_string())?;
        self.install_session(connection)
    }

    fn load_path(&mut self, path: PathBuf) {
        match DocumentHost::from_file(path.clone(), FORMAT, SCHEMA_VERSION, APP_VERSION) {
            Ok(host) => {
                self.host = host;
                self.editing = None;
                self.undo_edits.clear();
                self.redo_edits.clear();
                self.error = None;
                let database_path = self.host.document().database_path.clone();
                if let Some(database_path) = database_path
                    && let Err(error) = self.attach_database(&database_path)
                {
                    self.shell
                        .notify(Tone::Warning, "Database not opened", error);
                }
                self.shell.set_status(format!("Opened {}", path.display()));
            }
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not open project", error.to_string())
            }
        }
    }

    fn new_document(&mut self) {
        self.host.reset(Document::default());
        self.session = None;
        self.tables.clear();
        self.selected_table = None;
        self.table_rows = TableRows::default();
        self.form = FormState::empty(Vec::new());
        self.editing = None;
        self.search.clear();
        self.undo_edits.clear();
        self.redo_edits.clear();
        self.error = None;
        self.shell.set_status("New document");
    }

    fn finish_gesture(&mut self) {
        if self.gesture_active {
            self.host.history_mut().end_gesture();
            self.gesture_active = false;
        }
    }

    fn save(&mut self) -> bool {
        self.finish_gesture();
        if self.host.path().is_none() {
            return self.save_as();
        }
        match self.host.save() {
            Ok(path) => {
                self.shell.set_status(format!("Saved {}", path.display()));
                true
            }
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not save project", error.to_string());
                false
            }
        }
    }

    fn save_as(&mut self) -> bool {
        self.finish_gesture();
        let default_name = format!("{}.json", slugify(&self.host.document().title));
        let Some(path) = dialogs::save_project("Save project", &default_name, "json") else {
            return false;
        };
        match self.host.save_as(path) {
            Ok(path) => {
                self.shell.set_status(format!("Saved {}", path.display()));
                true
            }
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not save project", error.to_string());
                false
            }
        }
    }

    fn request_new(&mut self) {
        self.finish_gesture();
        if self.host.is_dirty() {
            self.pending = Some(PendingAction::New);
        } else {
            self.new_document();
        }
    }

    fn request_open(&mut self) {
        self.finish_gesture();
        let Some(path) = dialogs::open_project("Open project", &["json"]) else {
            return;
        };
        if self.host.is_dirty() {
            self.pending = Some(PendingAction::Open(path));
        } else {
            self.load_path(path);
        }
    }

    fn perform(&mut self, action: PendingAction) {
        match action {
            PendingAction::New => self.new_document(),
            PendingAction::Open(path) => self.load_path(path),
        }
    }

    fn resolve_pending(&mut self, choice: PendingChoice) {
        let Some(action) = self.pending.take() else {
            return;
        };
        match choice {
            PendingChoice::Cancel => {}
            PendingChoice::Discard => self.perform(action),
            PendingChoice::Save => {
                if self.save() {
                    self.perform(action);
                } else {
                    self.pending = Some(action);
                }
            }
        }
    }

    fn open_database(&mut self) {
        self.finish_gesture();
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open SQLite database")
            .add_filter("SQLite database", &["sqlite", "db", "sqlite3"])
            .pick_file()
        else {
            return;
        };
        let Some(path_text) = path.to_str().map(str::to_owned) else {
            self.shell
                .notify(Tone::Error, "Could not open database", "Invalid file path.");
            return;
        };
        if let Err(error) = self.attach_database(&path_text) {
            self.shell
                .notify(Tone::Error, "Could not open database", error);
            return;
        }
        let first_table = self.selected_table.clone();
        self.host.apply(move |document| {
            document.database_path = Some(path_text);
            if document.last_query.is_empty()
                && let Some(table) = first_table
            {
                document.last_query = database::select_all_sql(&table);
            }
        });
        self.shell
            .set_status(format!("Opened database {}", path.display()));
    }

    fn refresh_tables(&mut self) {
        let tables = match self.session.as_ref() {
            Some(connection) => match database::list_tables(connection) {
                Ok(tables) => tables,
                Err(error) => {
                    self.report_error("Could not list tables", &error.to_string());
                    return;
                }
            },
            None => Vec::new(),
        };
        self.tables = tables;
        if self
            .selected_table
            .as_ref()
            .is_none_or(|table| !self.tables.contains(table))
        {
            self.selected_table = self.tables.first().cloned();
        }
        self.refresh_rows();
        self.refresh_form();
    }

    fn refresh_rows(&mut self) {
        let outcome = match (self.session.as_ref(), self.selected_table.as_deref()) {
            (Some(connection), Some(table)) => {
                database::table_rows(connection, table, Some(&self.search), self.limit)
            }
            _ => {
                self.table_rows = TableRows::default();
                return;
            }
        };
        match outcome {
            Ok(rows) => {
                self.table_rows = rows;
                self.error = None;
            }
            Err(error) => {
                self.table_rows = TableRows::default();
                self.error = Some(error.to_string());
            }
        }
    }

    fn refresh_form(&mut self) {
        let columns = match (self.session.as_ref(), self.selected_table.as_deref()) {
            (Some(connection), Some(table)) => match database::table_schema(connection, table) {
                Ok(columns) => columns,
                Err(error) => {
                    self.error = Some(error.to_string());
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        let fields = database::build_form_fields(&columns, None);
        let mut state = FormState::empty(columns);
        state.values = fields.iter().map(|field| field.value.clone()).collect();
        self.form = state;
    }

    fn select_table(&mut self, table: String) {
        self.selected_table = Some(table.clone());
        self.editing = None;
        self.search.clear();
        self.refresh_rows();
        self.refresh_form();
        let sql = database::select_all_sql(&table);
        self.host.apply(move |document| document.last_query = sql);
        self.shell.set_status(format!("Table {table}"));
    }

    fn run_query(&mut self) {
        self.finish_gesture();
        let sql = self.host.document().last_query.clone();
        let outcome = match (self.session.as_ref(), sql.trim().is_empty()) {
            (Some(connection), false) => database::run_query(connection, &sql),
            (Some(_), true) => {
                self.shell
                    .notify(Tone::Warning, "No query", "Enter a SQL query first.");
                return;
            }
            (None, _) => {
                self.shell.notify(
                    Tone::Warning,
                    "No database",
                    "Open a SQLite database first.",
                );
                return;
            }
        };
        match outcome {
            Ok(result) => {
                let rows = result.row_count();
                let columns = result.columns.len();
                self.table_rows = TableRows {
                    result,
                    row_ids: Vec::new(),
                };
                self.error = None;
                self.host.apply(|document| document.remember_query(&sql));
                if columns == 0 {
                    self.shell.set_status("Statement executed");
                } else {
                    self.shell
                        .set_status(format!("{rows} rows, {columns} columns"));
                }
            }
            Err(error) => {
                self.table_rows = TableRows::default();
                self.error = Some(error.to_string());
                self.shell.set_status("Query failed");
            }
        }
    }

    fn commit_cell_edit(&mut self) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let Some(table) = self.selected_table.clone() else {
            return;
        };
        let Some(row_id) = self.table_rows.row_ids.get(edit.row).copied() else {
            return;
        };
        let Some(column) = self.table_rows.result.columns.get(edit.column).cloned() else {
            return;
        };
        let before = self
            .table_rows
            .result
            .rows
            .get(edit.row)
            .and_then(|row| row.get(edit.column))
            .cloned()
            .flatten();
        let after = if edit.text.trim().is_empty() {
            None
        } else {
            Some(edit.text.trim().to_owned())
        };
        let mut change = DataEdit::Cell {
            table,
            row_id,
            column,
            before,
            after,
        };
        if let Some(connection) = self.session.as_mut() {
            match change.redo(connection) {
                Ok(()) => {
                    self.undo_edits.push(change);
                    self.redo_edits.clear();
                    self.refresh_rows();
                    self.refresh_form();
                    self.shell.set_status("Cell updated");
                }
                Err(error) => self.report_error("Could not update cell", &error.to_string()),
            }
        }
    }

    fn add_row(&mut self) {
        let Some(table) = self.selected_table.clone() else {
            return;
        };
        let columns = match self.session.as_ref() {
            Some(connection) => match database::table_schema(connection, &table) {
                Ok(columns) => columns,
                Err(error) => {
                    self.report_error("Could not read schema", &error.to_string());
                    return;
                }
            },
            None => return,
        };
        let names = columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let values = vec![None; names.len()];
        let mut change = DataEdit::Insert {
            table,
            columns: names,
            values,
            row_id: 0,
        };
        if let Some(connection) = self.session.as_mut() {
            match change.redo(connection) {
                Ok(()) => {
                    self.undo_edits.push(change);
                    self.redo_edits.clear();
                    self.refresh_rows();
                    self.refresh_form();
                    self.shell.set_status("Row added");
                }
                Err(error) => self.report_error("Could not add row", &error.to_string()),
            }
        }
    }

    fn request_delete_row(&mut self, row_id: i64) {
        self.confirmation = Some(Confirmation::DeleteRow { row_id });
    }

    fn request_drop_table(&mut self, table: String) {
        self.confirmation = Some(Confirmation::DropTable { table });
    }

    fn perform_confirmation(&mut self, confirmation: Confirmation) {
        match confirmation {
            Confirmation::DeleteRow { row_id } => {
                let Some(table) = self.selected_table.clone() else {
                    return;
                };
                let details = self.session.as_ref().and_then(|connection| {
                    let columns = database::table_schema(connection, &table).ok()?;
                    let values = database::record_values(connection, &table, row_id).ok()??;
                    Some((columns, values))
                });
                let Some((columns, values)) = details else {
                    self.report_error("Could not read row", "The row no longer exists.");
                    return;
                };
                let names = columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>();
                let mut change = DataEdit::Delete {
                    table,
                    columns: names,
                    values,
                    row_id,
                };
                if let Some(connection) = self.session.as_mut() {
                    match change.redo(connection) {
                        Ok(()) => {
                            self.undo_edits.push(change);
                            self.redo_edits.clear();
                            self.refresh_rows();
                            self.refresh_form();
                            self.shell.set_status("Row deleted");
                        }
                        Err(error) => self.report_error("Could not delete row", &error.to_string()),
                    }
                }
            }
            Confirmation::DropTable { table } => {
                if let Some(connection) = self.session.as_ref() {
                    match database::drop_table(connection, &table) {
                        Ok(()) => {
                            self.undo_edits.clear();
                            self.redo_edits.clear();
                            self.refresh_tables();
                            let dropped = table.clone();
                            self.host.apply(move |document| {
                                if document.last_query.contains(&dropped) {
                                    document.last_query.clear();
                                }
                            });
                            self.shell.set_status(format!("Dropped table {table}"));
                        }
                        Err(error) => self.report_error("Could not drop table", &error.to_string()),
                    }
                }
            }
        }
    }

    fn undo(&mut self) {
        self.finish_gesture();
        if let Some(mut edit) = self.undo_edits.pop() {
            let label = edit.label();
            if let Some(connection) = self.session.as_mut() {
                match edit.undo(connection) {
                    Ok(()) => {
                        self.redo_edits.push(edit);
                        self.refresh_rows();
                        self.refresh_form();
                        self.shell.set_status(format!("Undo {label}"));
                    }
                    Err(error) => {
                        self.undo_edits.push(edit);
                        self.report_error("Could not undo", &error.to_string());
                    }
                }
            }
        } else if self.host.undo() {
            self.shell.set_status("Undo");
        }
    }

    fn redo(&mut self) {
        self.finish_gesture();
        if let Some(mut edit) = self.redo_edits.pop() {
            let label = edit.label();
            if let Some(connection) = self.session.as_mut() {
                match edit.redo(connection) {
                    Ok(()) => {
                        self.undo_edits.push(edit);
                        self.refresh_rows();
                        self.refresh_form();
                        self.shell.set_status(format!("Redo {label}"));
                    }
                    Err(error) => {
                        self.redo_edits.push(edit);
                        self.report_error("Could not redo", &error.to_string());
                    }
                }
            }
        } else if self.host.redo() {
            self.shell.set_status("Redo");
        }
    }

    fn load_form_row(&mut self, row_id: i64) {
        let Some(table) = self.selected_table.clone() else {
            return;
        };
        let outcome = match self.session.as_ref() {
            Some(connection) => database::record_values(connection, &table, row_id),
            None => return,
        };
        match outcome {
            Ok(Some(values)) => {
                let fields = database::build_form_fields(&self.form.columns, Some(&values));
                self.form.row_id = Some(row_id);
                self.form.original = values;
                self.form.values = fields.iter().map(|field| field.value.clone()).collect();
                self.view = View::Form;
                self.shell.set_status(format!("Editing row {row_id}"));
            }
            Ok(None) => self.report_error("Could not load row", "The row no longer exists."),
            Err(error) => self.report_error("Could not load row", &error.to_string()),
        }
    }

    fn save_form(&mut self) {
        let Some(table) = self.selected_table.clone() else {
            return;
        };
        let form = self.form.clone();
        if form.is_empty() {
            self.shell
                .notify(Tone::Warning, "No fields", "Select a table first.");
            return;
        }
        let values = form
            .values
            .iter()
            .map(|text| {
                if text.trim().is_empty() {
                    None
                } else {
                    Some(text.trim().to_owned())
                }
            })
            .collect::<Vec<_>>();
        for (column, value) in form.columns.iter().zip(&values) {
            if let Err(error) = database::coerce_value(value.as_deref(), column.sql_type) {
                self.report_error("Invalid value", &error.to_string());
                return;
            }
        }
        match form.row_id {
            Some(row_id) => {
                let mut edits = Vec::new();
                for (index, column) in form.columns.iter().enumerate() {
                    let before = form.original.get(index).cloned().flatten();
                    let after = values.get(index).cloned().flatten();
                    if before != after {
                        edits.push(DataEdit::Cell {
                            table: table.clone(),
                            row_id,
                            column: column.name.clone(),
                            before,
                            after,
                        });
                    }
                }
                if edits.is_empty() {
                    self.shell.set_status("No changes to save");
                    return;
                }
                let change = DataEdit::Batch(edits);
                if let Some(connection) = self.session.as_mut() {
                    match database::update_record(connection, &table, row_id, &values) {
                        Ok(()) => {
                            self.undo_edits.push(change);
                            self.redo_edits.clear();
                            self.refresh_rows();
                            self.form.original = values;
                            self.shell.set_status("Record saved");
                        }
                        Err(error) => {
                            self.report_error("Could not save record", &error.to_string())
                        }
                    }
                }
            }
            None => {
                let names = form
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>();
                let mut change = DataEdit::Insert {
                    table,
                    columns: names,
                    values,
                    row_id: 0,
                };
                if let Some(connection) = self.session.as_mut() {
                    match change.redo(connection) {
                        Ok(()) => {
                            self.undo_edits.push(change);
                            self.redo_edits.clear();
                            self.refresh_rows();
                            self.shell.set_status("Record created");
                        }
                        Err(error) => {
                            self.report_error("Could not create record", &error.to_string())
                        }
                    }
                }
            }
        }
    }

    fn open_schema_dialog(&mut self, kind: SchemaKind) {
        let table_name = self.selected_table.clone().unwrap_or_default();
        self.schema_dialog = Some(SchemaDialog {
            kind,
            table_name,
            columns_text: "id:INTEGER\nname:TEXT\nnotes:TEXT".into(),
            column_name: String::new(),
            column_type: SqlType::Text,
        });
    }

    fn apply_schema_dialog(&mut self, dialog: &SchemaDialog) {
        match dialog.kind {
            SchemaKind::CreateTable => {
                let definitions = match database::parse_column_defs(&dialog.columns_text) {
                    Ok(definitions) => definitions,
                    Err(error) => {
                        self.report_error("Invalid columns", &error.to_string());
                        return;
                    }
                };
                let columns = definitions
                    .iter()
                    .map(|column| (column.name.clone(), column.sql_type))
                    .collect::<Vec<_>>();
                if let Some(connection) = self.session.as_ref() {
                    match database::create_table(connection, &dialog.table_name, &columns) {
                        Ok(()) => {
                            self.refresh_tables();
                            self.selected_table = Some(dialog.table_name.clone());
                            self.refresh_rows();
                            self.refresh_form();
                            self.shell
                                .set_status(format!("Created table {}", dialog.table_name));
                        }
                        Err(error) => {
                            self.report_error("Could not create table", &error.to_string())
                        }
                    }
                }
            }
            SchemaKind::AddColumn => {
                let Some(table) = self.selected_table.clone() else {
                    return;
                };
                if let Some(connection) = self.session.as_ref() {
                    match database::add_column(
                        connection,
                        &table,
                        &dialog.column_name,
                        dialog.column_type,
                    ) {
                        Ok(()) => {
                            self.refresh_rows();
                            self.refresh_form();
                            self.shell.set_status(format!(
                                "Added column {} to {table}",
                                dialog.column_name
                            ));
                        }
                        Err(error) => self.report_error("Could not add column", &error.to_string()),
                    }
                }
            }
        }
    }

    fn import_csv(&mut self) {
        self.finish_gesture();
        let Some(table) = self.selected_table.clone() else {
            self.shell
                .notify(Tone::Warning, "No table", "Select a table first.");
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import CSV")
            .add_filter("CSV", &["csv"])
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                self.report_error("Could not read CSV", &error.to_string());
                return;
            }
        };
        let data = match database::parse_csv(&text, true) {
            Ok(data) => data,
            Err(error) => {
                self.report_error("Could not parse CSV", &error.to_string());
                return;
            }
        };
        if let Some(connection) = self.session.as_mut() {
            match database::import_csv(connection, &table, &data) {
                Ok(count) => {
                    self.refresh_rows();
                    self.refresh_form();
                    self.shell
                        .set_status(format!("Imported {count} row(s) into {table}"));
                }
                Err(error) => self.report_error("Could not import CSV", &error.to_string()),
            }
        }
    }

    fn export_csv(&mut self) {
        self.finish_gesture();
        let result = self.table_rows.result.clone();
        if result.is_empty() && result.columns.is_empty() {
            self.shell
                .notify(Tone::Warning, "Nothing to export", "Run a query first.");
            return;
        }
        let default_name = match &self.selected_table {
            Some(table) => format!("{}.csv", slugify(table)),
            None => "results.csv".to_owned(),
        };
        let Some(chosen) = rfd::FileDialog::new()
            .set_title("Export CSV")
            .add_filter("CSV", &["csv"])
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        let path = if chosen.extension().is_some() {
            chosen
        } else {
            let mut path = chosen;
            path.set_extension("csv");
            path
        };
        let csv = database::export_csv(&result);
        match std::fs::write(&path, csv) {
            Ok(()) => self
                .shell
                .set_status(format!("Exported {} rows", result.row_count())),
            Err(error) => self
                .shell
                .notify(Tone::Error, "Could not export CSV", error.to_string()),
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::New => self.request_new(),
            Action::Open => self.request_open(),
            Action::OpenDatabase => self.open_database(),
            Action::Save => {
                self.save();
            }
            Action::SaveAs => {
                self.save_as();
            }
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::RunQuery => self.run_query(),
            Action::ExportCsv => self.export_csv(),
            Action::ImportCsv => self.import_csv(),
            Action::CreateTable => self.open_schema_dialog(SchemaKind::CreateTable),
            Action::AddColumn => self.open_schema_dialog(SchemaKind::AddColumn),
            Action::DropTable => {
                if let Some(table) = self.selected_table.clone() {
                    self.request_drop_table(table);
                }
            }
            Action::ShowSchema => self.show_schema = true,
            Action::About => self.shell.open_about(),
            Action::Settings => self.shell.open_settings(),
        }
    }

    fn handle_shortcuts(&mut self, ui: &egui::Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let action = ui.input_mut(|input| {
            if input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            ) {
                Some(Action::SaveAs)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::S) {
                Some(Action::Save)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::O) {
                Some(Action::Open)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::N) {
                Some(Action::New)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z) {
                Some(Action::Undo)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y) {
                Some(Action::Redo)
            } else if input.key_pressed(egui::Key::F5) {
                Some(Action::RunQuery)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.handle_action(action);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let title = self.host.display_name();
        let dirty = self.host.is_dirty();
        let mut action = None;
        self.shell.top_bar(ui, &title, dirty, |ui| {
            ui.menu_button("File", |ui| {
                for (label, candidate) in [
                    ("New", Action::New),
                    ("Open project...", Action::Open),
                    ("Save", Action::Save),
                    ("Save As...", Action::SaveAs),
                ] {
                    if ui.button(label).clicked() {
                        action = Some(candidate);
                        ui.close();
                    }
                }
                ui.separator();
                for (label, candidate) in [
                    ("Open database...", Action::OpenDatabase),
                    ("Import CSV...", Action::ImportCsv),
                    ("Export CSV...", Action::ExportCsv),
                ] {
                    if ui.button(label).clicked() {
                        action = Some(candidate);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Table", |ui| {
                for (label, candidate) in [
                    ("Create table...", Action::CreateTable),
                    ("Add column...", Action::AddColumn),
                    ("Show schema", Action::ShowSchema),
                    ("Delete table...", Action::DropTable),
                ] {
                    if ui.button(label).clicked() {
                        action = Some(candidate);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Query", |ui| {
                if ui.button("Run (F5)").clicked() {
                    action = Some(Action::RunQuery);
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.button("Undo").clicked() {
                    action = Some(Action::Undo);
                    ui.close();
                }
                if ui.button("Redo").clicked() {
                    action = Some(Action::Redo);
                    ui.close();
                }
            });
            if ui.button("About").clicked() {
                action = Some(Action::About);
            }
            if ui.button("Settings").clicked() {
                action = Some(Action::Settings);
            }
        });
        if let Some(action) = action {
            self.handle_action(action);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let rows = self.table_rows.result.row_count();
        let label = match self.view {
            View::Grid => "grid",
            View::Form => "form",
        };
        self.shell.status_bar_with(ui, |ui| {
            ui.label(
                RichText::new(format!("{rows} row(s) — {label}"))
                    .size(11.0)
                    .color(theme::MUTED),
            );
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        let mut chosen = None;
        let mut open_database = false;
        let mut create_table = false;
        egui::Panel::left("tables-panel")
            .resizable(true)
            .default_size(230.0)
            .min_size(170.0)
            .show(ui, |ui| {
                widgets::section_label(ui, "Tables");
                ui.add_space(4.0);
                if self.session.is_none() {
                    widgets::empty_state(
                        ui,
                        "No database open",
                        "Use File > Open database... or launch with --sample.",
                    );
                } else {
                    let selected = self.selected_table.clone();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for table in &self.tables {
                                let is_selected = selected.as_deref() == Some(table.as_str());
                                if ui.selectable_label(is_selected, table).clicked() {
                                    chosen = Some(table.clone());
                                }
                            }
                        });
                    ui.separator();
                    if ui.button("New table...").clicked() {
                        create_table = true;
                    }
                }
                ui.separator();
                if ui.button("Open database...").clicked() {
                    open_database = true;
                }
                if let Some(path) = self.host.document().database_path.clone() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(path).size(10.0).color(theme::FAINT));
                } else if self.session.is_some() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("In-memory sample database")
                            .size(10.0)
                            .color(theme::FAINT),
                    );
                }
            });
        if let Some(table) = chosen {
            self.select_table(table);
        }
        if open_database {
            self.open_database();
        }
        if create_table {
            self.open_schema_dialog(SchemaKind::CreateTable);
        }
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        let Self {
            host,
            table_rows,
            editing,
            form,
            view,
            search,
            limit,
            error,
            session,
            ..
        } = self;
        let mut actions = CentralActions::default();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Data");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::toolbar_button(ui, "Settings", "Preferences").clicked() {
                        actions.settings = true;
                    }
                    if widgets::toolbar_button(ui, "Export CSV", "Export the current result")
                        .clicked()
                    {
                        actions.export = true;
                    }
                    if widgets::toolbar_button(ui, "Import CSV", "Import into the selected table")
                        .clicked()
                    {
                        actions.import = true;
                    }
                    if widgets::toolbar_button(ui, "Run", "Execute the SQL query (F5)").clicked() {
                        actions.run = true;
                    }
                });
            });
            ui.add_space(6.0);

            ui.label(RichText::new("SQL query").size(11.0).color(theme::MUTED));
            let query = &mut host.history_mut().current_mut().last_query;
            let response = ui.add(
                egui::TextEdit::multiline(query)
                    .hint_text("SELECT * FROM people;")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            actions.gesture_gained |= response.gained_focus();
            actions.gesture_lost |= response.lost_focus();

            egui::CollapsingHeader::new("Recent queries")
                .default_open(false)
                .show(ui, |ui| {
                    let history = host.document().query_history.clone();
                    if history.is_empty() {
                        ui.label(RichText::new("No queries yet").color(theme::FAINT));
                    }
                    for query in history.iter().rev().take(12) {
                        if ui.selectable_label(false, query).clicked() {
                            actions.choose_query = Some(query.clone());
                        }
                    }
                });

            if let Some(message) = error {
                ui.add_space(4.0);
                ui.label(RichText::new(message.as_str()).color(Tone::Error.color()));
            }
            ui.add_space(4.0);
            ui.separator();

            ui.horizontal(|ui| {
                ui.selectable_value(view, View::Grid, "Grid");
                ui.selectable_value(view, View::Form, "Form");
                if session.is_none() {
                    return;
                }
                ui.add_space(8.0);
                ui.label("Filter");
                let response = ui.add(egui::TextEdit::singleline(search).desired_width(150.0));
                actions.search_typed |= response.changed();
                if ui.button("Refresh").clicked() {
                    actions.refresh = true;
                }
                egui::ComboBox::from_label("Limit")
                    .selected_text(limit.to_string())
                    .show_ui(ui, |ui| {
                        for candidate in [50_u32, 100, 200, 500] {
                            ui.selectable_value(limit, candidate, candidate.to_string());
                        }
                    });
            });

            if session.is_none() {
                widgets::empty_state(
                    ui,
                    "No database open",
                    "Open a SQLite file or launch with --sample to browse data.",
                );
                return;
            }

            match *view {
                View::Grid => {
                    ui.horizontal(|ui| {
                        if ui.button("Add row").clicked() {
                            actions.add_row = true;
                        }
                        ui.label(
                            RichText::new(format!("{} row(s)", table_rows.result.row_count()))
                                .color(theme::MUTED),
                        );
                    });
                    ui.add_space(4.0);
                    if table_rows.result.columns.is_empty() {
                        widgets::empty_state(
                            ui,
                            "No table selected",
                            "Choose a table on the left.",
                        );
                    } else {
                        egui::ScrollArea::both()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Grid::new("result-grid").striped(true).show(ui, |ui| {
                                    ui.label("");
                                    for column in &table_rows.result.columns {
                                        ui.label(RichText::new(column).strong());
                                    }
                                    ui.end_row();
                                    for (row_index, row) in
                                        table_rows.result.rows.iter().enumerate()
                                    {
                                        let key = table_rows.row_ids.get(row_index).copied();
                                        if ui.small_button("Form").clicked() {
                                            actions.edit_row = key;
                                        }
                                        if ui.small_button("Del").clicked() {
                                            actions.delete_row = key;
                                        }
                                        for (column_index, cell) in row.iter().enumerate() {
                                            let is_editing = editing.as_ref().is_some_and(|edit| {
                                                edit.row == row_index && edit.column == column_index
                                            });
                                            if is_editing {
                                                if let Some(edit) = editing.as_mut() {
                                                    let id = egui::Id::unique((
                                                        "cell-edit",
                                                        row_index,
                                                        column_index,
                                                    ));
                                                    let response = ui.add(
                                                        egui::TextEdit::singleline(&mut edit.text)
                                                            .id(id)
                                                            .desired_width(140.0),
                                                    );
                                                    if response.lost_focus()
                                                        || ui.input(|input| {
                                                            input.key_pressed(egui::Key::Enter)
                                                        })
                                                    {
                                                        actions.commit_edit = true;
                                                    }
                                                    if ui.input(|input| {
                                                        input.key_pressed(egui::Key::Escape)
                                                    }) {
                                                        actions.cancel_edit = true;
                                                    }
                                                }
                                            } else {
                                                let response =
                                                    ui.label(cell.as_deref().unwrap_or("NULL"));
                                                if response.double_clicked() {
                                                    actions.start_edit = Some((
                                                        row_index,
                                                        column_index,
                                                        cell.as_deref().unwrap_or("").to_owned(),
                                                    ));
                                                }
                                            }
                                        }
                                        ui.end_row();
                                    }
                                });
                            });
                    }
                }
                View::Form => {
                    ui.horizontal(|ui| {
                        if ui.button("New record").clicked() {
                            actions.new_form = true;
                        }
                        if ui.button("Create / Save").clicked() {
                            actions.save_form = true;
                        }
                    });
                    ui.add_space(6.0);
                    if form.is_empty() {
                        widgets::empty_state(ui, "No fields", "Select a table first.");
                    } else {
                        egui::Grid::new("form-grid").striped(true).show(ui, |ui| {
                            for (index, column) in form.columns.iter().enumerate() {
                                ui.label(&column.name);
                                if let Some(value) = form.values.get_mut(index) {
                                    ui.add(egui::TextEdit::singleline(value).desired_width(260.0));
                                }
                                ui.label(
                                    RichText::new(column.sql_type.label()).color(theme::FAINT),
                                );
                                ui.end_row();
                            }
                        });
                    }
                }
            }
        });

        if actions.gesture_gained {
            self.gesture_active = true;
        }
        if actions.gesture_lost {
            self.finish_gesture();
        }
        if let Some((row, column, text)) = actions.start_edit {
            self.editing = Some(CellEdit { row, column, text });
            ui.memory_mut(|memory| {
                memory.request_focus(egui::Id::unique(("cell-edit", row, column)));
            });
        }
        if actions.commit_edit {
            self.commit_cell_edit();
        }
        if actions.cancel_edit {
            self.editing = None;
        }
        if actions.run {
            self.run_query();
        }
        if actions.import {
            self.import_csv();
        }
        if actions.export {
            self.export_csv();
        }
        if actions.settings {
            self.shell.open_settings();
        }
        if let Some(query) = actions.choose_query {
            self.host.apply(move |document| document.last_query = query);
        }
        if actions.refresh || actions.search_typed {
            self.refresh_rows();
        }
        if actions.add_row {
            self.add_row();
        }
        if let Some(row_id) = actions.edit_row {
            self.load_form_row(row_id);
        }
        if let Some(row_id) = actions.delete_row {
            self.request_delete_row(row_id);
        }
        if actions.new_form {
            self.form = FormState::empty(self.form.columns.clone());
        }
        if actions.save_form {
            self.save_form();
        }
    }

    fn confirmation_window(&mut self, ctx: &egui::Context) {
        let Some(confirmation) = self.confirmation.clone() else {
            return;
        };
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new("Confirm deletion")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                match &confirmation {
                    Confirmation::DeleteRow { .. } => {
                        ui.label("Delete this row? This cannot be undone from the project file.");
                    }
                    Confirmation::DropTable { table } => {
                        ui.label(format!(
                            "Delete table '{table}' and all its rows? This cannot be undone."
                        ));
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    if ui.button("Delete").clicked() {
                        confirmed = true;
                    }
                });
            });
        if confirmed {
            self.perform_confirmation(confirmation);
            self.confirmation = None;
        } else if cancelled || !open {
            self.confirmation = None;
        }
    }

    fn schema_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.schema_dialog.clone() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Table schema")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                match dialog.kind {
                    SchemaKind::CreateTable => {
                        ui.label("Table name");
                        ui.text_edit_singleline(&mut dialog.table_name);
                        ui.add_space(4.0);
                        ui.label("Columns, one per line as name:TYPE");
                        ui.add(egui::TextEdit::multiline(&mut dialog.columns_text).desired_rows(4));
                        ui.label(
                            RichText::new("TYPE: TEXT, INTEGER or REAL")
                                .size(10.5)
                                .color(theme::FAINT),
                        );
                    }
                    SchemaKind::AddColumn => {
                        ui.label("Column name");
                        ui.text_edit_singleline(&mut dialog.column_name);
                        egui::ComboBox::from_label("Type").show_ui(ui, |ui| {
                            for sql_type in SqlType::ALL {
                                ui.selectable_value(
                                    &mut dialog.column_type,
                                    sql_type,
                                    sql_type.label(),
                                );
                            }
                        });
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                });
            });
        if apply {
            self.apply_schema_dialog(&dialog);
            self.schema_dialog = None;
        } else if cancel || !open {
            self.schema_dialog = None;
        }
    }

    fn schema_view_window(&mut self, ctx: &egui::Context) {
        if !self.show_schema {
            return;
        }
        let columns = self.form.columns.clone();
        let table = self
            .selected_table
            .clone()
            .unwrap_or_else(|| "none".to_owned());
        let mut open = true;
        egui::Window::new(format!("Schema — {table}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .show(ctx, |ui| {
                if columns.is_empty() {
                    ui.label("No table selected.");
                }
                for column in &columns {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&column.name).strong());
                        ui.label(column.sql_type.label());
                        if column.primary_key {
                            ui.label(RichText::new("PK").color(theme::ACCENT));
                        }
                        if column.not_null {
                            ui.label(RichText::new("NOT NULL").color(theme::WARNING));
                        }
                    });
                }
            });
        if !open {
            self.show_schema = false;
        }
    }

    fn unsaved_window(&mut self, ctx: &egui::Context) {
        if self.pending.is_none() {
            return;
        }
        let mut open = true;
        let mut choice = None;
        egui::Window::new("Unsaved changes")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.label("The current project has unsaved changes.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Save and continue").clicked() {
                        choice = Some(PendingChoice::Save);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(PendingChoice::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(PendingChoice::Cancel);
                    }
                });
            });
        if let Some(choice) = choice {
            self.resolve_pending(choice);
        } else if !open {
            self.pending = None;
        }
    }
}

impl eframe::App for FormCraftApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(ui);
        if self.gesture_active {
            self.host.history_mut().begin_gesture();
        }
        self.top_bar(ui);
        self.status_bar(ui);
        self.left_panel(ui);
        self.central_panel(ui);
        self.unsaved_window(ui.ctx());
        self.confirmation_window(ui.ctx());
        self.schema_window(ui.ctx());
        self.schema_view_window(ui.ctx());
        self.shell.show_windows(ui.ctx());
        self.shell
            .update_title(ui.ctx(), &self.host.display_name(), self.host.is_dirty());
    }
}

fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "untitled".to_owned()
    } else {
        slug.to_owned()
    }
}
