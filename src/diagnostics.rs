//! Compiler diagnostics for the open file.
//!
//! The linter is the system C++ compiler in syntax-only mode: no extra tool to
//! install, no compile database to maintain, and it reports exactly the errors
//! the student will meet when building. Its output is parsed into the
//! (line, column, message) triples that the editor underlines.
//!
//! The buffer is linted through a hidden copy written next to the original
//! file, so unsaved text is checked and relative `#include`s still resolve. The
//! copy is removed as soon as the compiler exits.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

/// Delay between the last keystroke and the lint run.
const DEBOUNCE: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    /// 1-based line.
    pub line: usize,
    /// 1-based column.
    pub column: usize,
    pub message: String,
    pub warning: bool,
}

pub struct Linter {
    rx: Option<Receiver<(String, Vec<Diagnostic>)>>,
    /// Text the current diagnostics belong to.
    linted: String,
    diagnostics: Vec<Diagnostic>,
    /// When the buffer started differing from `linted`.
    pending_since: Option<f64>,
}

impl Linter {
    pub fn new() -> Self {
        Self {
            rx: None,
            linted: String::new(),
            diagnostics: Vec::new(),
            pending_since: None,
        }
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Forget everything, e.g. when another file is opened.
    pub fn reset(&mut self) {
        self.rx = None;
        self.linted.clear();
        self.diagnostics.clear();
        self.pending_since = None;
    }

    /// Called every frame while a file is open. Runs the compiler once the
    /// buffer has been quiet for [`DEBOUNCE`], never more than one run at a
    /// time.
    pub fn update(&mut self, ctx: &egui::Context, path: &Path, text: &str) {
        self.poll();
        if !supported(path) {
            self.diagnostics.clear();
            self.linted.clear();
            return;
        }
        if self.rx.is_some() || self.linted == text {
            return;
        }
        let now = ctx.input(|i| i.time);
        let since = *self.pending_since.get_or_insert(now);
        if now - since < DEBOUNCE {
            // egui is reactive: without this the delay never elapses.
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        self.pending_since = None;
        self.rx = Some(spawn(path, text));
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        match rx.try_recv() {
            Ok((text, diagnostics)) => {
                self.linted = text;
                self.diagnostics = diagnostics;
                self.rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.rx = None,
        }
    }
}

fn supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hh" | "hxx" | "ino")
    )
}

/// Hidden copy of the buffer, in the same directory so `#include "…"` works.
/// The tree skips dotfiles, so it never shows up in the panel.
fn temp_path(path: &Path) -> PathBuf {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("cpp");
    path.with_file_name(format!(".vrs-lint-{}.{ext}", std::process::id()))
}

fn spawn(path: &Path, text: &str) -> Receiver<(String, Vec<Diagnostic>)> {
    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    let text = text.to_string();
    std::thread::spawn(move || {
        let temp = temp_path(&path);
        let mut diagnostics = Vec::new();
        if std::fs::write(&temp, &text).is_ok() {
            let output = std::process::Command::new("g++")
                .args([
                    "-fsyntax-only",
                    "-fdiagnostics-plain-output",
                    "-fno-diagnostics-color",
                    "-Wall",
                    "-Wextra",
                ])
                .arg(&temp)
                .env("LC_ALL", "C")
                .output();
            let _ = std::fs::remove_file(&temp);
            if let Ok(output) = output {
                diagnostics = parse(&String::from_utf8_lossy(&output.stderr), &temp);
            }
        }
        let _ = tx.send((text, diagnostics));
    });
    rx
}

/// Parse `file:line:col: error: message` lines. Notes and context lines
/// (`file: In function …`) are skipped.
fn parse(stderr: &str, temp: &Path) -> Vec<Diagnostic> {
    let temp = temp.to_string_lossy();
    let mut diagnostics = Vec::new();
    for line in stderr.lines() {
        let mut parts = line.splitn(4, ':');
        let (Some(file), Some(line_no), Some(column), Some(rest)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if file != temp {
            continue;
        }
        let (Ok(line), Ok(column)) = (line_no.parse(), column.parse()) else {
            continue;
        };
        let rest = rest.trim_start();
        let (warning, message) = if let Some(message) = rest.strip_prefix("warning:") {
            (true, message)
        } else if let Some(message) = rest
            .strip_prefix("error:")
            .or_else(|| rest.strip_prefix("fatal error:"))
        {
            (false, message)
        } else {
            continue;
        };
        diagnostics.push(Diagnostic {
            line,
            column,
            message: message.trim().to_string(),
            warning,
        });
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, parse};
    use std::path::Path;

    #[test]
    fn parses_errors_and_warnings() {
        let temp = Path::new("/tmp/.vrs-lint-1.cpp");
        let stderr = "\
/tmp/.vrs-lint-1.cpp: In function 'int main()':
/tmp/.vrs-lint-1.cpp:3:5: error: 'printf' was not declared in this scope
/tmp/.vrs-lint-1.cpp:3:5: note: 'printf' is defined in header '<cstdio>'
/tmp/.vrs-lint-1.cpp:2:9: warning: unused variable 'x' [-Wunused-variable]
other.cpp:1:1: error: not our file
";
        let found = parse(stderr, temp);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            Diagnostic {
                line: 3,
                column: 5,
                message: "'printf' was not declared in this scope".into(),
                warning: false,
            }
        );
        assert!(found[1].warning);
        assert_eq!(found[1].line, 2);
    }

    #[test]
    fn ignores_a_missing_compiler_output() {
        assert!(parse("", Path::new("/tmp/.vrs-lint-1.cpp")).is_empty());
    }
}
