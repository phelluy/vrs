//! Bottom panel: a real interactive terminal. Keystrokes are passed straight
//! through to the system shell via a PTY (so readline provides completion,
//! history, line editing), and the shell's output is rendered through a
//! vt100 emulator (colors, cursor, scrollback).
//!
//! Output bytes are parsed in the reader thread (like a real terminal
//! emulator does): the kernel PTY buffer then applies natural backpressure
//! on runaway programs, and the UI thread only renders the grid.

use std::io::Write;
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::{Arc, Mutex};

use egui::Ui;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const SCROLLBACK_CAPACITY: usize = 10_000;

pub struct Terminal {
    error: Option<String>,
    parser: Arc<Mutex<vt100::Parser>>,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    focus_input: bool,
    /// Whether the terminal owned the keyboard on the last frame.
    focused: bool,
    /// Desired size waiting to be stable before it is applied to the PTY.
    pending_size: Option<(u16, u16)>,
    pending_since: f64,
    cols: u16,
    rows: u16,
}

impl Terminal {
    /// The screen parser. A panic in the reader thread poisons the mutex;
    /// the terminal keeps rendering the last known state instead of taking
    /// the whole UI down.
    fn parser(&self) -> std::sync::MutexGuard<'_, vt100::Parser> {
        self.parser.lock().unwrap_or_else(|err| err.into_inner())
    }

    pub fn new(cwd: &Path, ctx: egui::Context) -> Self {
        let mut terminal = Terminal {
            error: None,
            parser: Arc::new(Mutex::new(vt100::Parser::new(
                nz(24),
                nz(80),
                SCROLLBACK_CAPACITY,
            ))),
            master: None,
            writer: None,
            child: None,
            focus_input: false,
            focused: false,
            pending_size: None,
            pending_since: 0.0,
            cols: 80,
            rows: 24,
        };
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
        let pty_system = native_pty_system();
        let pair = match pty_system.openpty(PtySize {
            rows: terminal.rows,
            cols: terminal.cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(pair) => pair,
            Err(err) => {
                terminal.error = Some(err.to_string());
                return terminal;
            }
        };
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        let child = match pair.slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(err) => {
                terminal.error = Some(err.to_string());
                return terminal;
            }
        };
        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(err) => {
                terminal.error = Some(err.to_string());
                return terminal;
            }
        };
        terminal.writer = pair.master.take_writer().ok();
        let parser = Arc::clone(&terminal.parser);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        parser
                            .lock()
                            .unwrap_or_else(|err| err.into_inner())
                            .process(&chunk[..n]);
                        // New shell output must trigger a repaint: egui is
                        // reactive and would otherwise never show it.
                        ctx.request_repaint();
                    }
                }
            }
        });
        terminal.master = Some(pair.master);
        terminal.child = Some(child);
        terminal
    }

    /// Whether the terminal owned the keyboard on the last frame. The shell
    /// shortcut handler uses it to leave Ctrl+S to the shell when the user is
    /// typing down here.
    pub fn has_focus(&self) -> bool {
        self.focused
    }

    /// Draw the terminal. Returns a height delta while the user drags the
    /// top edge to resize the panel.
    pub fn ui(&mut self, ui: &mut Ui) -> Option<f32> {
        if let Some(error) = &self.error {
            ui.label(error);
            return None;
        }
        // Clicking anywhere in the terminal panel focuses the keyboard
        // capture, like in a real terminal.
        let panel_rect = ui.max_rect();
        if ui.input(|i| {
            i.pointer.primary_clicked()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| panel_rect.contains(p))
        }) {
            self.focus_input = true;
        }
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let (char_width, row_height) =
            ui.fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)));

        // Drag handle along the top edge, to resize the panel.
        let (_, handle) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 6.0),
            egui::Sense::drag(),
        );
        let handle = handle.on_hover_cursor(egui::CursorIcon::ResizeNorth);
        let mut drag_delta = None;
        if handle.dragged() {
            drag_delta = Some(ui.input(|i| i.pointer.delta().y));
        }

        let available = ui.available_size();
        let rows = ((available.y / row_height).floor().max(3.0)) as u16;
        let cols = ((available.x / char_width).floor().max(10.0)) as u16;
        self.schedule_resize(ui, rows, cols);

        // Mouse-wheel scrollback, only when the pointer is over the panel.
        let over_panel = ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| panel_rect.contains(p));
        if over_panel {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                let mut parser = self.parser();
                let screen = parser.screen_mut();
                let offset = screen.scrollback() as f32 + wheel / row_height;
                let max = screen.scrollback_len() as f32;
                screen.set_scrollback(offset.clamp(0.0, max).round() as usize);
            }
        }

        let fg = ui.visuals().text_color();
        // Backgrounds are painted behind the glyphs; the panel fill is the
        // "no background" colour, and the one an inverse cell swaps with.
        let bg = ui.visuals().panel_fill;
        // The grid rows are drawn edge to edge: no vertical spacing between
        // them, so the last line is not pushed out of the panel.
        ui.spacing_mut().item_spacing.y = 0.0;
        {
            let parser = self.parser();
            let screen = parser.screen();
            let (screen_rows, screen_cols) = screen.size();
            let (screen_rows, screen_cols) = (screen_rows.get(), screen_cols.get());
            let (cursor_row, cursor_col) = screen.cursor_position();
            let show_cursor = !screen.hide_cursor() && screen.scrollback() == 0;
            for row in 0..screen_rows {
                let mut job = egui::text::LayoutJob::default();
                for run in row_runs(screen, row, screen_cols, fg, bg) {
                    // egui has no bold face for the monospace family, so SGR
                    // bold is left as colour only.
                    let mut format =
                        egui::text::TextFormat::simple(font_id.clone(), run.color);
                    if run.background != bg {
                        format.background = run.background;
                    }
                    if run.underline {
                        format.underline = egui::Stroke::new(1.0, run.color);
                    }
                    job.append(&run.text, 0.0, format);
                }
                let response = ui.add(egui::Label::new(job));
                if show_cursor && row == cursor_row {
                    let pos =
                        response.rect.left_top() + egui::vec2(cursor_col as f32 * char_width, 0.0);
                    let rect = egui::Rect::from_min_size(pos, egui::vec2(2.0, row_height));
                    ui.painter().rect_filled(rect, 0.0, fg);
                }
            }
        }

        // Invisible keyboard capture, placed over the first pixel of the
        // panel: the shell draws the command line itself through the PTY,
        // we only route the keys to it. A plain interact widget (not a
        // TextEdit) so nothing is rendered when focused.
        let capture_rect =
            egui::Rect::from_min_size(panel_rect.left_top(), egui::vec2(1.0, 1.0));
        let capture_id = egui::Id::new("vrs-terminal-capture");
        let response = ui.interact(capture_rect, capture_id, egui::Sense::click());
        ui.ctx().memory_mut(|m| {
            m.set_focus_lock_filter(
                capture_id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        if self.focus_input {
            response.request_focus();
            self.focus_input = false;
        }
        self.focused = response.has_focus();
        if self.focused {
            let events = ui.input(|i| i.events.clone());
            // readline and friends switch bracketed paste on through the PTY.
            let bracketed = self.parser().screen().bracketed_paste();
            for event in &events {
                let bytes = match event {
                    egui::Event::Paste(text) => Some(paste_bytes(text, bracketed)),
                    other => key_bytes(other),
                };
                if let Some(bytes) = bytes {
                    self.parser().screen_mut().set_scrollback(0);
                    self.send_bytes(&bytes);
                }
            }
        }
        drag_delta
    }

    /// Resize the PTY and the emulator only once the desired size has been
    /// stable for a while, so dragging the panel edge does not spam the
    /// shell with SIGWINCH.
    fn schedule_resize(&mut self, ui: &egui::Ui, rows: u16, cols: u16) {
        if (rows, cols) == (self.rows, self.cols) {
            self.pending_size = None;
            return;
        }
        let now = ui.input(|i| i.time);
        if self.pending_size != Some((rows, cols)) {
            self.pending_size = Some((rows, cols));
            self.pending_since = now;
        }
        if now - self.pending_since < 0.25 {
            // The delay only elapses if a frame runs, and egui is reactive:
            // without this the PTY would keep its stale size at an idle
            // prompt.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.pending_size = None;
        self.parser().set_size(nz(rows), nz(cols));
        if let Some(master) = &self.master {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        if let Some(writer) = &mut self.writer {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).expect("nonzero")
}

/// A stretch of one screen row sharing the same attributes.
struct Run {
    text: String,
    color: egui::Color32,
    background: egui::Color32,
    underline: bool,
}

/// Split a screen row into runs of identical attributes. Trailing blanks are
/// dropped unless they carry a background: a coloured bar keeps its width.
fn row_runs(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
    fg: egui::Color32,
    bg: egui::Color32,
) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        let mut color = to_egui_color(cell.fgcolor(), fg);
        let mut background = to_egui_color(cell.bgcolor(), bg);
        // Reverse video (vim status lines, htop bars).
        if cell.inverse() {
            std::mem::swap(&mut color, &mut background);
        }
        let underline = cell.underline();
        match runs.last_mut() {
            Some(run)
                if run.color == color
                    && run.background == background
                    && run.underline == underline =>
            {
                run.text.push_str(cell.contents());
            }
            _ => runs.push(Run {
                text: cell.contents().to_string(),
                color,
                background,
                underline,
            }),
        }
    }
    if let Some(last) = runs.last_mut() {
        if last.background == bg {
            while last.text.ends_with(' ') {
                last.text.pop();
            }
        }
    }
    runs.retain(|run| !run.text.is_empty());
    runs
}

/// Map a vt100 color to an egui color, on a light theme.
fn to_egui_color(color: vt100::Color, default: egui::Color32) -> egui::Color32 {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => egui::Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i) => ansi_palette(i),
    }
}

const XTERM_16: [egui::Color32; 16] = [
    egui::Color32::from_rgb(0x00, 0x00, 0x00),
    egui::Color32::from_rgb(0xcd, 0x00, 0x00),
    egui::Color32::from_rgb(0x00, 0xcd, 0x00),
    egui::Color32::from_rgb(0xcd, 0xcd, 0x00),
    egui::Color32::from_rgb(0x00, 0x00, 0xee),
    egui::Color32::from_rgb(0xcd, 0x00, 0xcd),
    egui::Color32::from_rgb(0x00, 0xcd, 0xcd),
    egui::Color32::from_rgb(0xe5, 0xe5, 0xe5),
    egui::Color32::from_rgb(0x7f, 0x7f, 0x7f),
    egui::Color32::from_rgb(0xff, 0x00, 0x00),
    egui::Color32::from_rgb(0x00, 0xff, 0x00),
    egui::Color32::from_rgb(0xff, 0xff, 0x00),
    egui::Color32::from_rgb(0x5c, 0x5c, 0xff),
    egui::Color32::from_rgb(0xff, 0x00, 0xff),
    egui::Color32::from_rgb(0x00, 0xff, 0xff),
    egui::Color32::from_rgb(0xff, 0xff, 0xff),
];

/// Map an xterm 256-color index to a color.
fn ansi_palette(i: u8) -> egui::Color32 {
    if i < 16 {
        XTERM_16[i as usize]
    } else if i < 232 {
        // 6x6x6 color cube.
        let v = i - 16;
        let levels = [0, 95, 135, 175, 215, 255];
        let r = levels[(v / 36) as usize];
        let g = levels[((v / 6) % 6) as usize];
        let b = levels[(v % 6) as usize];
        egui::Color32::from_rgb(r, g, b)
    } else {
        // Grayscale ramp.
        let v = 8 + 10 * (i - 232);
        egui::Color32::from_rgb(v, v, v)
    }
}

/// Bytes to send for a clipboard paste. Newlines become carriage returns,
/// like a real terminal, and the text is wrapped in bracketed-paste markers
/// when the running program asked for that mode: readline then inserts it
/// instead of executing every line.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// Translate an egui key event into the bytes a real terminal would send.
fn key_bytes(event: &egui::Event) -> Option<Vec<u8>> {
    match event {
        // egui turns Ctrl+C / Ctrl+X into Copy / Cut commands before we see
        // the raw key; in a terminal they mean SIGINT / ^X.
        egui::Event::Copy => Some(vec![0x03]),
        egui::Event::Cut => Some(vec![0x18]),
        egui::Event::Text(text) => Some(text.as_bytes().to_vec()),
        // `repeat` is deliberately not constrained: a held Backspace or arrow
        // key must send its byte again on every auto-repeat event.
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            if modifiers.ctrl && !modifiers.shift && !modifiers.alt {
                let code = match key {
                    egui::Key::A => 0x01,
                    egui::Key::B => 0x02,
                    egui::Key::D => 0x04,
                    egui::Key::E => 0x05,
                    egui::Key::F => 0x06,
                    egui::Key::G => 0x07,
                    egui::Key::H => 0x08,
                    egui::Key::J => 0x0a,
                    egui::Key::K => 0x0b,
                    egui::Key::L => 0x0c,
                    egui::Key::N => 0x0e,
                    egui::Key::O => 0x0f,
                    egui::Key::P => 0x10,
                    egui::Key::Q => 0x11,
                    egui::Key::R => 0x12,
                    egui::Key::S => 0x13,
                    egui::Key::T => 0x14,
                    egui::Key::U => 0x15,
                    egui::Key::V => 0x16,
                    egui::Key::W => 0x17,
                    egui::Key::X => 0x18,
                    egui::Key::Y => 0x19,
                    egui::Key::Z => 0x1a,
                    egui::Key::OpenBracket => 0x1b,
                    egui::Key::Backslash => 0x1c,
                    egui::Key::CloseBracket => 0x1d,
                    _ => return None,
                };
                return Some(vec![code]);
            }
            if modifiers.shift || modifiers.alt || modifiers.command {
                return None;
            }
            match key {
                egui::Key::Enter => Some(b"\r".to_vec()),
                egui::Key::Backspace => Some(b"\x7f".to_vec()),
                egui::Key::Tab => Some(b"\t".to_vec()),
                egui::Key::Escape => Some(b"\x1b".to_vec()),
                egui::Key::ArrowUp => Some(b"\x1b[A".to_vec()),
                egui::Key::ArrowDown => Some(b"\x1b[B".to_vec()),
                egui::Key::ArrowRight => Some(b"\x1b[C".to_vec()),
                egui::Key::ArrowLeft => Some(b"\x1b[D".to_vec()),
                egui::Key::Home => Some(b"\x1b[H".to_vec()),
                egui::Key::End => Some(b"\x1b[F".to_vec()),
                egui::Key::Delete => Some(b"\x1b[3~".to_vec()),
                egui::Key::Insert => Some(b"\x1b[2~".to_vec()),
                egui::Key::PageUp => Some(b"\x1b[5~".to_vec()),
                egui::Key::PageDown => Some(b"\x1b[6~".to_vec()),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::ansi_palette;
    use super::key_bytes;
    use super::paste_bytes;
    use super::row_runs;

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn repeated(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn enter_sends_carriage_return() {
        assert_eq!(
            key_bytes(&key(egui::Key::Enter, egui::Modifiers::NONE)),
            Some(b"\r".to_vec())
        );
    }

    #[test]
    fn tab_sends_tab_byte() {
        assert_eq!(
            key_bytes(&key(egui::Key::Tab, egui::Modifiers::NONE)),
            Some(b"\t".to_vec())
        );
    }

    #[test]
    fn ctrl_c_sends_intr() {
        // egui reports Ctrl+C as a Copy command, not as a key event.
        assert_eq!(key_bytes(&egui::Event::Copy), Some(vec![0x03]));
    }

    #[test]
    fn ctrl_x_sends_ctrl_x_byte() {
        assert_eq!(key_bytes(&egui::Event::Cut), Some(vec![0x18]));
    }

    #[test]
    fn arrows_send_escape_sequences() {
        assert_eq!(
            key_bytes(&key(egui::Key::ArrowUp, egui::Modifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn auto_repeat_sends_the_byte_again() {
        // Holding Backspace must delete more than one character.
        assert_eq!(key_bytes(&repeated(egui::Key::Backspace)), Some(vec![0x7f]));
        assert_eq!(
            key_bytes(&repeated(egui::Key::ArrowLeft)),
            Some(b"\x1b[D".to_vec())
        );
    }

    #[test]
    fn paste_newlines_become_carriage_returns() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb".to_vec());
    }

    #[test]
    fn paste_is_wrapped_in_bracketed_paste_when_enabled() {
        assert_eq!(
            paste_bytes("a\nb", true),
            b"\x1b[200~a\rb\x1b[201~".to_vec()
        );
    }

    fn screen_of(bytes: &[u8]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(2).expect("rows"),
            std::num::NonZeroU16::new(10).expect("cols"),
            0,
        );
        parser.process(bytes);
        parser
    }

    const FG: egui::Color32 = egui::Color32::from_rgb(0, 0, 0);
    const BG: egui::Color32 = egui::Color32::from_rgb(0xf8, 0xf8, 0xf8);

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let parser = screen_of(b"\x1b[7mINV\x1b[0m");
        let runs = row_runs(parser.screen(), 0, 10, FG, BG);
        assert_eq!(runs[0].text, "INV");
        assert_eq!(runs[0].color, BG);
        assert_eq!(runs[0].background, FG);
    }

    #[test]
    fn explicit_background_is_kept_without_trimming() {
        // A blue bar made of blanks must keep its width.
        let parser = screen_of(b"\x1b[44m  \x1b[0m x");
        let runs = row_runs(parser.screen(), 0, 10, FG, BG);
        assert_eq!(runs[0].text, "  ");
        assert_eq!(runs[0].background, ansi_palette(4));
        assert_eq!(runs[1].text, " x");
        assert_eq!(runs[1].background, BG);
    }

    #[test]
    fn plain_row_is_one_run_without_trailing_blanks() {
        let parser = screen_of(b"hello");
        let runs = row_runs(parser.screen(), 0, 10, FG, BG);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "hello");
        assert_eq!(runs[0].background, BG);
    }

    #[test]
    fn text_is_passed_through() {
        assert_eq!(
            key_bytes(&egui::Event::Text("ls".into())),
            Some(b"ls".to_vec())
        );
    }

    #[test]
    fn palette_covers_base_and_cube() {
        assert_eq!(
            ansi_palette(1),
            egui::Color32::from_rgb(0xcd, 0x00, 0x00)
        );
        assert_eq!(
            ansi_palette(196),
            egui::Color32::from_rgb(0xff, 0x00, 0x00)
        );
        assert_eq!(ansi_palette(255), egui::Color32::from_rgb(238, 238, 238));
    }
}
