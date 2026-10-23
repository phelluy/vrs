//! Minimal client for `clangd`, the C++ language server.
//!
//! clangd speaks the Language Server Protocol over its standard input and
//! output: JSON-RPC messages framed by a `Content-Length` header. Only the few
//! exchanges the editor needs are implemented — open and change a document,
//! receive its diagnostics, ask for a hover or a definition — so the protocol
//! layer stays small, with `serde_json` as its only dependency.
//!
//! A [`Server`] owns the process and two threads: one pumps frames from a
//! channel into its stdin, one reads its stdout and turns frames into
//! [`Event`]s. Neither thread draws anything; answers are collected by
//! [`Server::poll`] on the next frame, which the reader asks for itself
//! (`Context::request_repaint`): egui is idle when nothing is typed and would
//! never notice that a diagnostic arrived.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

use serde_json::{json, Value};

/// A position as the protocol defines it: a 0-based line, and a column counted
/// in UTF-16 code units (not bytes, not characters).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A range as the protocol defines it.
#[derive(Clone, Copy, Debug)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub range: Range,
    pub message: String,
    pub warning: bool,
}

#[derive(Clone, Debug)]
pub struct Location {
    pub path: PathBuf,
    pub position: Position,
}

/// What the editor has to react to, once a frame has been read.
pub enum Event {
    Diagnostics {
        path: PathBuf,
        items: Vec<Diagnostic>,
    },
    Hover {
        position: Position,
        text: Option<String>,
    },
    Definition {
        location: Option<Location>,
    },
}

/// What a request in flight is about, so its answer can be recognised.
enum Request {
    Hover(Position),
    Definition,
}

/// Message from the reader thread, before the request map is consulted.
enum ToUi {
    Diagnostics {
        path: PathBuf,
        items: Vec<Diagnostic>,
    },
    Response {
        id: i64,
        result: Value,
    },
}

pub struct Server {
    project: PathBuf,
    child: Child,
    to_child: Sender<Value>,
    events: Receiver<ToUi>,
    pending: HashMap<i64, Request>,
    next_id: i64,
    /// `uri` of the document clangd has open, and its version.
    document: Option<String>,
    version: i64,
}

impl Server {
    /// Start `clangd` on `project`: the directory it uses as workspace root,
    /// where it looks for `.clangd` and `compile_commands.json`.
    pub fn spawn(project: &Path, ctx: egui::Context) -> io::Result<Self> {
        let mut child = Command::new("clangd")
            // Nothing on stderr is meant for the student, and an unread pipe
            // would eventually block the server.
            .arg("--log=error")
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(io::Error::other("clangd: no stdio"));
        };

        let (to_child, frames) = mpsc::channel();
        let (to_ui, events) = mpsc::channel();
        std::thread::spawn(move || write_frames(stdin, frames));
        let replies = to_child.clone();
        std::thread::spawn(move || read_frames(stdout, replies, to_ui, ctx));

        let mut server = Self {
            project: project.to_path_buf(),
            child,
            to_child,
            events,
            pending: HashMap::new(),
            next_id: 1,
            document: None,
            version: 0,
        };

        let root = uri(project);
        let name = project
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = server.take_id();
        server.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": { "name": crate::strings::APP_NAME },
                "rootUri": root,
                "workspaceFolders": [{ "uri": root, "name": name }],
                "capabilities": {
                    // UTF-16 is the protocol default, and the one the editor
                    // converts to character indices.
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": {
                        "hover": { "contentFormat": ["plaintext"] },
                        "definition": {},
                        "publishDiagnostics": { "versionSupport": true }
                    }
                }
            }
        }));
        server.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        Ok(server)
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    /// True while the process is alive. A crashed server is not restarted
    /// behind the user's back; the next project opened starts a new one.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Hand the buffer of `path` to the server, replacing whatever it had.
    pub fn open(&mut self, path: &Path, text: &str) {
        self.close();
        let document = uri(path);
        self.version = 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": document,
                "languageId": language_id(path),
                "version": 1,
                "text": text
            }}
        }));
        self.document = Some(document);
    }

    /// Send the whole buffer again. Full text, unlike incremental deltas: the
    /// file is small, and nothing can drift out of sync.
    pub fn change(&mut self, text: &str) {
        let Some(document) = self.document.clone() else {
            return;
        };
        self.version += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": document, "version": self.version },
                "contentChanges": [{ "text": text }]
            }
        }));
    }

    pub fn close(&mut self) {
        let Some(document) = self.document.take() else {
            return;
        };
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": document } }
        }));
    }

    pub fn hover(&mut self, position: Position) {
        self.request("textDocument/hover", position, Request::Hover(position));
    }

    pub fn definition(&mut self, position: Position) {
        self.request("textDocument/definition", position, Request::Definition);
    }

    /// Everything the server said since the last call.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(message) = self.events.try_recv() {
            match message {
                ToUi::Diagnostics { path, items } => {
                    events.push(Event::Diagnostics { path, items })
                }
                ToUi::Response { id, result } => match self.pending.remove(&id) {
                    Some(Request::Hover(position)) => events.push(Event::Hover {
                        position,
                        text: hover_text(&result),
                    }),
                    Some(Request::Definition) => events.push(Event::Definition {
                        location: location(&result),
                    }),
                    // `initialize`: nothing to do with the answer.
                    None => {}
                },
            }
        }
        events
    }

    fn request(&mut self, method: &str, position: Position, kind: Request) {
        let Some(document) = self.document.clone() else {
            return;
        };
        let id = self.take_id();
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": {
                "textDocument": { "uri": document },
                "position": { "line": position.line, "character": position.character }
            }
        }));
        self.pending.insert(id, kind);
    }

    fn take_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Frames go through a thread so a slow server can never freeze a frame:
    /// the pipe has a small buffer and the editor writes on every keystroke.
    fn send(&self, message: Value) {
        let _ = self.to_child.send(message);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Ask politely, then make sure the process is gone: it must not
        // outlive the window.
        let id = self.take_id();
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown" }));
        self.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write_frames(mut stdin: impl Write, frames: Receiver<Value>) {
    for frame in frames {
        let body = frame.to_string();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        if stdin.write_all(header.as_bytes()).is_err()
            || stdin.write_all(body.as_bytes()).is_err()
            || stdin.flush().is_err()
        {
            break;
        }
    }
}

fn read_frames(
    stdout: ChildStdout,
    to_child: Sender<Value>,
    to_ui: Sender<ToUi>,
    ctx: egui::Context,
) {
    let mut stdout = BufReader::new(stdout);
    while let Some(message) = read_frame(&mut stdout) {
        if deliver(&message, &to_child, &to_ui) {
            ctx.request_repaint();
        }
    }
}

/// One `Content-Length` frame. `None` at end of stream.
fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // the server exited
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Route one message from the server. Returns true when the editor has
/// something new to draw.
fn deliver(message: &Value, to_child: &Sender<Value>, to_ui: &Sender<ToUi>) -> bool {
    if let Some(method) = message.get("method") {
        if let Some(id) = message.get("id") {
            // A request *from* the server, e.g. `client/registerCapability`.
            // We advertise nothing that needs more than a null result.
            let _ = to_child.send(json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
            return false;
        }
        if method == "textDocument/publishDiagnostics" {
            if let Some((path, items)) = diagnose(&message["params"]) {
                return to_ui.send(ToUi::Diagnostics { path, items }).is_ok();
            }
        }
        return false;
    }
    let Some(id) = message.get("id").and_then(Value::as_i64) else {
        return false;
    };
    let result = message.get("result").cloned().unwrap_or(Value::Null);
    to_ui.send(ToUi::Response { id, result }).is_ok()
}

fn diagnose(params: &Value) -> Option<(PathBuf, Vec<Diagnostic>)> {
    let path = path_of(params.get("uri")?.as_str()?)?;
    let items = params
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|item| {
            let range = item.get("range")?;
            let mut message = item.get("message")?.as_str()?.trim().to_string();
            // The compiler puts `[-Wunused-variable]` in its message, clangd
            // in a separate field; show the student the same thing.
            if let Some(code) = item.get("code").and_then(Value::as_str) {
                message = format!("{message} [{code}]");
            }
            Some(Diagnostic {
                range: Range {
                    start: position_of(range.get("start")?)?,
                    end: position_of(range.get("end")?)?,
                },
                message,
                // 1 is Error; warnings, information and hints mark in yellow.
                warning: item.get("severity").and_then(Value::as_u64) != Some(1),
            })
        })
        .collect();
    Some((path, items))
}

/// `contents` of a hover answer, flattened to plain text.
fn hover_text(result: &Value) -> Option<String> {
    let text = match result.get("contents")? {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(marked_string)
            .collect::<Vec<_>>()
            .join("\n\n"),
        contents => marked_string(contents)?,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A `MarkedString`: either a string or `{ language, value }`.
fn marked_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        value => Some(value.get("value")?.as_str()?.to_string()),
    }
}

/// First location of a definition answer: `Location` when the server answers
/// with one object, otherwise `Location` or `LocationLink` items.
fn location(result: &Value) -> Option<Location> {
    let first = match result {
        Value::Array(items) => items.first()?,
        result => result,
    };
    let uri = first
        .get("uri")
        .or_else(|| first.get("targetUri"))?
        .as_str()?;
    let range = first.get("range").or_else(|| first.get("targetRange"))?;
    Some(Location {
        path: path_of(uri)?,
        position: position_of(range.get("start")?)?,
    })
}

fn position_of(value: &Value) -> Option<Position> {
    Some(Position {
        line: u32::try_from(value.get("line")?.as_u64()?).ok()?,
        character: u32::try_from(value.get("character")?.as_u64()?).ok()?,
    })
}

fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("c") => "c",
        _ => "cpp",
    }
}

/// `file://` URI of an absolute path, as the protocol wants it.
pub fn uri(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                uri.push(byte as char)
            }
            _ => {
                uri.push('%');
                uri.push(HEX[usize::from(byte >> 4)] as char);
                uri.push(HEX[usize::from(byte & 0xf)] as char);
            }
        }
    }
    uri
}

pub fn path_of(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?.as_bytes();
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut rest = encoded.iter().copied();
    while let Some(byte) = rest.next() {
        if byte == b'%' {
            let high = hex(rest.next()?)?;
            let low = hex(rest.next()?)?;
            bytes.push(high << 4 | low);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Character index of a protocol position, clamped to the end of its line.
pub fn char_index(text: &str, position: Position) -> Option<usize> {
    let mut index = 0;
    for (line, content) in text.split_inclusive('\n').enumerate() {
        if line as u32 != position.line {
            index += content.chars().count();
            continue;
        }
        let mut units = 0;
        for character in content.chars() {
            // Past the end of the line, clamp to the newline: a column never
            // points into the next line.
            if units >= position.character || character == '\n' {
                return Some(index);
            }
            units += character.len_utf16() as u32;
            index += 1;
        }
        return Some(index);
    }
    None
}

/// Protocol position of a character index: the inverse of [`char_index`].
pub fn position(text: &str, char_index: usize) -> Position {
    let mut position = Position {
        line: 0,
        character: 0,
    };
    for character in text.chars().take(char_index) {
        if character == '\n' {
            position.line += 1;
            position.character = 0;
        } else {
            position.character += character.len_utf16() as u32;
        }
    }
    position
}

#[cfg(test)]
mod tests {
    use super::{char_index, path_of, position, uri, Position};
    use std::path::Path;

    #[test]
    fn positions_round_trip() {
        let text = "one\ntwo\nthree";
        for index in 0..=text.chars().count() {
            let back = char_index(text, position(text, index));
            assert_eq!(back, Some(index), "at {index}");
        }
    }

    #[test]
    fn a_column_past_the_end_clamps_to_the_line() {
        let text = "ab\ncd\n";
        // Column 99 of line 1: end of "cd", index 5.
        assert_eq!(
            char_index(
                text,
                Position {
                    line: 1,
                    character: 99
                }
            ),
            Some(5)
        );
        assert_eq!(
            char_index(
                text,
                Position {
                    line: 9,
                    character: 0
                }
            ),
            None
        );
    }

    #[test]
    fn utf16_columns_count_code_units_not_characters() {
        // 'é' takes one UTF-16 unit (two bytes), '𝄞' two (a surrogate pair).
        let text = "é𝄞x";
        assert_eq!(
            position(text, 1),
            Position {
                line: 0,
                character: 1
            }
        );
        assert_eq!(
            position(text, 2),
            Position {
                line: 0,
                character: 3
            }
        );
        assert_eq!(
            char_index(
                text,
                Position {
                    line: 0,
                    character: 1
                }
            ),
            Some(1)
        );
        // A column inside a surrogate pair rounds to the end of that character.
        assert_eq!(
            char_index(
                text,
                Position {
                    line: 0,
                    character: 2
                }
            ),
            Some(2)
        );
    }

    #[test]
    fn uris_round_trip_and_escape_what_must_be_escaped() {
        let path = Path::new("/tmp/a b/c+d.cpp");
        let encoded = uri(path);
        assert_eq!(encoded, "file:///tmp/a%20b/c%2Bd.cpp");
        assert_eq!(path_of(&encoded).as_deref(), Some(path));
        assert!(path_of("http://example.com").is_none());
    }
}
