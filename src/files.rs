//! Left panel: file tree of the projects root (~/.vrs), with context menus,
//! inline creation, drag-and-drop moves, and periodic rescans so files created
//! from the terminal show up.

use std::fs;
use std::path::{Path, PathBuf};

use egui::{Id, Ui};

use crate::strings as s;

#[derive(Clone, Debug)]
pub enum Entry {
    Dir {
        path: PathBuf,
        name: String,
        children: Vec<Entry>,
    },
    File {
        path: PathBuf,
        name: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CreateKind {
    File,
    Folder,
}

pub enum Action {
    Open(PathBuf),
    Delete(PathBuf),
    Moved { from: PathBuf, to: PathBuf },
}

pub struct FilePanel {
    pub root: PathBuf,
    tree: Vec<Entry>,
    next_scan: f64,
    create: Option<(PathBuf, CreateKind, String)>,
    /// Whether the pending inline creation input should grab keyboard focus.
    focus_create: bool,
    actions: Vec<Action>,
}

impl FilePanel {
    pub fn new(root: PathBuf) -> Self {
        Self {
            tree: scan_dir(&root),
            root,
            next_scan: 0.0,
            create: None,
            focus_create: false,
            actions: Vec::new(),
        }
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    pub fn confirm_delete(&mut self, path: &Path) -> Option<PathBuf> {
        let result = if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        match result {
            Ok(()) => {
                self.schedule_rescan();
                Some(path.to_path_buf())
            }
            Err(_) => None,
        }
    }

    /// Refresh the tree on the next frame. The tree is never written while it
    /// is being drawn (it is temporarily moved out of `self`), so a rescan
    /// cannot be clobbered by the drawing pass.
    fn schedule_rescan(&mut self) {
        self.next_scan = 0.0;
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let now = ui.input(|i| i.time);
        // Keep waking up so files created elsewhere (e.g. in the terminal)
        // appear even when the user is not interacting with the window.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        if self.next_scan <= now {
            self.tree = scan_dir(&self.root);
            self.next_scan = now + 1.0;
        }

        // The panel shows where the tree actually lives, not a generic title.
        ui.label(
            egui::RichText::new(self.root.display().to_string())
                .monospace()
                .size(11.0)
                .weak(),
        );
        ui.add_space(4.0);

        // The tree scrolls when it is taller than the panel.
        egui::ScrollArea::vertical().auto_shrink(true).show(ui, |ui| {
            // Inline creation at the root level.
            if self
                .create
                .as_ref()
                .is_some_and(|(parent, _, _)| parent == &self.root)
            {
                self.draw_create_row(ui);
            }

            // Move the tree out instead of cloning it every frame: the
            // drawing only needs a shared borrow, which `self` cannot give
            // while `&mut self` methods run inside the loop.
            let tree = std::mem::take(&mut self.tree);
            for entry in &tree {
                self.draw_entry(ui, entry);
            }
            self.tree = tree;
        });

        // Background (below the tree when it is short): click and
        // right-click, and drop target for the root.
        let bg = ui.allocate_rect(ui.available_rect_before_wrap(), egui::Sense::click());
        bg.context_menu(|ui| {
            if ui.button(s::NEW_FILE).clicked() {
                self.create = Some((self.root.clone(), CreateKind::File, String::new()));
                self.focus_create = true;
                ui.close();
            }
            if ui.button(s::NEW_FOLDER).clicked() {
                self.create = Some((self.root.clone(), CreateKind::Folder, String::new()));
                self.focus_create = true;
                ui.close();
            }
            if ui.button(s::REFRESH).clicked() {
                self.schedule_rescan();
                ui.close();
            }
        });
        if bg.hovered() && ui.input(|i| i.pointer.any_released()) {
            if let Some(payload) = egui::DragAndDrop::take_payload::<String>(ui.ctx()) {
                let src = PathBuf::from(payload.as_ref().clone());
                self.move_entry(src, self.root.clone());
            }
        }
    }

    fn draw_entry(&mut self, ui: &mut Ui, entry: &Entry) {
        match entry {
            Entry::File { path, name } => {
                let payload = path.to_string_lossy().into_owned();
                // One widget sensing both click and drag: an extra drag-only
                // widget on top would swallow the clicks (egui hit test).
                let response = ui
                    .selectable_label(false, name)
                    .interact(egui::Sense::click_and_drag());
                if ui.ctx().is_being_dragged(response.id) {
                    egui::DragAndDrop::set_payload(ui.ctx(), payload);
                }
                response.context_menu(|ui| {
                    if ui.button(s::OPEN).clicked() {
                        self.actions.push(Action::Open(path.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(s::DELETE).clicked() {
                        self.actions.push(Action::Delete(path.clone()));
                        ui.close();
                    }
                });
                if response.clicked() {
                    self.actions.push(Action::Open(path.clone()));
                }
            }
            Entry::Dir {
                path,
                name,
                children,
            } => {
                let payload = path.to_string_lossy().into_owned();
                let creating_here = self
                    .create
                    .as_ref()
                    .is_some_and(|(parent, _, _)| parent == path);
                let frame = egui::Frame::new().inner_margin(egui::Margin::same(2));
                let (zone, dropped) = ui.dnd_drop_zone::<String, _>(frame, |ui| {
                    egui::CollapsingHeader::new(name)
                        .id_salt(Id::new(("vrs-folder", payload.clone())))
                        // Keep the folder open while creating inside it,
                        // otherwise the inline input stays hidden.
                        .open(creating_here.then_some(true))
                        .show(ui, |ui| {
                            if creating_here {
                                self.draw_create_row(ui);
                            }
                            for kid in children {
                                self.draw_entry(ui, kid);
                            }
                        })
                        .header_response
                });
                let folder_id = zone.inner.id;
                let header = zone.inner.interact(egui::Sense::click_and_drag());
                // The extra interact widget swallows the click before the
                // CollapsingHeader sees it: toggle the folder ourselves,
                // using the header's own widget id.
                if header.clicked() {
                    egui::containers::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        folder_id,
                        false,
                    )
                    .toggle(ui);
                    ui.ctx().request_repaint();
                }
                if ui.ctx().is_being_dragged(header.id) {
                    egui::DragAndDrop::set_payload(ui.ctx(), payload);
                }
                if let Some(src) = dropped {
                    let src = PathBuf::from(src.as_ref().clone());
                    self.move_entry(src, path.clone());
                }
                header.context_menu(|ui| {
                    if ui.button(s::NEW_FILE).clicked() {
                        self.create = Some((path.clone(), CreateKind::File, String::new()));
                        self.focus_create = true;
                        ui.close();
                    }
                    if ui.button(s::NEW_FOLDER).clicked() {
                        self.create = Some((path.clone(), CreateKind::Folder, String::new()));
                        self.focus_create = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(s::DELETE).clicked() {
                        self.actions.push(Action::Delete(path.clone()));
                        ui.close();
                    }
                });
            }
        }
    }

    fn draw_create_row(&mut self, ui: &mut Ui) {
        let mut confirm = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let Some((_, _, name)) = self.create.as_mut() else {
                return;
            };
            ui.label("+");
            let response = ui.text_edit_singleline(name);
            if self.focus_create {
                response.request_focus();
                self.focus_create = false;
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                confirm = true;
            } else if response.lost_focus()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                cancel = true;
            }
        });
        if cancel {
            self.create = None;
        } else if confirm {
            self.create_now();
        }
    }

    fn create_now(&mut self) {
        let Some((parent, kind, name)) = self.create.take() else {
            return;
        };
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let path = parent.join(name);
        match kind {
            CreateKind::File => {
                let _ = fs::File::create(&path);
            }
            CreateKind::Folder => {
                let _ = fs::create_dir(&path);
            }
        }
        self.schedule_rescan();
    }

    fn move_entry(&mut self, src: PathBuf, dst_dir: PathBuf) {
        if src == dst_dir || dst_dir.starts_with(&src) {
            return;
        }
        let Some(name) = src.file_name() else {
            return;
        };
        let dst = dst_dir.join(name);
        if dst.exists() {
            return;
        }
        if fs::rename(&src, &dst).is_ok() {
            self.actions.push(Action::Moved {
                from: src,
                to: dst,
            });
            self.schedule_rescan();
        }
    }
}

fn scan_dir(dir: &Path) -> Vec<Entry> {
    let mut dirs: Vec<Entry> = Vec::new();
    let mut files: Vec<Entry> = Vec::new();
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // file_type() does not follow symlinks, so we cannot loop.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            dirs.push(Entry::Dir {
                children: scan_dir(&path),
                path,
                name,
            });
        } else if file_type.is_file() {
            files.push(Entry::File { path, name });
        }
    }
    dirs.sort_by(|a, b| entry_name(a).cmp(entry_name(b)));
    files.sort_by(|a, b| entry_name(a).cmp(entry_name(b)));
    dirs.extend(files);
    dirs
}

fn entry_name(entry: &Entry) -> &str {
    match entry {
        Entry::Dir { name, .. } | Entry::File { name, .. } => name,
    }
}
