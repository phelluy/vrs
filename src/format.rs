//! Format a range of lines with the system `clang-format`.
//!
//! Nothing is linked into the binary: the editor shells out to the same
//! `clang-format` a student would use in the terminal. `--lines` restricts
//! the work to the selected lines, so the rest of the buffer is untouched.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Format lines `first..=last` (1-based, inclusive) of `text`, using the style
/// that `clang-format` would pick for `path` (`.clang-format` in a parent
/// directory, or the LLVM fallback).
///
/// Returns the whole buffer with the formatted region spliced in.
pub fn format_range(path: &Path, text: &str, first: usize, last: usize) -> Result<String, String> {
    let mut child = Command::new("clang-format")
        .arg("--style=file")
        .arg(format!("--assume-filename={}", path.display()))
        .arg(format!("--lines={first}:{last}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("clang-format: {err}"))?;

    // Write from another thread: clang-format reads all of stdin before
    // writing anything, but the pipe buffer is small and a large file would
    // block us before we ever start reading.
    if let Some(mut stdin) = child.stdin.take() {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }

    let output = child
        .wait_with_output()
        .map_err(|err| format!("clang-format: {err}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "clang-format failed".to_string()
        } else {
            message
        });
    }
    let formatted = String::from_utf8_lossy(&output.stdout);
    Ok(splice(text, &formatted))
}

/// Keep the lines `clang-format` left alone, take the ones it rewrote.
///
/// The rewritten block is found by comparing both texts from the outside in:
/// `--lines` only touches the requested lines, so the middle is exactly the
/// formatted region, whatever its new length.
fn splice(original: &str, formatted: &str) -> String {
    let before: Vec<&str> = original.split_inclusive('\n').collect();
    let after: Vec<&str> = formatted.split_inclusive('\n').collect();
    let prefix = before
        .iter()
        .zip(&after)
        .take_while(|(ours, theirs)| ours == theirs)
        .count();
    // Never let the suffix eat into the prefix, even for identical texts.
    let small = before.len().min(after.len());
    let suffix = before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take_while(|(ours, theirs)| ours == theirs)
        .count()
        .min(small.saturating_sub(prefix));

    let mut text = String::with_capacity(original.len());
    for line in before.iter().take(prefix) {
        text.push_str(line);
    }
    let rewritten = after.len().saturating_sub(prefix + suffix);
    for line in after.iter().skip(prefix).take(rewritten) {
        text.push_str(line);
    }
    for line in before.iter().skip(before.len().saturating_sub(suffix)) {
        text.push_str(line);
    }
    text
}

/// 1-based line holding the character at `char_index`.
pub fn line_of(text: &str, char_index: usize) -> usize {
    1 + text.chars().take(char_index).filter(|c| *c == '\n').count()
}

#[cfg(test)]
mod tests {
    use super::{line_of, splice};

    #[test]
    fn splice_takes_the_formatted_lines_and_keeps_the_rest() {
        let original = "int  main(){\nint x;\nreturn 0;\n}\n";
        let formatted = "int  main(){\n    int x;\n    return 0;\n}\n";
        assert_eq!(
            splice(original, formatted),
            "int  main(){\n    int x;\n    return 0;\n}\n"
        );
    }

    #[test]
    fn splice_handles_the_region_changing_length() {
        let original = "a\nx(\n1,\n2);\nb\n";
        let formatted = "a\nx(1, 2);\nb\n";
        assert_eq!(splice(original, formatted), "a\nx(1, 2);\nb\n");
    }

    #[test]
    fn splice_is_identity_when_nothing_changed() {
        let text = "a\nb\n";
        assert_eq!(splice(text, text), text);
    }

    #[test]
    fn counts_lines_from_a_character_index() {
        let text = "one\ntwo\nthree";
        assert_eq!(line_of(text, 0), 1);
        assert_eq!(line_of(text, 4), 2);
        assert_eq!(line_of(text, 8), 3);
    }
}
