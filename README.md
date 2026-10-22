# vrs

A minimal editor for learning C++ from the terminal. Three panes: the files
under `~/.vrs`, a syntax-highlighted editor, and a real terminal.

## Features

- File panel rooted at `~/.vrs`: create files and folders, open, delete.
- Editor with C++ syntax highlighting, line numbers, undo/redo (egui).
- Compiler diagnostics from the system `g++`: errors highlighted in red,
  warnings in yellow behind the identifier, message on hover.
- Real terminal (PTY passthrough, `vt100` emulation): colours, `Tab`
  completion, `Ctrl+C`, mouse-wheel scrolling, bracketed paste.
- Right-click in the editor to format the selected lines with the system
  `clang-format` (`--lines`, so only the selection is rewritten).

## Build and run

```sh
cargo run --release
```

Needs `g++` on `PATH` for the diagnostics and `clang-format` for the formatter
(both are the tools a student already uses in the terminal). Linux first;
other systems later.

## Keys

| Key | In the editor | In the terminal |
| --- | --- | --- |
| `Ctrl+S` | save | sent to the shell |
| `Ctrl+Q` | quit (asks to save) | sent to the shell |
| `Ctrl+C` | copy (if a selection) | interrupt the running command |

## Design notes

See `afaire.md` (French) for the requirements and the decisions taken.
