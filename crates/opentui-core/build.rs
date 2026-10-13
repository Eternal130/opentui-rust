//! Build script — compiles the vendored tree-sitter grammar C sources
//! (`third_party/tree-sitter-grammars/`, pinned at the SAME tags as the
//! `.wasm` files `@opentui/core@0.5.14` ships in `assets/`, see
//! `packages/core/src/lib/tree-sitter/parsers-config.ts`):
//!
//! | language         | vendored tag | upstream wasm pin (parsers-config.ts) |
//! |------------------|--------------|---------------------------------------|
//! | javascript       | v0.25.0      | v0.25.0 (line 11)                     |
//! | typescript       | v0.23.2      | v0.23.2 (line 21)                     |
//! | markdown         | v0.5.1       | v0.5.1 (line 31)                      |
//! | markdown_inline  | v0.5.1       | v0.5.1 (line 61)                      |
//! | zig              | v1.1.2       | v1.1.2 (line 73)                      |
//!
//! The C `parser.c` IS the grammar — byte-identical source to what
//! upstream compiled to `.wasm`, so parse trees (and therefore highlight
//! captures) match the oracle's exactly. Sources fetched from the PyPI
//! sdists of the same tags (GitHub releases mirror).

use std::path::PathBuf;

fn grammar_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/tree-sitter-grammars")
        .join(name)
        .join("src")
}

fn main() {
    let grammars: &[(&str, &[&str])] = &[
        ("javascript", &["parser.c", "scanner.c"]),
        ("typescript", &["parser.c", "scanner.c"]),
        ("markdown", &["parser.c", "scanner.c"]),
        ("markdown_inline", &["parser.c", "scanner.c"]),
        ("zig", &["parser.c"]),
    ];

    // Each grammar ships its own era's tree_sitter/parser.h (0.23
    // grammars use TSFieldMapSlice, 0.25 grammars TSMapSlice — mixed ABI
    // generations exactly like upstream's shipped .wasm set).

    for (name, sources) in grammars {
        let dir = grammar_dir(name);
        let mut build = cc::Build::new();
        build
            .include(&dir)
            .flag_if_supported("-Wno-unused-parameter")
            .flag_if_supported("-Wno-unused-but-set-variable")
            .flag_if_supported("-Wno-trigraphs");
        for src in *sources {
            let path = dir.join(src);
            println!("cargo:rerun-if-changed={}", path.display());
            build.file(path);
        }
        build.compile(&format!("tree-sitter-{name}"));
    }
}
