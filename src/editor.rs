//! Central text editor panel: a single file at a time.

use std::fs;
use std::path::{Path, PathBuf};

use egui::Ui;
use egui_code_editor::{CodeEditor, ColorTheme};

use crate::diagnostics::Linter;
use crate::strings as s;
use crate::syntax;

/// Highlight laid over a diagnostic, like a highlighter pen: the alpha lets
/// the code show through. Values are premultiplied (rgb already scaled by the
/// alpha), which is the only const constructor egui offers.
/// Red = rgb(255, 90, 90) at 37 % ; yellow = rgb(255, 225, 60) at 51 %.
const ERROR_MARK: egui::Color32 = egui::Color32::from_rgba_premultiplied(95, 34, 34, 95);
const WARNING_MARK: egui::Color32 = egui::Color32::from_rgba_premultiplied(130, 115, 31, 130);

pub struct Editor {
    pub path: Option<PathBuf>,
    pub text: String,
    saved_text: String,
    /// Last filesystem failure (unreadable or unwritable file), shown in the
    /// header instead of failing silently.
    error: Option<String>,
    linter: Linter,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            text: String::new(),
            saved_text: String::new(),
            error: None,
            linter: Linter::new(),
        }
    }

    pub fn open(&mut self, path: PathBuf) {
        match fs::read_to_string(&path) {
            Ok(text) => {
                self.path = Some(path);
                self.text = text;
                self.saved_text = self.text.clone();
                self.error = None;
                self.linter.reset();
            }
            Err(err) => {
                self.error = Some(format!("{} {}: {err}", s::CANT_OPEN, Self::file_name(&path)))
            }
        }
    }

    pub fn clear(&mut self) {
        self.path = None;
        self.text.clear();
        self.saved_text.clear();
        self.error = None;
        self.linter.reset();
    }

    pub fn is_dirty(&self) -> bool {
        self.text != self.saved_text
    }

    pub fn save(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        match fs::write(path, &self.text) {
            Ok(()) => {
                self.saved_text = self.text.clone();
                self.error = None;
            }
            Err(err) => {
                self.error = Some(format!("{} {}: {err}", s::CANT_SAVE, Self::file_name(path)))
            }
        }
    }

    fn file_name(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let path = self.path.clone();
        match &path {
            None => {
                // A file that failed to open leaves no path behind: the
                // error must show up even with an empty editor.
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.centered_and_justified(|ui| {
                    ui.weak(s::NO_FILE_OPEN);
                });
            }
            Some(path) => {
                ui.horizontal(|ui| {
                    ui.strong(Self::file_name(path));
                    if self.is_dirty() {
                        ui.label(egui::RichText::new(s::MODIFIED).weak());
                        if ui.button(s::SAVE).clicked() {
                            self.save();
                        }
                    }
                    if let Some(error) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                });
                ui.add_space(4.0);
                let id = format!("vrs-editor-{}", path.display());
                let syntax = syntax::for_path(path);
                // Our own line-number gutter, drawn only for the lines that
                // exist (egui_code_editor pads the gutter up to `rows`).
                // The editor itself has numlines and vscroll disabled, so the
                // gutter and the text scroll together in our scroll area.
                let n_lines = self.text.lines().count().max(1);
                let font_id = egui::FontId::monospace(14.0);
                let row_height = ui.fonts_mut(|f| f.row_height(&font_id));
                let panel_rows = (ui.available_size().y / row_height).ceil() as usize;
                let rows = panel_rows.max(n_lines).max(10);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.add_space(3.0); // align with the text edit
                                let digits = n_lines.to_string().len();
                                for i in 1..=n_lines {
                                    ui.label(
                                        egui::RichText::new(format!("{i:>digits$}"))
                                            .monospace()
                                            .size(14.0)
                                            .weak(),
                                    );
                                }
                            });
                            let mut code = CodeEditor::default()
                                .id_source(id)
                                .with_theme(ColorTheme::GITHUB_LIGHT)
                                .with_fontsize(14.0)
                                .with_numlines(false)
                                .vscroll(false)
                                .with_rows(rows);
                            let (output, _tokens) = code.show(ui, &mut self.text, &syntax);
                            self.draw_diagnostics(ui, &output);
                        });
                    });
                self.linter.update(ui.ctx(), path, &self.text);
            }
        }
    }

    /// Highlight each diagnostic and explain it on hover.
    /// Positions come from the text galley itself, so they follow the font,
    /// the scroll offset and the glyph advances without arithmetic.
    fn draw_diagnostics(&self, ui: &Ui, output: &egui::text_edit::TextEditOutput) {
        if self.linter.diagnostics().is_empty() {
            return;
        }
        let galley = &output.galley;
        let origin = output.galley_pos;
        for (index, diagnostic) in self.linter.diagnostics().iter().enumerate() {
            let Some(row) = galley.rows.get(diagnostic.line.saturating_sub(1)) else {
                continue;
            };
            let glyphs = &row.row.glyphs;
            let Some(last) = glyphs.last() else {
                continue;
            };
            let first = (diagnostic.column.saturating_sub(1)).min(glyphs.len() - 1);
            let start = glyphs[first].pos.x;
            // The compiler gives the start column only: mark the whole
            // identifier when the diagnostic points at one.
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let mut last_index = first;
            if is_word(glyphs[first].chr) {
                while let Some(next) = glyphs.get(last_index + 1) {
                    if !is_word(next.chr) {
                        break;
                    }
                    last_index += 1;
                }
            }
            let end = match glyphs.get(last_index + 1) {
                Some(next) => next.pos.x,
                None => last.pos.x + last.advance_width,
            };
            let top = origin.y + row.pos.y;
            let bottom = top + row.row.size.y;
            let left = origin.x + start;
            let right = origin.x + end.max(start + 2.0);

            let color = if diagnostic.warning {
                WARNING_MARK
            } else {
                ERROR_MARK
            };
            let rect = egui::Rect::from_min_max(
                egui::pos2(left - 1.0, top + 2.0),
                egui::pos2(right.max(left + 2.0) + 1.0, bottom - 2.0),
            );
            ui.painter().rect_filled(rect, 3.0, color);
            let response = ui.interact(
                rect.expand(1.0),
                ui.id().with(("vrs-diagnostic", index)),
                egui::Sense::hover(),
            );
            let _ = response.on_hover_text(&diagnostic.message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use std::fs;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vrs-editor-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir.join(name)
    }

    #[test]
    fn save_writes_the_text_and_clears_dirty() {
        let path = temp_path("save.txt");
        fs::write(&path, "a\n").expect("write");
        let mut editor = Editor::new();
        editor.open(path.clone());
        assert!(!editor.is_dirty());
        editor.text.push('b');
        assert!(editor.is_dirty());
        editor.save();
        assert!(!editor.is_dirty());
        assert_eq!(fs::read_to_string(&path).expect("read"), "a\nb");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn open_reports_a_read_failure() {
        let path = temp_path("binary.bin");
        fs::write(&path, [0xff, 0xfe, 0x00]).expect("write");
        let mut editor = Editor::new();
        editor.open(path.clone());
        assert!(editor.path.is_none());
        assert!(editor.error.is_some());
        let _ = fs::remove_file(path);
    }
}
