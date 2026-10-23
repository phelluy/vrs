//! Central text editor panel: a single file at a time.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::Ui;
use egui_code_editor::{CodeEditor, ColorTheme};

use crate::clangd;
use crate::format;
use crate::language::{self, Jump, Language};
use crate::strings as s;
use crate::syntax;

/// Delay between the pointer resting on a word and the hover request. Long
/// enough to spare the server when the pointer just crosses the panel.
const HOVER_DELAY: f64 = 0.35;

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
    language: Language,
    /// Last non-empty selection, as character indices.
    selection: Option<(usize, usize)>,
    /// Caret position to restore once the formatting is done.
    collapse_to: Option<usize>,
    /// Character under the pointer, and when it got there: the hover request
    /// waits for the pointer to rest.
    hover_watch: Option<(usize, f64)>,
}

impl Editor {
    pub fn new(root: PathBuf, ctx: egui::Context) -> Self {
        Self {
            path: None,
            text: String::new(),
            saved_text: String::new(),
            error: None,
            language: Language::new(root, ctx),
            selection: None,
            collapse_to: None,
            hover_watch: None,
        }
    }

    pub fn open(&mut self, path: PathBuf) {
        match fs::read_to_string(&path) {
            Ok(text) => {
                self.path = Some(path);
                self.text = text;
                self.saved_text = self.text.clone();
                self.error = None;
                self.selection = None;
                self.collapse_to = None;
                self.hover_watch = None;
            }
            Err(err) => {
                self.error = Some(format!(
                    "{} {}: {err}",
                    s::CANT_OPEN,
                    Self::file_name(&path)
                ))
            }
        }
    }

    pub fn clear(&mut self) {
        self.path = None;
        self.text.clear();
        self.saved_text.clear();
        self.error = None;
        self.selection = None;
        self.collapse_to = None;
        self.hover_watch = None;
        self.language.close();
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
                // Before drawing: the server may have a new document, and the
                // previous file's diagnostics must not be underlined here.
                self.language.update(path, &self.text);
                let language_error = self.language.error().map(str::to_string);
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
                    } else if let Some(error) = language_error {
                        ui.colored_label(ui.visuals().warn_fg_color, error);
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
                            let explained = self.draw_diagnostics(ui, &output);
                            self.language_ui(ui, &output, explained);
                            self.track_selection(ui, &output);
                            self.format_menu(&output.response.response, path);
                        });
                    });
                if let Some(jump) = self.language.take_jump() {
                    self.goto(jump);
                }
            }
        }
    }

    /// Ask the language server about the character under the pointer: its
    /// documentation on hover, its definition on Ctrl+click.
    ///
    /// Hover waits for the pointer to rest: a request per frame while the
    /// mouse crosses a line would keep the server busy for nothing.
    fn language_ui(&mut self, ui: &Ui, output: &egui::text_edit::TextEditOutput, explained: bool) {
        let response = &output.response.response;
        let pointer = ui.ctx().input(|i| i.pointer.hover_pos());
        let Some(pointer) = pointer.filter(|position| response.rect.contains(*position)) else {
            self.hover_watch = None;
            return;
        };
        let cursor = output.galley.cursor_from_pos(pointer - output.galley_pos);
        let index = usize::from(cursor.index);
        let position = clangd::position(&self.text, index);

        // `command` is Ctrl on Linux and Windows, Cmd on macOS, where
        // Ctrl+click is the usual right-click. Same rule as `Ctrl+S` in
        // `app.rs`, so the editor has one modifier, not two.
        let click = ui.input(|i| i.modifiers.command && i.pointer.primary_clicked());
        if click {
            self.language.request_definition(position);
        }

        // The diagnostic tooltip already explains this spot: two tooltips
        // would pile up.
        if explained {
            self.hover_watch = None;
            return;
        }
        let now = ui.input(|i| i.time);
        match self.hover_watch {
            Some((watched, since)) if watched == index => {
                if now - since >= HOVER_DELAY {
                    self.language.request_hover(position);
                }
            }
            _ => {
                self.hover_watch = Some((index, now));
                // egui only draws when something happens: without this the
                // delay would never elapse under a still pointer.
                ui.ctx().request_repaint_after(Duration::from_millis(120));
            }
        }
        if let Some(text) = self.language.hover_text(position) {
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                response.id,
                egui::PopupAnchor::Pointer,
            )
            .show(|ui| ui.label(text));
        }
    }

    /// Follow a definition, in this file or another one.
    fn goto(&mut self, jump: Jump) {
        let elsewhere = self
            .path
            .as_deref()
            .is_none_or(|path| !language::same_file(path, &jump.path));
        if elsewhere {
            // Ctrl+click must not throw away what was typed: the buffer goes
            // to disk first, as it would before any other file is opened.
            if self.is_dirty() {
                self.save();
            }
            self.open(jump.path);
            if self.path.is_none() {
                return; // unreadable target, the header says why
            }
        }
        // Applied on the next frame, once the widget shows this file.
        self.collapse_to = Some(jump.index);
    }

    /// Right-click menu: hand the selected lines to `clang-format`.
    fn format_menu(&mut self, response: &egui::Response, path: &Path) {
        let selection = self.selection;
        let cpp = language::supported(path);
        response.context_menu(|ui| {
            let selected = selection.is_some_and(|(start, end)| end > start);
            let button = egui::Button::new(s::FORMAT_SELECTION);
            if ui.add_enabled(cpp && selected, button).clicked() {
                if let Some((start, end)) = selection {
                    self.format_lines(start, end);
                }
                ui.close();
            }
        });
    }

    /// Remember the selection while it lasts.
    ///
    /// A right-click collapses the caret in egui (the text edit moves it to
    /// the pointer on any press), and `TextEditOutput::cursor_range` is only
    /// reported while the widget holds the keyboard focus, so the menu reads
    /// the range kept in the widget state on the frame before it opened.
    fn track_selection(&mut self, ui: &Ui, output: &egui::text_edit::TextEditOutput) {
        let id = output.response.response.id;
        // A press elsewhere starts a new selection; keep the old one while a
        // menu is open, otherwise clicking its item would clear it first.
        let popup_open = egui::Popup::is_any_open(ui.ctx());
        let pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        if !popup_open && pressed {
            self.selection = None;
        }
        if let Some(range) = output.state.cursor.char_range() {
            let (a, b): (usize, usize) = (range.primary.index.into(), range.secondary.index.into());
            if a != b {
                self.selection = Some((a.min(b), a.max(b)));
            }
        }
        if let Some(index) = self.collapse_to.take() {
            self.selection = None;
            let index = index.min(self.text.chars().count());
            self.set_cursor(ui, id, index);
            return;
        }
        // Put the range back when the right-click landed inside it: egui
        // drops the highlight on any press, while an editor keeps it.
        // Right-clicking elsewhere still moves the caret, as usual.
        let right_click = ui.input(|i| {
            i.pointer.button_pressed(egui::PointerButton::Secondary)
                || i.pointer.button_released(egui::PointerButton::Secondary)
        });
        if right_click {
            if let Some((start, end)) = self.selection {
                let caret = output
                    .state
                    .cursor
                    .char_range()
                    .map(|range| usize::from(range.primary.index));
                if caret.is_none_or(|caret| start <= caret && caret <= end) {
                    self.set_selection(ui, id, start, end);
                }
            }
        }
    }

    /// Write a character range back into the text edit, so the highlight
    /// follows the text and not just the caret.
    fn set_selection(&self, ui: &Ui, id: egui::Id, start: usize, end: usize) {
        self.set_range(
            ui,
            id,
            egui::text::CCursorRange::two(
                egui::text::CCursor::new(start),
                egui::text::CCursor::new(end),
            ),
        );
    }

    /// Leave a single caret at `index`, without any selection.
    fn set_cursor(&self, ui: &Ui, id: egui::Id, index: usize) {
        self.set_range(
            ui,
            id,
            egui::text::CCursorRange::one(egui::text::CCursor::new(index)),
        );
    }

    fn set_range(&self, ui: &Ui, id: egui::Id, range: egui::text::CCursorRange) {
        let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
        state.cursor.set_char_range(Some(range));
        state.store(ui.ctx(), id);
        // Without the focus egui collapses the range on the next frame.
        ui.ctx().memory_mut(|memory| memory.request_focus(id));
        ui.ctx().request_repaint();
    }

    /// Format the lines spanned by the character range `start..end`.
    fn format_lines(&mut self, start: usize, end: usize) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let lines = self.text.lines().count().max(1);
        let first = format::line_of(&self.text, start).min(lines);
        // `end` is one past the last selected character.
        let last = format::line_of(&self.text, end.saturating_sub(1).max(start));
        let last = last.max(first).min(lines);
        match format::format_range(&path, &self.text, first, last) {
            Ok(text) => {
                self.text = text;
                self.error = None;
                // The old range no longer matches the new text: leave a single
                // caret where the selection used to end.
                self.collapse_to = self.selection.map(|(_, end)| end);
            }
            Err(err) => self.error = Some(format!("{}: {err}", s::CANT_FORMAT)),
        }
    }

    /// Highlight each diagnostic and explain it on hover.
    /// Positions come from the text galley itself, so they follow the font,
    /// the scroll offset and the glyph advances without arithmetic.
    ///
    /// Returns true when a message is showing: the hover of the language
    /// server would otherwise open a second tooltip on the same spot.
    fn draw_diagnostics(&self, ui: &Ui, output: &egui::text_edit::TextEditOutput) -> bool {
        let mut explained = false;
        let galley = &output.galley;
        let origin = output.galley_pos;
        for (index, diagnostic) in self.language.diagnostics().iter().enumerate() {
            let Some(row) = galley.rows.get(diagnostic.line.saturating_sub(1)) else {
                continue;
            };
            let glyphs = &row.row.glyphs;
            let Some(last) = glyphs.last() else {
                continue;
            };
            // Past the end of a short line, clamp to its last character.
            let first = diagnostic.column.saturating_sub(1);
            let Some(glyph) = glyphs.get(first).or(glyphs.last()) else {
                continue;
            };
            let start = glyph.pos.x;
            // The server gives the range of the offending token. It is
            // exclusive, so the last underlined character is the one before
            // `end_column`; a range spilling onto the next lines stops at the
            // end of this one.
            let mut last_index = if diagnostic.end_line == diagnostic.line {
                diagnostic.end_column.saturating_sub(2)
            } else {
                glyphs.len().saturating_sub(1)
            }
            .max(first)
            .min(glyphs.len().saturating_sub(1));
            // A caret-sized range says nothing about its extent: mark the
            // whole identifier it points at.
            if diagnostic.end_line == diagnostic.line && diagnostic.end_column <= diagnostic.column
            {
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                if is_word(glyph.chr) {
                    while let Some(next) = glyphs.get(last_index + 1) {
                        if !is_word(next.chr) {
                            break;
                        }
                        last_index += 1;
                    }
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
            explained |= response.hovered();
            let _ = response.on_hover_text(&diagnostic.message);
        }
        explained
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use crate::{clangd, language};
    use std::fs;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vrs-editor-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir.join(name)
    }

    /// An editor rooted in the temporary directory. Building one does not
    /// start the language server: it starts on the first frame.
    fn editor() -> Editor {
        let root = std::env::temp_dir().join(format!("vrs-editor-{}", std::process::id()));
        fs::create_dir_all(&root).expect("temp dir");
        Editor::new(root, egui::Context::default())
    }

    #[test]
    fn save_writes_the_text_and_clears_dirty() {
        let path = temp_path("save.txt");
        fs::write(&path, "a\n").expect("write");
        let mut editor = editor();
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
        let mut editor = editor();
        editor.open(path.clone());
        assert!(editor.path.is_none());
        assert!(editor.error.is_some());
        let _ = fs::remove_file(path);
    }

    /// Draw a few frames headless, until the language server has underlined
    /// something: the panel, the gutter and the diagnostic overlay all paint
    /// without a window. Skips when clangd is not installed.
    #[test]
    fn draws_diagnostics_without_a_window() {
        let path = temp_path("frame.cpp");
        fs::write(&path, "int main() {\n    return oops;\n}\n").expect("write");
        let mut editor = editor();
        editor.open(path.clone());
        let ctx = egui::Context::default();
        let mut underlined = false;
        for _ in 0..40 {
            let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| editor.ui(ui));
            // Nobody uploads textures in a test; dropping them unapplied
            // panics on purpose.
            frame.textures_delta.clear();
            if editor.language.error().is_some() {
                break; // no clangd on this machine
            }
            underlined = !editor.language.diagnostics().is_empty();
            if underlined {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if editor.language.error().is_none() {
            assert!(underlined, "clangd never reported the error on `oops`");
        }
        let _ = fs::remove_file(path);
    }

    /// Rest a headless pointer on a word, then Ctrl+click it: hover and
    /// definition, from the pointer to the other file. Skips when clangd is
    /// not installed.
    #[test]
    fn the_pointer_hovers_and_follows_a_definition() {
        let path = temp_path("main.cpp");
        let header = temp_path("util.h");
        let text = "#include \"util.h\"\n\nint main() {\n    return helper();\n}\n";
        fs::write(&path, text).expect("write");
        fs::write(&header, "int helper();\n\nint helper() { return 1; }\n").expect("write");
        let mut editor = editor();
        editor.open(path.clone());
        let target = text.find("helper").expect("helper") + 1;
        let position = clangd::position(text, target);
        let ctx = egui::Context::default();

        // Scan the panel for the screen point that lands on `helper`.
        let mut spot = None;
        'scan: for y in (25..200).step_by(4) {
            for x in (16..800).step_by(3) {
                let pos = egui::pos2(x as f32, y as f32);
                draw(&ctx, &mut editor, Some(pos));
                if editor.hover_watch.map(|(index, _)| index) == Some(target) {
                    spot = Some(pos);
                    break 'scan;
                }
            }
        }
        let spot = spot.expect("no pixel shows `helper`");

        // Rest there until the server answers, then let the tooltip draw.
        let mut documentation = None;
        for _ in 0..40 {
            draw(&ctx, &mut editor, Some(spot));
            if let Some(text) = editor.language.hover_text(position) {
                documentation = Some(text.to_string());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // One more frame with the answer in hand: this is the tooltip.
        draw(&ctx, &mut editor, Some(spot));
        if editor.language.error().is_some() {
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(header);
            return; // no clangd on this machine
        }
        let documentation = documentation.expect("no hover answer for `helper`");
        assert!(
            documentation.contains("helper"),
            "unexpected hover: {documentation}"
        );

        // Ctrl+click on the same word follows its declaration.
        click(&ctx, &mut editor, spot, true);
        click(&ctx, &mut editor, spot, false);
        for _ in 0..40 {
            draw(&ctx, &mut editor, Some(spot));
            if editor.path.as_deref() != Some(path.as_path()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let opened = editor.path.clone().expect("a file is open");
        let shown = editor.text.clone();
        assert!(
            language::same_file(&opened, &header),
            "Ctrl+click opened {opened:?} instead of the header"
        );
        assert!(shown.contains("int helper();"), "unexpected text: {shown}");
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(header);
    }

    /// Run one headless frame with the pointer at `pointer`.
    fn draw(ctx: &egui::Context, editor: &mut Editor, pointer: Option<egui::Pos2>) {
        frame(ctx, editor, pointer, Vec::new(), egui::Modifiers::NONE);
    }

    /// One frame with a Ctrl+click (Cmd on macOS) on `pos`.
    fn click(ctx: &egui::Context, editor: &mut Editor, pos: egui::Pos2, pressed: bool) {
        let button = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::COMMAND,
        };
        frame(
            ctx,
            editor,
            Some(pos),
            vec![button],
            egui::Modifiers::COMMAND,
        );
    }

    fn frame(
        ctx: &egui::Context,
        editor: &mut Editor,
        pointer: Option<egui::Pos2>,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) {
        // egui tracks the modifiers through events, not a field.
        let mut all = vec![egui::Event::ModifiersChanged(modifiers)];
        all.extend(pointer.map(egui::Event::PointerMoved));
        all.extend(events);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 700.0),
            )),
            events: all,
            ..Default::default()
        };
        let mut frame = ctx.run_ui(input, |ui| editor.ui(ui));
        // Nobody uploads textures in a test, and dropping them unapplied
        // panics on purpose.
        frame.textures_delta.clear();
    }
}
