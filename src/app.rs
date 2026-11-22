//! FormCraft UI and project lifecycle.

use std::path::PathBuf;

use craftkit_io::DocumentHost;
use craftkit_ui::cli::CliOptions;
use craftkit_ui::eframe::{self, egui};
use craftkit_ui::egui::{Align, Layout, RichText};
use craftkit_ui::{AppIdentity, AppShell, Tone, dialogs, theme, widgets};
use rusqlite::Connection;

use crate::database::{self, QueryResult, select_all_sql};
use crate::document::Document;

const FORMAT: &str = "formcraft";
const SCHEMA_VERSION: u32 = 1;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    New,
    Open,
    Save,
    SaveAs,
    Undo,
    Redo,
    OpenDatabase,
    RunQuery,
    ExportCsv,
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

/// The application state.
pub struct FormCraftApp {
    shell: AppShell,
    host: DocumentHost<Document>,
    session: Option<Connection>,
    tables: Vec<String>,
    selected_table: Option<String>,
    result: QueryResult,
    error: Option<String>,
    gesture_active: bool,
    pending: Option<PendingAction>,
}

impl FormCraftApp {
    /// Build the application, loading the sample database or startup project
    /// when requested by the command line.
    pub fn new(context: &eframe::CreationContext<'_>, options: CliOptions) -> Self {
        let identity = AppIdentity {
            id: "formcraft",
            name: "FormCraft",
            version: APP_VERSION,
            tagline: "SQLite database browser",
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
            result: QueryResult::default(),
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

    /// Replace the live session, reading its tables and first result grid.
    ///
    /// State is only replaced after every step succeeds, so a failed open
    /// leaves the previous database usable.
    fn install_session(&mut self, connection: Connection) -> Result<(), String> {
        let tables = database::list_tables(&connection).map_err(|error| error.to_string())?;
        let first = tables.first().cloned();
        let result = match &first {
            Some(table) => {
                database::query_table(&connection, table).map_err(|error| error.to_string())?
            }
            None => QueryResult::default(),
        };
        self.session = Some(connection);
        self.tables = tables;
        self.selected_table = first;
        self.result = result;
        self.error = None;
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
                self.tables.clear();
                self.selected_table = None;
                self.result = QueryResult::default();
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
        self.result = QueryResult::default();
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
                document.last_query = select_all_sql(&table);
            }
        });
        self.shell
            .set_status(format!("Opened database {}", path.display()));
    }

    fn select_table(&mut self, table: String) {
        let outcome = match self.session.as_ref() {
            Some(connection) => database::query_table(connection, &table),
            None => return,
        };
        match outcome {
            Ok(result) => {
                self.result = result;
                self.error = None;
                self.selected_table = Some(table.clone());
                let sql = select_all_sql(&table);
                self.host.apply(move |document| document.last_query = sql);
                self.shell.set_status(format!("Table {table}"));
            }
            Err(error) => {
                self.result = QueryResult::default();
                self.error = Some(error.to_string());
                self.shell.set_status("Could not read table");
            }
        }
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
                self.result = result;
                self.error = None;
                if columns == 0 {
                    self.shell.set_status("Statement executed");
                } else {
                    self.shell
                        .set_status(format!("{rows} rows, {columns} columns"));
                }
            }
            Err(error) => {
                self.result = QueryResult::default();
                self.error = Some(error.to_string());
                self.shell.set_status("Query failed");
            }
        }
    }

    fn export_csv(&mut self) {
        self.finish_gesture();
        if self.result.is_empty() && self.result.columns.is_empty() {
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
        let csv = database::export_csv(&self.result);
        match std::fs::write(&path, csv) {
            Ok(()) => self
                .shell
                .set_status(format!("Exported {} rows", self.result.row_count())),
            Err(error) => self
                .shell
                .notify(Tone::Error, "Could not export CSV", error.to_string()),
        }
    }

    fn clamp_selection(&mut self) {
        let valid = self
            .selected_table
            .as_ref()
            .is_some_and(|table| self.tables.contains(table));
        if !valid {
            self.selected_table = self.tables.first().cloned();
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::New => self.request_new(),
            Action::Open => self.request_open(),
            Action::Save => {
                self.save();
            }
            Action::SaveAs => {
                self.save_as();
            }
            Action::Undo => {
                self.finish_gesture();
                if self.host.undo() {
                    self.clamp_selection();
                    self.shell.set_status("Undo");
                }
            }
            Action::Redo => {
                self.finish_gesture();
                if self.host.redo() {
                    self.clamp_selection();
                    self.shell.set_status("Redo");
                }
            }
            Action::OpenDatabase => self.open_database(),
            Action::RunQuery => self.run_query(),
            Action::ExportCsv => self.export_csv(),
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
        let can_undo = self.host.can_undo();
        let can_redo = self.host.can_redo();
        let mut action = None;
        self.shell.top_bar(ui, &title, dirty, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    action = Some(Action::New);
                    ui.close();
                }
                if ui.button("Open project...").clicked() {
                    action = Some(Action::Open);
                    ui.close();
                }
                if ui.button("Save").clicked() {
                    action = Some(Action::Save);
                    ui.close();
                }
                if ui.button("Save As...").clicked() {
                    action = Some(Action::SaveAs);
                    ui.close();
                }
                ui.separator();
                if ui.button("Open database...").clicked() {
                    action = Some(Action::OpenDatabase);
                    ui.close();
                }
                if ui.button("Export CSV...").clicked() {
                    action = Some(Action::ExportCsv);
                    ui.close();
                }
            });
            ui.menu_button("Query", |ui| {
                if ui.button("Run (F5)").clicked() {
                    action = Some(Action::RunQuery);
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(can_undo, egui::Button::new("Undo"))
                    .clicked()
                {
                    action = Some(Action::Undo);
                    ui.close();
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("Redo"))
                    .clicked()
                {
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
        let rows = self.result.row_count();
        self.shell.status_bar_with(ui, |ui| {
            ui.label(RichText::new(format!("{rows} rows")).size(11.0));
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        let mut chosen = None;
        let mut open_database = false;
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
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        let mut gained_focus = false;
        let mut lost_focus = false;
        let mut run = false;
        let mut export = false;

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SQL query");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::toolbar_button(ui, "Run", "Execute the SQL query (F5)").clicked() {
                        run = true;
                    }
                    if widgets::toolbar_button(ui, "Export CSV", "Export the current result as CSV")
                        .clicked()
                    {
                        export = true;
                    }
                });
            });
            ui.add_space(6.0);

            let query = &mut self.host.history_mut().current_mut().last_query;
            let response = ui.add(
                egui::TextEdit::multiline(query)
                    .hint_text("SELECT * FROM people;")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            gained_focus |= response.gained_focus();
            lost_focus |= response.lost_focus();

            ui.add_space(6.0);
            if self.session.is_none() {
                widgets::empty_state(
                    ui,
                    "No database open",
                    "Open a SQLite file or launch with --sample to browse data.",
                );
                return;
            }
            if let Some(error) = &self.error {
                ui.label(RichText::new(error).color(Tone::Error.color()));
                ui.add_space(4.0);
            }
            ui.separator();
            ui.label(
                RichText::new(format!("{} rows", self.result.row_count()))
                    .size(11.0)
                    .color(theme::MUTED),
            );
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("result-grid").striped(true).show(ui, |ui| {
                        for column in &self.result.columns {
                            ui.label(RichText::new(column).strong());
                        }
                        ui.end_row();
                        for row in &self.result.rows {
                            for cell in row {
                                ui.label(cell.as_deref().unwrap_or("NULL"));
                            }
                            ui.end_row();
                        }
                    });
                });
        });

        if gained_focus {
            self.gesture_active = true;
        }
        if lost_focus {
            self.finish_gesture();
        }
        if run {
            self.run_query();
        }
        if export {
            self.export_csv();
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
