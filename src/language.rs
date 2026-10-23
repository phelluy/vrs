//! C++ intelligence for the open file: diagnostics, hover and definition.
//!
//! The work is done by `clangd` (see [`crate::clangd`]), one process per
//! project so it indexes a project once instead of re-parsing its headers on
//! every keystroke. This module owns that process and keeps the editor's view
//! of it: the diagnostics of the current buffer, the last hover answer, and
//! the pending jump. Nothing here draws.

use std::fs;
use std::path::{Path, PathBuf};

use egui::Context;

use crate::clangd::{self, Event, Position, Server};

/// Flags given to clangd for every project. Written once, at the root of
/// `~/.vrs`: clangd reads the `.clangd` files of a file's parents, so one file
/// covers them all, and a project's own `.clangd` adds to it. Without this,
/// clangd's fallback flags report errors but none of the warnings the compiler
/// gives in the terminal.
const CONFIG: &str = "\
# Written by VRS: flags for clangd, the language server behind the editor's
# error highlighting. They match the warnings given by the compiler, so the
# editor and the build agree.
CompileFlags:
  Add: [-Wall, -Wextra]
";

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    /// 1-based line.
    pub line: usize,
    /// 1-based column, in characters.
    pub column: usize,
    /// End of the underlined range, same units; handier than guessing where
    /// the offending word stops.
    pub end_line: usize,
    pub end_column: usize,
    pub message: String,
    pub warning: bool,
}

/// A place another file asks the editor to jump to.
pub struct Jump {
    pub path: PathBuf,
    /// Character index in the target file.
    pub index: usize,
}

pub struct Language {
    root: PathBuf,
    ctx: Context,
    server: Option<Server>,
    /// Why the last start failed, if it did ("clangd: No such file…").
    error: Option<String>,
    /// Project whose start failed, so it is not tried again on every frame.
    failed: Option<PathBuf>,
    /// Path of the document the server has open, and the text it was given.
    document: Option<PathBuf>,
    sent: String,
    diagnostics: Vec<Diagnostic>,
    /// Last hover answer, with the position it belongs to. `None` inside means
    /// "asked, no documentation": it stops the question being asked again.
    hover: Option<(Position, Option<String>)>,
    hover_pending: Option<Position>,
    jump: Option<Jump>,
}

impl Language {
    pub fn new(root: PathBuf, ctx: Context) -> Self {
        write_config(&root);
        Self {
            root,
            ctx,
            server: None,
            error: None,
            failed: None,
            document: None,
            sent: String::new(),
            diagnostics: Vec::new(),
            hover: None,
            hover_pending: None,
            jump: None,
        }
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Why the language server is not running, when it could not start.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Called every frame while a file is open. Keeps the server in step with
    /// the buffer: another file, another project or another text.
    pub fn update(&mut self, path: &Path, text: &str) {
        if !supported(path) {
            self.close();
            return;
        }
        if self.document.as_deref() != Some(path) {
            self.open(path, text);
        } else if self.sent != text {
            if let Some(server) = &mut self.server {
                server.change(text);
            }
            self.sent.clear();
            self.sent.push_str(text);
        }
        self.poll();
    }

    /// Forget the open document: another kind of file, or no file at all.
    pub fn close(&mut self) {
        if let Some(server) = &mut self.server {
            server.close();
        }
        self.document = None;
        self.sent.clear();
        self.diagnostics.clear();
        self.hover = None;
        self.hover_pending = None;
    }

    /// Documentation of the symbol at `position`, if the server already
    /// answered about that exact spot.
    pub fn hover_text(&self, position: Position) -> Option<&str> {
        let (answered, text) = self.hover.as_ref()?;
        (*answered == position).then_some(text.as_deref()).flatten()
    }

    /// Ask once per position: a hover re-asked every frame would put the
    /// server in a loop.
    pub fn request_hover(&mut self, position: Position) {
        let answered = self.hover.as_ref().map(|(position, _)| *position) == Some(position);
        if answered || self.hover_pending == Some(position) {
            return;
        }
        if let Some(server) = &mut self.server {
            server.hover(position);
            self.hover_pending = Some(position);
        }
    }

    pub fn request_definition(&mut self, position: Position) {
        if let Some(server) = &mut self.server {
            server.definition(position);
        }
    }

    /// The jump the last definition answer asked for, once.
    pub fn take_jump(&mut self) -> Option<Jump> {
        self.jump.take()
    }

    /// Read the target file to turn the protocol position into a character
    /// index. Only files under `~/.vrs` are followed: landing in a system
    /// header would fill the editor with thousands of lines nobody asked for,
    /// and start another server on another project.
    fn jump_to(&self, location: clangd::Location) -> Option<Jump> {
        let root = fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        let path = fs::canonicalize(&location.path).ok()?;
        if !path.starts_with(&root) {
            return None;
        }
        let text = fs::read_to_string(&path).ok()?;
        let index = clangd::char_index(&text, location.position)?;
        Some(Jump { path, index })
    }

    fn open(&mut self, path: &Path, text: &str) {
        let project = project_of(&self.root, path);
        let started = self.server.as_ref().map(Server::project) == Some(project.as_path());
        // A start that failed (no clangd installed, say) is not retried on
        // every frame: `document` stays empty and the update would come back.
        let failed_here = self.failed.as_deref() == Some(project.as_path());
        if !started && !failed_here {
            self.server = None; // drops, and stops, the previous one
            match Server::spawn(&project, self.ctx.clone()) {
                Ok(server) => {
                    self.server = Some(server);
                    self.failed = None;
                    self.error = None;
                }
                Err(err) => {
                    self.failed = Some(project);
                    self.error = Some(format!("clangd: {err}"));
                }
            }
        }
        self.diagnostics.clear();
        self.hover = None;
        self.hover_pending = None;
        self.sent.clear();
        self.sent.push_str(text);
        if let Some(server) = &mut self.server {
            server.open(path, text);
            self.document = Some(path.to_path_buf());
        }
    }

    fn poll(&mut self) {
        let Some(server) = &mut self.server else {
            return;
        };
        let events = server.poll();
        if !server.is_running() {
            // The server died. Keep the editor usable and silent rather than
            // respawning it on every frame: another file starts a new one.
            self.server = None;
            self.hover_pending = None;
            return;
        }
        for event in events {
            match event {
                Event::Diagnostics { path, items } => {
                    let ours = self
                        .document
                        .as_deref()
                        .is_some_and(|ours| same_file(ours, &path));
                    if !ours {
                        // Headers, and files the server still knows about.
                        continue;
                    }
                    let diagnostics = items.iter().filter_map(|item| convert(&self.sent, item));
                    self.diagnostics = diagnostics.collect();
                }
                Event::Hover { position, text } => {
                    self.hover_pending = None;
                    self.hover = Some((position, text));
                }
                Event::Definition { location } => {
                    if let Some(location) = location {
                        self.jump = self.jump_to(location);
                    }
                }
            }
        }
    }
}

pub fn supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hh" | "hxx" | "ino")
    )
}

/// Project a file belongs to: the folder just below `~/.vrs`, since that is
/// what clangd uses as workspace root. Files outside (a system header opened
/// from a definition) fall back to their own directory.
fn project_of(root: &Path, path: &Path) -> PathBuf {
    let project = path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|first| root.join(first))
        .filter(|candidate| candidate.is_dir());
    project
        .or_else(|| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| root.to_path_buf())
}

pub fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    // clangd resolves symbolic links (`/tmp` is `/private/tmp` on macOS).
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Turn a protocol range into 1-based lines and columns, in characters.
fn convert(text: &str, item: &clangd::Diagnostic) -> Option<Diagnostic> {
    let start = clangd::char_index(text, item.range.start)?;
    let end = clangd::char_index(text, item.range.end).unwrap_or(start);
    let (line, column) = line_column(text, start);
    let (end_line, end_column) = line_column(text, end);
    Some(Diagnostic {
        line,
        column,
        end_line,
        end_column,
        message: item.message.clone(),
        warning: item.warning,
    })
}

/// 1-based line and character column of a character index.
fn line_column(text: &str, char_index: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for character in text.chars().take(char_index) {
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn write_config(root: &Path) {
    let path = root.join(".clangd");
    if !path.exists() {
        let _ = fs::write(&path, CONFIG);
    }
}

#[cfg(test)]
mod tests {
    use super::{convert, line_column, project_of, same_file, Language};
    use crate::clangd::{self, Diagnostic as RawDiagnostic, Position, Range};
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    fn a_range_becomes_one_based_lines_and_columns() {
        let text = "int main() {\n    return x;\n}\n";
        let item = RawDiagnostic {
            range: Range {
                start: Position {
                    line: 1,
                    character: 11,
                },
                end: Position {
                    line: 1,
                    character: 12,
                },
            },
            message: "Use of undeclared identifier 'x'".into(),
            warning: false,
        };
        let diagnostic = convert(text, &item).expect("converted");
        assert_eq!(diagnostic.line, 2);
        assert_eq!(diagnostic.column, 12);
        assert_eq!(diagnostic.end_line, 2);
        assert_eq!(diagnostic.end_column, 13);
    }

    #[test]
    fn counts_columns_in_characters() {
        assert_eq!(line_column("abc", 0), (1, 1));
        assert_eq!(line_column("abc\ndef", 5), (2, 2));
    }

    #[test]
    fn a_project_is_the_folder_under_the_root() {
        let root = Path::new("/home/student/.vrs");
        assert_eq!(
            project_of(root, Path::new("/home/student/.vrs/demo")),
            Path::new("/home/student/.vrs")
        );
        assert_eq!(
            project_of(root, Path::new("/usr/include/c++/12/vector")),
            Path::new("/usr/include/c++/12")
        );
    }

    /// End to end against the real clangd: diagnostics, hover and definition
    /// on the buffer, without the file being written again. Skips silently
    /// when clangd is not installed.
    #[test]
    fn clangd_answers_diagnostics_hover_and_definition() {
        let root = std::env::temp_dir().join(format!("vrs-language-{}", std::process::id()));
        let project = root.join("demo");
        std::fs::create_dir_all(&project).expect("temp dir");
        let path = project.join("probe.cpp");
        let text = "\
#include <cstdio>

int add(int a, int b) {
    return a + b;
}

int main() {
    std::printf(\"%d\\n\", add(1, 2));
    return oops;
}
";
        std::fs::write(&path, text).expect("write");

        let mut language = Language::new(root.clone(), egui::Context::default());
        language.update(&path, text);
        if language.server.is_none() {
            return; // no clangd on this machine
        }

        // clangd builds a preamble first: give it a moment.
        wait(&mut language, &path, text, |language| {
            !language.diagnostics().is_empty()
        });
        let diagnostics = language.diagnostics();
        assert!(!diagnostics.is_empty(), "no diagnostic arrived");
        let undeclared = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("oops"))
            .expect("the undeclared identifier");
        assert_eq!(undeclared.line, 9);
        assert!(!undeclared.warning);
        // "oops" starts at column 12 and the range covers its four letters.
        assert_eq!((undeclared.column, undeclared.end_column), (12, 16));

        let call = text.rfind("add(1, 2)").expect("the call") + 1;
        let position = clangd::position(text, call);
        language.request_hover(position);
        wait(&mut language, &path, text, |language| {
            language.hover_text(position).is_some()
        });
        let hover = language.hover_text(position).expect("a hover answer");
        assert!(hover.contains("add"), "unexpected hover: {hover}");

        language.request_definition(position);
        wait(&mut language, &path, text, |language| {
            language.jump.is_some()
        });
        let jump = language.take_jump().expect("a definition");
        assert!(
            same_file(&jump.path, &path),
            "{:?} is not {:?}",
            jump.path,
            path
        );
        // The definition of `add` is the one on line 3, at its name.
        let body = std::fs::read_to_string(&jump.path).expect("read");
        assert_eq!(clangd::position(&body, jump.index).line, 2);

        let _ = std::fs::remove_dir_all(root);
    }

    /// Feed `update` until `done`, or five seconds.
    fn wait(language: &mut Language, path: &Path, text: &str, done: impl Fn(&Language) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            language.update(path, text);
            if done(language) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
