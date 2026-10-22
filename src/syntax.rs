//! Syntax definitions for highlighting. `egui_code_editor` ships presets for
//! some languages but not C++, which is the main language taught here.

use egui_code_editor::Syntax;

// `rustfmt` would spread these lists one word per line; keeping them in a
// block makes them easy to read and to extend.
#[rustfmt::skip]
const KEYWORDS: [&str; 62] = [
    "alignas", "alignof", "and", "asm", "auto", "break", "case", "catch", "class",
    "co_await", "co_return", "co_yield", "concept", "const", "const_cast", "constexpr",
    "continue", "decltype", "default", "delete", "do", "dynamic_cast", "else", "enum",
    "explicit", "export", "extern", "for", "friend", "goto", "if", "inline", "mutable",
    "namespace", "new", "noexcept", "operator", "private", "protected", "public",
    "register", "reinterpret_cast", "requires", "return", "sizeof", "static",
    "static_assert", "static_cast", "struct", "switch", "template", "this", "throw",
    "try", "typedef", "typeid", "typename", "union", "using", "virtual", "volatile",
    "while",
];

#[rustfmt::skip]
const TYPES: [&str; 22] = [
    "bool", "char", "char8_t", "char16_t", "char32_t", "double", "float", "int", "long",
    "short", "signed", "unsigned", "void", "wchar_t", "size_t", "string", "vector",
    "cout", "cin", "cerr", "endl", "std",
];

pub fn cpp() -> Syntax {
    Syntax::new("C++")
        .with_case_sensitive(true)
        .with_comment("//")
        .with_comment_multiline(["/*", "*/"])
        .with_quotes(['\'', '"'])
        .with_hyperlinks(["http:", "https:", "www.", "file:"])
        .with_keywords(KEYWORDS)
        .with_types(TYPES)
        .with_special(["true", "false", "nullptr"])
}

pub fn for_path(path: &std::path::Path) -> Syntax {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hh" | "hxx" | "ino") => cpp(),
        Some("py") => Syntax::python(),
        Some("rs") => Syntax::rust(),
        Some("sh" | "bash") => Syntax::shell(),
        Some("lua") => Syntax::lua(),
        Some("sql") => Syntax::sql(),
        Some("asm" | "s") => Syntax::asm(),
        _ => Syntax::default(),
    }
}
