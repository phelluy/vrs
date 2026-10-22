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

## Install

Rust is assumed to be installed: `cargo --version` must answer, and the
compiler must be at least 1.95 (what `egui` 0.36 asks for). There is no build
script and no C library to compile, so no `cmake` or `pkg-config` needed.

### System packages

On Debian/Ubuntu:

```sh
sudo apt install g++ clang-format \
    libx11-6 libxcursor1 libxi6 libxkbcommon0 libxkbcommon-x11-0 \
    libwayland-client0 libwayland-egl1 libvulkan1 mesa-vulkan-drivers
```

| Package | Used by | For what |
| --- | --- | --- |
| `g++` | diagnostics | `-fsyntax-only` on the file being edited |
| `clang-format` | Format selection | rewriting the selected lines |
| `libx11-6`, `libxcursor1`, `libxi6` | window backend | window, mouse pointer, resize |
| `libxkbcommon0`, `libxkbcommon-x11-0` | window backend | keyboard layout |
| `libwayland-client0`, `libwayland-egl1` | window backend | same, under Wayland |
| `libvulkan1`, `mesa-vulkan-drivers` | renderer | drawing the window |

The X11, Wayland, Vulkan and EGL libraries are loaded when the program starts,
not linked: nothing is needed at build time, and a missing one only shows up
as a startup error. On a normal desktop session they are already there; the
last line covers software rendering on a machine without a GPU driver.

### Build and install

```sh
git clone git@github.com:phelluy/vrs.git
cd vrs
cargo install --path .
```

That builds in release mode and puts the `vrs` binary in `~/.cargo/bin`, which
is on `PATH` when Rust is installed through `rustup`. To try it without
installing anything:

```sh
cargo run --release
```

### First run

The program creates `~/.vrs` if it does not exist and uses it as the file
panel: that is where student files go, outside the repository. The terminal
pane runs `$SHELL`, falling back to `bash`. Nothing else to configure.

## Keys

| Key | In the editor | In the terminal |
| --- | --- | --- |
| `Ctrl+S` | save | sent to the shell |
| `Ctrl+Q` | quit (asks to save) | sent to the shell |
| `Ctrl+C` | copy (if a selection) | interrupt the running command |

## Design notes

See `afaire.md` (French) for the requirements and the decisions taken.
