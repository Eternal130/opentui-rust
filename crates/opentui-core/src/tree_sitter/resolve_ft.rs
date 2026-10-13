//! Filetype resolution — port of
//! `packages/core/src/lib/tree-sitter/resolve-ft.ts`.
//!
//! `info_string_to_filetype` (resolve-ft.ts:173-189) is the tier-1 path
//! (markdown fenced-block language → parser filetype); the extension and
//! basename maps are ported verbatim so `pathToFiletype` /
//! `extensionToFiletype` behave identically for consumers.

use std::collections::HashMap;
use std::sync::OnceLock;

/// `extensionToFiletype` (resolve-ft.ts:3-100).
pub fn extension_to_filetype_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let pairs: &[(&str, &str)] = &[
            ("astro", "astro"),
            ("bash", "bash"),
            ("c", "c"),
            ("cc", "cpp"),
            ("cjs", "javascript"),
            ("clj", "clojure"),
            ("cljs", "clojure"),
            ("cljc", "clojure"),
            ("cpp", "cpp"),
            ("cxx", "cpp"),
            ("cs", "csharp"),
            ("cts", "typescript"),
            ("ctsx", "typescriptreact"),
            ("dart", "dart"),
            ("diff", "diff"),
            ("edn", "clojure"),
            ("go", "go"),
            ("gemspec", "ruby"),
            ("groovy", "groovy"),
            ("h", "c"),
            ("handlebars", "handlebars"),
            ("hbs", "handlebars"),
            ("hpp", "cpp"),
            ("hxx", "cpp"),
            ("h++", "cpp"),
            ("hh", "cpp"),
            ("hrl", "erlang"),
            ("hs", "haskell"),
            ("htm", "html"),
            ("html", "html"),
            ("ini", "ini"),
            ("js", "javascript"),
            ("jsx", "javascriptreact"),
            ("jl", "julia"),
            ("json", "json"),
            ("ksh", "bash"),
            ("kt", "kotlin"),
            ("kts", "kotlin"),
            ("latex", "latex"),
            ("less", "less"),
            ("lua", "lua"),
            ("markdown", "markdown"),
            ("md", "markdown"),
            ("mdown", "markdown"),
            ("mkd", "markdown"),
            ("mjs", "javascript"),
            ("ml", "ocaml"),
            ("mli", "ocaml"),
            ("mts", "typescript"),
            ("mtsx", "typescriptreact"),
            ("patch", "diff"),
            ("php", "php"),
            ("pl", "perl"),
            ("pm", "perl"),
            ("ps1", "powershell"),
            ("psm1", "powershell"),
            ("py", "python"),
            ("pyi", "python"),
            ("r", "r"),
            ("rb", "ruby"),
            ("rake", "ruby"),
            ("rs", "rust"),
            ("ru", "ruby"),
            ("sass", "sass"),
            ("sc", "scala"),
            ("scala", "scala"),
            ("scss", "scss"),
            ("sh", "bash"),
            ("sql", "sql"),
            ("svelte", "svelte"),
            ("swift", "swift"),
            ("ts", "typescript"),
            ("tsx", "typescriptreact"),
            ("tex", "latex"),
            ("toml", "toml"),
            ("vue", "vue"),
            ("vim", "vim"),
            ("xml", "xml"),
            ("xsl", "xsl"),
            ("yaml", "yaml"),
            ("yml", "yaml"),
            ("zig", "zig"),
            ("zon", "zig"),
            ("zsh", "bash"),
            ("c++", "cpp"),
            ("erl", "erlang"),
            ("exs", "elixir"),
            ("ex", "elixir"),
            ("elm", "elm"),
            ("fsharp", "fsharp"),
            ("fs", "fsharp"),
            ("fsx", "fsharp"),
            ("fsscript", "fsharp"),
            ("fsi", "fsharp"),
            ("java", "java"),
            ("css", "css"),
        ];
        pairs.iter().copied().collect()
    })
}

/// `basenameToFiletype` (resolve-ft.ts:102-131).
pub fn basename_to_filetype_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let pairs: &[(&str, &str)] = &[
            (".bash_aliases", "bash"),
            (".bash_logout", "bash"),
            (".bash_profile", "bash"),
            (".bashrc", "bash"),
            (".kshrc", "bash"),
            (".profile", "bash"),
            (".vimrc", "vim"),
            (".zlogin", "bash"),
            (".zlogout", "bash"),
            (".zprofile", "bash"),
            (".zshenv", "bash"),
            (".zshrc", "bash"),
            ("appfile", "ruby"),
            ("berksfile", "ruby"),
            ("brewfile", "ruby"),
            ("cheffile", "ruby"),
            ("containerfile", "dockerfile"),
            ("dockerfile", "dockerfile"),
            ("fastfile", "ruby"),
            ("gemfile", "ruby"),
            ("gnumakefile", "make"),
            ("gvimrc", "vim"),
            ("guardfile", "ruby"),
            ("makefile", "make"),
            ("podfile", "ruby"),
            ("rakefile", "ruby"),
            ("thorfile", "ruby"),
            ("vagrantfile", "ruby"),
        ];
        pairs.iter().copied().collect()
    })
}

/// `normalizeFiletypeToken` (resolve-ft.ts:133-136): trim, strip a leading
/// dot, lowercase; empty → None.
fn normalize_filetype_token(value: &str) -> Option<String> {
    let normalized = value.trim().trim_start_matches('.').to_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// `getBasename` (resolve-ft.ts:138-144): trim, normalize backslashes to
/// forward slashes, lowercase POSIX basename.
fn get_basename(value: &str) -> Option<String> {
    let normalized = value.trim().replace('\\', "/");
    if normalized.is_empty() {
        return None;
    }
    let basename = normalized
        .rsplit('/')
        .next()
        .unwrap_or(&normalized)
        .to_lowercase();
    if basename.is_empty() {
        None
    } else {
        Some(basename)
    }
}

/// `extToFiletype` (resolve-ft.ts:146-151).
#[must_use]
pub fn ext_to_filetype(extension: &str) -> Option<&'static str> {
    let normalized = normalize_filetype_token(extension)?;
    extension_to_filetype_map()
        .get(normalized.as_str())
        .copied()
}

/// `pathToFiletype` (resolve-ft.ts:153-171).
#[must_use]
pub fn path_to_filetype(path: &str) -> Option<&'static str> {
    let basename = get_basename(path)?;
    if let Some(ft) = basename_to_filetype_map().get(basename.as_str()) {
        return Some(ft);
    }
    let last_dot = basename.rfind('.')?;
    if last_dot == basename.len() - 1 {
        return None;
    }
    ext_to_filetype(&basename[last_dot + 1..])
}

/// `infoStringToFiletype` (resolve-ft.ts:173-189) — the markdown fence
/// info-string → filetype resolution: first whitespace-delimited token,
/// basename map, then path, then extension, else the normalized token
/// itself (unknown languages resolve to their own name and simply have
/// no parser downstream).
#[must_use]
pub fn info_string_to_filetype(info_string: &str) -> Option<String> {
    let token = info_string.split_whitespace().next()?;
    let lower = token.to_lowercase();
    if let Some(ft) = basename_to_filetype_map().get(lower.as_str()) {
        return Some((*ft).to_owned());
    }
    let normalized = normalize_filetype_token(token)?;
    Some(
        basename_to_filetype_map()
            .get(normalized.as_str())
            .copied()
            .map(String::from)
            .or_else(|| path_to_filetype(&normalized).map(String::from))
            .or_else(|| ext_to_filetype(&normalized).map(String::from))
            .unwrap_or(normalized),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_string_resolution() {
        // resolve-ft.ts:176-188 — first token only, unknown → identity
        assert_eq!(info_string_to_filetype("ts"), Some("typescript".into()));
        assert_eq!(info_string_to_filetype("js"), Some("javascript".into()));
        assert_eq!(info_string_to_filetype("zig"), Some("zig".into()));
        assert_eq!(info_string_to_filetype("rust"), Some("rust".into()));
        assert_eq!(
            info_string_to_filetype("  ts extra "),
            Some("typescript".into())
        );
        assert_eq!(info_string_to_filetype(""), None);
        assert_eq!(info_string_to_filetype("Makefile"), Some("make".into()));
    }

    #[test]
    fn path_and_ext_resolution() {
        assert_eq!(path_to_filetype("src/main.rs"), Some("rust"));
        assert_eq!(path_to_filetype(".zshrc"), Some("bash"));
        assert_eq!(path_to_filetype("DockerFile"), Some("dockerfile"));
        assert_eq!(ext_to_filetype(".TS"), Some("typescript"));
        assert_eq!(ext_to_filetype(""), None);
    }
}
