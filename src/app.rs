//! Application shell: panel layout, shortcuts, and the two modal dialogs
//! (save prompt when switching files, delete confirmation).

use std::path::PathBuf;

use crate::editor::Editor;
use crate::files::{Action, FilePanel};
use crate::strings as s;
use crate::terminal::Terminal;

pub struct VrsApp {
    files: FilePanel,
    editor: Editor,
    terminal: Terminal,
    terminal_height: f32,
    pending_open: Option<PathBuf>,
    pending_delete: Option<PathBuf>,
    /// Set when the window close button was pressed while the editor is dirty.
    close_prompt: bool,
    /// True once the user agreed to leave: the next close request goes through.
    allow_close: bool,
}

impl VrsApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let root = PathBuf::from(home).join(".vrs");
        let _ = std::fs::create_dir_all(&root);
        // Set before the first frame: doing it inside `ui` would only apply
        // from the next pass, showing a dark flash on startup.
        ctx.set_visuals(egui::Visuals::light());
        // egui binds Ctrl+Q to quitting by default, before the app can tell
        // whether the terminal has the keyboard. We handle it ourselves.
        ctx.options_mut(|o| o.quit_shortcuts.clear());
        Self {
            files: FilePanel::new(root.clone()),
            editor: Editor::new(),
            terminal: Terminal::new(&root, ctx.clone()),
            terminal_height: 220.0,
            pending_open: None,
            pending_delete: None,
            close_prompt: false,
            allow_close: false,
        }
    }

    fn handle_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Open(path) => {
                    if self.editor.is_dirty() {
                        self.pending_open = Some(path);
                    } else {
                        self.editor.open(path);
                    }
                }
                Action::Delete(path) => self.pending_delete = Some(path),
                Action::Moved { from, to } => {
                    if let Some(path) = &self.editor.path {
                        if let Ok(rest) = path.strip_prefix(&from) {
                            self.editor.path = Some(to.join(rest));
                        }
                    }
                }
            }
        }
    }
}

impl eframe::App for VrsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Do not lose unsaved work when the window is closed.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.editor.is_dirty()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_prompt = true;
        }

        // egui 0.36.2 stores the content rect of resizable panels, which drifts
        // whenever the content does not exactly match the panel size, so we
        // manage the terminal height ourselves with a non-resizable panel.
        // Keep the terminal inside the window: the minimum window is 420 px
        // tall, so a stored 600 px would push the editor off screen.
        let max_terminal = (ui.available_height() * 0.7).max(80.0);
        self.terminal_height = self.terminal_height.clamp(80.0, max_terminal);

        let mut drag = None;
        egui::Panel::bottom("terminal")
            .resizable(false)
            .exact_size(self.terminal_height)
            .show(ui, |ui| {
                drag = self.terminal.ui(ui);
            });
        if let Some(dy) = drag {
            self.terminal_height = (self.terminal_height - dy).clamp(80.0, max_terminal);
        }

        egui::Panel::left("files")
            .resizable(true)
            .default_size(200.0)
            .min_size(120.0)
            .max_size(420.0)
            .show(ui, |ui| {
                self.files.ui(ui);
            });

        egui::CentralPanel::default().show(ui, |ui| {
            self.editor.ui(ui);
        });

        if ctx.input(|i| i.modifiers.command) {
            // Ctrl+S and Ctrl+Q belong to the shell when the user is typing
            // in the terminal: they must not act behind their back.
            if !self.terminal.has_focus() {
                if ctx.input(|i| i.key_pressed(egui::Key::S)) {
                    self.editor.save();
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Q)) {
                    if self.editor.is_dirty() {
                        self.close_prompt = true;
                    } else {
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }

        let actions = self.files.take_actions();
        self.handle_actions(actions);

        // A pending open can go through once the editor is no longer dirty
        // (e.g. the user pressed Ctrl+S in the save prompt).
        if let Some(path) = self.pending_open.clone() {
            if !self.editor.is_dirty() {
                self.pending_open = None;
                self.editor.open(path);
            }
        }

        if self.editor.is_dirty() && self.pending_open.is_some() {
            let mut open = true;
            egui::Window::new(s::SAVE_CHANGES)
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    let name = self
                        .editor
                        .path
                        .as_deref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(format!("{}: {}", s::UNSAVED_CHANGES, name));
                    ui.horizontal(|ui| {
                        if ui.button(s::SAVE).clicked() {
                            self.editor.save();
                        }
                        if ui.button(s::DISCARD).clicked() {
                            if let Some(path) = self.pending_open.take() {
                                self.editor.open(path);
                            }
                        }
                        if ui.button(s::CANCEL).clicked() {
                            self.pending_open = None;
                        }
                    });
                });
            if !open {
                self.pending_open = None;
            }
        }

        if self.close_prompt {
            let mut open = true;
            egui::Window::new(s::QUIT_TITLE)
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    let name = self
                        .editor
                        .path
                        .as_deref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(format!("{}: {}", s::UNSAVED_CHANGES, name));
                    let mut quit = false;
                    ui.horizontal(|ui| {
                        if ui.button(s::SAVE).clicked() {
                            self.editor.save();
                            quit = true;
                        }
                        if ui.button(s::QUIT_CONFIRM).clicked() {
                            quit = true;
                        }
                        if ui.button(s::CANCEL).clicked() {
                            self.close_prompt = false;
                        }
                    });
                    if quit {
                        self.close_prompt = false;
                        self.allow_close = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            if !open {
                self.close_prompt = false;
            }
        }

        if let Some(path) = self.pending_delete.clone() {
            let mut open = true;
            egui::Window::new(s::CONFIRM_DELETE)
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(format!("{}: {}", s::DELETE_PROMPT, name));
                    ui.horizontal(|ui| {
                        if ui.button(s::DELETE).clicked() {
                            if let Some(deleted) = self.files.confirm_delete(&path) {
                                // The open file may sit inside a deleted
                                // folder, not only be the deleted file.
                                if self
                                    .editor
                                    .path
                                    .as_deref()
                                    .is_some_and(|open| open.starts_with(&deleted))
                                {
                                    self.editor.clear();
                                }
                            }
                            self.pending_delete = None;
                        }
                        if ui.button(s::CANCEL).clicked() {
                            self.pending_delete = None;
                        }
                    });
                });
            if !open {
                self.pending_delete = None;
            }
        }
    }
}
