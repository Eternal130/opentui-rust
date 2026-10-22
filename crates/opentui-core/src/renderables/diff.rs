//! `DiffRenderable` — port of `packages/core/src/renderables/Diff.ts`
//! (0.5.14), plus the `parsePatch` it runs on (the `diff` npm package
//! 9.0.0 — the same pin `@opentui/core@0.5.14` depends on; source read
//! from `tools/diff/sandbox/node_modules/diff/libcjs/patch/parse.js`).
//!
//! Upstream composes live renderables:
//! `DiffRenderable` (column/row root) → per side a
//! [`LineNumberRenderable`](crate::renderables::line_number) wrapper
//! (width `100%`/`50%`) → `[GutterRenderable, CodeRenderable]`
//! (Diff.ts:427-484). The settled frame is what the differential oracle
//! captures, and the port builds exactly that tree:
//!
//! * `build_diff_view` is the faithful `buildView` split
//!   (`buildUnifiedView` Diff.ts:486-588 / `buildSplitView`
//!   Diff.ts:590-926), parameterized by the wrap-alignment inputs —
//!   upstream computes them from the live CodeRenderables' lineInfo at
//!   rebuild time (:755-821); the port feeds the measured per-logical-
//!   line visual counts through the renderer's settle loop
//!   (upstream: `queueMicrotask` rebuild :221-234 + `line-info-change`
//!   :248-288 — same fixed point, reached synchronously).
//! * the error view (`buildErrorView`, Diff.ts:310-365) renders the
//!   message text + the raw diff through a `filetype: "diff"` code
//!   block (no bundled parser → plain, on both sides).
//!
//! `getHunkRowOffsets` (Diff.ts:1254-1281) is ported as a pure
//! function — the opencode-facing hunk anchor API.

use crate::color::{parse_color, Rgba};
use crate::layout::Dimension;
use crate::renderables::line_number::{LineColorConfig, LineColorValue, LineSign};
use crate::renderables::text::WrapMode;
use std::collections::{HashMap, HashSet};

/// `StructuredPatch` — the slice Diff.ts consumes (`parsePatch` result
/// item; `hunks` only — filenames parsed for control-flow fidelity).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StructuredPatch {
    pub hunks: Vec<Hunk>,
}

/// One hunk — `parseHunk` (parse.js:445-510).
#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    /// f64 to carry the JS `+undefined → NaN` hunk-header edge
    /// (parse.js:449-452); real diffs are integral.
    pub old_start: f64,
    pub old_lines: f64,
    pub new_start: f64,
    pub new_lines: f64,
    pub lines: Vec<String>,
}

// ---------------------------------------------------------------------------
// parsePatch — jsdiff 9.0.0 `patch/parse.js`, line-faithful.
// ---------------------------------------------------------------------------

const EQUALS_RULE: &str = "===================================================================";

fn is_git_diff_header(line: &str) -> bool {
    // /^diff --git /
    line.starts_with("diff --git ")
}

fn is_diff_header(line: &str) -> bool {
    // /^diff --git / || /^Index:\s/ || /^diff(?: -r \w+)+\s/
    is_git_diff_header(line)
        || (line.starts_with("Index:") && line[6..].starts_with(js_whitespace()))
        || is_diff_r_header(line)
}

/// The whitespace class `\s` (JS): space, \t, \n, \v, \f, \r.
fn js_whitespace() -> &'static str {
    " \t\n\u{0b}\u{0c}\r"
}

fn starts_with_js_whitespace(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| js_whitespace().contains(c))
}

/// `/^diff(?: -r \w+)+\s/` — "diff" then one or more " -r \w+" groups
/// then one whitespace char.
fn is_diff_r_header(line: &str) -> bool {
    let mut rest = line;
    if !rest.starts_with("diff") {
        return false;
    }
    rest = &rest[4..];
    let mut groups = 0;
    while let Some(tail) = rest.strip_prefix(" -r ") {
        // \w+ = [A-Za-z0-9_]+
        let word_len = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .count();
        if word_len == 0 {
            break;
        }
        rest = &rest[4 + word_len..];
        groups += 1;
    }
    groups > 0 && starts_with_js_whitespace(rest)
}

fn is_file_header(line: &str) -> bool {
    // /^(---|\+\+\+)\s/
    (line.starts_with("---") || line.starts_with("+++"))
        && line.len() > 3
        && starts_with_js_whitespace(&line[3..])
}

fn is_hunk_header(line: &str) -> bool {
    // /^@@\s/
    line.starts_with("@@") && line.len() > 2 && starts_with_js_whitespace(&line[2..])
}

/// `split(/@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/)` — the FIRST
/// match anywhere in the line (JS split semantics), with the four
/// captured groups.
fn split_hunk_header(line: &str) -> Option<[Option<String>; 4]> {
    let bytes = line.as_bytes();
    let mut probe = 0usize;
    while probe + 4 <= bytes.len() {
        let at = match line[probe..].find("@@ -") {
            Some(offset) => probe + offset,
            None => break,
        };
        let mut cursor = at + 4;
        // (\d+)
        let old_start = take_digits(line, &mut cursor);
        let Some(old_start) = old_start else {
            probe = at + 1;
            continue;
        };
        let old_count = if line[cursor..].starts_with(',') {
            cursor += 1;
            take_digits(line, &mut cursor)
        } else {
            None
        };
        if !line[cursor..].starts_with(" +") {
            probe = at + 1;
            continue;
        }
        cursor += 2;
        let new_start = take_digits(line, &mut cursor);
        let Some(new_start) = new_start else {
            probe = at + 1;
            continue;
        };
        let new_count = if line[cursor..].starts_with(',') {
            cursor += 1;
            take_digits(line, &mut cursor)
        } else {
            None
        };
        if !line[cursor..].starts_with(" @@") {
            probe = at + 1;
            continue;
        }
        return Some([Some(old_start), old_count, Some(new_start), new_count]);
    }
    None
}

fn take_digits(line: &str, cursor: &mut usize) -> Option<String> {
    let start = *cursor;
    while *cursor < line.len() && line.as_bytes()[*cursor].is_ascii_digit() {
        *cursor += 1;
    }
    if *cursor > start {
        Some(line[start..*cursor].to_owned())
    } else {
        None
    }
}

/// `parsePatch` (parse.js:17-515). Errors carry the exact JS message —
/// Diff.ts renders it (`Error parsing diff: ${message}`, Diff.ts:322).
pub fn parse_patch(uni_diff: &str) -> Result<Vec<StructuredPatch>, String> {
    let diffstr: Vec<&str> = uni_diff.split('\n').collect();
    let mut list: Vec<StructuredPatch> = Vec::new();
    let mut i = 0usize;

    // --- nested helpers over (diffstr, i, list) -------------------------

    // parseFileHeader (parse.js:421-442): consumes one `---`/`+++` line
    // (`/^(---|\+\+\+)\s+/`); returns which filename slot it filled.
    fn parse_file_header(diffstr: &[&str], i: &mut usize) -> Option<bool> {
        let line = diffstr.get(*i).copied().unwrap_or_default();
        let is_old = line.starts_with("---");
        let is_new = line.starts_with("+++");
        let matched = (is_old || is_new) && line.len() > 3 && starts_with_js_whitespace(&line[3..]);
        if !matched {
            return None;
        }
        *i += 1;
        Some(is_old)
    }

    // parseGitDiffHeader (parse.js:244-317): splits a `diff --git` line
    // into old/new path strings — only their PRESENCE matters for the
    // hunks, but the same consumption rules apply. Returns
    // (oldFileName?, newFileName?).
    fn parse_git_diff_header(line: &str) -> (Option<String>, Option<String>) {
        let rest = &line["diff --git ".len()..];
        if rest.starts_with('"') {
            let Some((old, raw_len)) = parse_quoted_file_name(rest) else {
                return (None, None);
            };
            let after_old = &rest[raw_len + 1..]; // +1 for space
            let new = match after_old.strip_prefix('"') {
                Some(_) => match parse_quoted_file_name(after_old) {
                    Some((name, _)) => Some(name),
                    None => return (Some(old), None),
                },
                None => Some(after_old.to_owned()),
            };
            return (Some(old), new);
        }
        if let Some(quote_idx) = rest.find('"') {
            if quote_idx > 0 {
                let old = rest[..quote_idx.saturating_sub(1)].to_owned();
                return match parse_quoted_file_name(&rest[quote_idx..]) {
                    Some((new, _)) => (Some(old), Some(new)),
                    None => (Some(old), None),
                };
            }
        }
        if rest.starts_with("a/") {
            // split on the middle " b/" occurrence (parse.js:297-313)
            let mut splits: Vec<usize> = Vec::new();
            let mut idx = 0usize;
            while let Some(offset) = rest[idx + 1..].find(" b/") {
                idx += 1 + offset;
                splits.push(idx);
            }
            if let Some(&mid) = splits.get(splits.len() / 2) {
                return (
                    Some(rest[..mid].to_owned()),
                    Some(rest[mid + 1..].to_owned()),
                );
            }
        }
        (None, None)
    }

    // parseQuotedFileName (parse.js:335-418): C-style quoted name →
    // (fileName, rawLength incl. quotes). Operates on chars where the
    // original indexes UTF-16 units — equivalent for the escapes Git
    // emits (ASCII); non-ASCII raw names only differ in unobservable
    // ways (filenames never reach the hunks).
    fn parse_quoted_file_name(s: &str) -> Option<(String, usize)> {
        let chars: Vec<char> = s.chars().collect();
        if chars.first() != Some(&'"') {
            return None;
        }
        let mut result = String::new();
        let mut j = 1usize; // skip opening quote
        while j < chars.len() {
            let ch = chars[j];
            if ch == '"' {
                let raw_len: usize = chars[..j + 1].iter().map(|c| c.len_utf8()).sum();
                return Some((result, raw_len));
            }
            if ch == '\\' && j + 1 < chars.len() {
                j += 1;
                match chars[j] {
                    'a' => result.push('\u{7}'),
                    'b' => result.push('\u{8}'),
                    'f' => result.push('\u{c}'),
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    'v' => result.push('\u{b}'),
                    '\\' => result.push('\\'),
                    '"' => result.push('"'),
                    first @ '0'..='7' => {
                        // C-style octal escapes: raw bytes decoded as
                        // UTF-8 (parse.js:384-400). chars[j] is the first
                        // octal digit; the escape spans j..j+2.
                        let _ = first;
                        if j + 2 >= chars.len()
                            || !('0'..='7').contains(&chars[j + 1])
                            || !('0'..='7').contains(&chars[j + 2])
                        {
                            return None;
                        }
                        let _ = first;
                        let oct: String = chars[j..=j + 2].iter().collect();
                        let mut bytes = vec![u8::from_str_radix(&oct, 8).ok()?];
                        j += 3;
                        while chars.get(j) == Some(&'\\')
                            && chars.get(j + 1).is_some_and(|c| ('0'..='7').contains(c))
                        {
                            if j + 3 >= chars.len()
                                || !('0'..='7').contains(&chars[j + 2])
                                || !('0'..='7').contains(&chars[j + 3])
                            {
                                return None;
                            }
                            let oct: String = chars[j + 1..=j + 3].iter().collect();
                            bytes.push(u8::from_str_radix(&oct, 8).ok()?);
                            j += 4;
                        }
                        result.push_str(&String::from_utf8_lossy(&bytes));
                        continue; // j already points at the next character
                    }
                    _ => return None,
                }
            } else {
                result.push(ch);
            }
            j += 1;
        }
        None // Unterminated quote
    }

    // parseHunk (parse.js:445-510)
    fn parse_hunk(diffstr: &[&str], i: &mut usize) -> Result<Hunk, String> {
        let chunk_header_index = *i;
        let chunk_header_line = diffstr[*i];
        *i += 1;
        // split(/@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/) — starts are
        // `+undefined → NaN` when absent, counts default to 1
        // (parse.js:449-452)
        let (old_start, old_lines, new_start, new_lines) =
            match &split_hunk_header(chunk_header_line) {
                Some([a, b, c, d]) => (
                    a.as_deref()
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(f64::NAN),
                    match b {
                        Some(s) => s.parse::<f64>().unwrap_or(f64::NAN),
                        None => 1.0,
                    },
                    c.as_deref()
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(f64::NAN),
                    match d {
                        Some(s) => s.parse::<f64>().unwrap_or(f64::NAN),
                        None => 1.0,
                    },
                ),
                None => (f64::NAN, 1.0, f64::NAN, 1.0),
            };
        let mut hunk = Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines: Vec::new(),
        };
        // Unified Diff Format quirk: chunk size 0 → first number one lower (parse.js:458-463)
        if hunk.old_lines == 0.0 {
            hunk.old_start += 1.0;
        }
        if hunk.new_lines == 0.0 {
            hunk.new_start += 1.0;
        }
        let mut add_count = 0.0f64;
        let mut remove_count = 0.0f64;
        while *i < diffstr.len()
            && (remove_count < hunk.old_lines
                || add_count < hunk.new_lines
                || diffstr[*i].starts_with('\\'))
        {
            let line = diffstr[*i];
            // (parse.js:466) empty non-last line reads as ' ' context
            let operation = if line.is_empty() && *i != diffstr.len() - 1 {
                ' '
            } else {
                line.chars().next().unwrap_or('\0')
            };
            if matches!(operation, '+' | '-' | ' ' | '\\') {
                hunk.lines.push(line.to_owned());
                match operation {
                    '+' => add_count += 1.0,
                    '-' => remove_count += 1.0,
                    ' ' => {
                        add_count += 1.0;
                        remove_count += 1.0;
                    }
                    _ => {}
                }
            } else {
                return Err(format!(
                    "Hunk at line {} contained invalid line {}",
                    chunk_header_index + 1,
                    line
                ));
            }
            *i += 1;
        }
        // Handle the empty block count case (parse.js:485-490)
        if add_count == 0.0 && hunk.new_lines == 1.0 {
            hunk.new_lines = 0.0;
        }
        if remove_count == 0.0 && hunk.old_lines == 1.0 {
            hunk.old_lines = 0.0;
        }
        // Perform sanity checking (parse.js:492-497)
        if add_count != hunk.new_lines {
            return Err(format!(
                "Added line count did not match for hunk at line {}",
                chunk_header_index + 1
            ));
        }
        if remove_count != hunk.old_lines {
            return Err(format!(
                "Removed line count did not match for hunk at line {}",
                chunk_header_index + 1
            ));
        }
        // Check for extra hunk-body-like lines (parse.js:498-508)
        if *i < diffstr.len() {
            let next = diffstr[*i];
            let extra = next
                .chars()
                .next()
                .is_some_and(|c| c == '+' || c == ' ' || c == '-');
            if extra && !is_file_header(next) {
                return Err(format!(
                    "Hunk at line {} has more lines than expected (expected {} old lines and {} new lines)",
                    chunk_header_index + 1,
                    format_js_number(hunk.old_lines),
                    format_js_number(hunk.new_lines)
                ));
            }
        }
        Ok(hunk)
    }

    // parseIndex (parse.js:42-205) — returns early (Some) when a new
    // file's diff header was reached mid-parse.
    fn parse_index(
        diffstr: &[&str],
        i: &mut usize,
        list: &mut Vec<StructuredPatch>,
    ) -> Result<(), String> {
        // parseIndex pushes its (possibly still-empty) index FIRST
        // (parse.js:42-46) — the early-return below leaves it in the list
        let list_len = list.len();
        list.push(StructuredPatch::default());
        let mut old_file_name: Option<String> = None;
        let mut new_file_name: Option<String> = None;
        let mut seen_diff_header = false;
        let mut early_return = false;
        while *i < diffstr.len() {
            let line = diffstr[*i];
            if is_file_header(line) || is_hunk_header(line) {
                break;
            }
            if is_git_diff_header(line) {
                if seen_diff_header {
                    early_return = true;
                    break;
                }
                seen_diff_header = true;
                let (old, new) = parse_git_diff_header(line);
                old_file_name = old;
                new_file_name = new;
                // Consume Git extended headers (parse.js:90-148)
                *i += 1;
                while *i < diffstr.len() {
                    let ext = diffstr[*i];
                    if is_file_header(ext) || is_hunk_header(ext) || is_diff_header(ext) {
                        break;
                    }
                    if let Some(name) = ext.strip_prefix("rename from ") {
                        old_file_name = Some(format!("a/{}", unquote_if_quoted(name)));
                    }
                    if let Some(name) = ext.strip_prefix("rename to ") {
                        new_file_name = Some(format!("b/{}", unquote_if_quoted(name)));
                    }
                    if let Some(name) = ext.strip_prefix("copy from ") {
                        old_file_name = Some(format!("a/{}", unquote_if_quoted(name)));
                    }
                    if let Some(name) = ext.strip_prefix("copy to ") {
                        new_file_name = Some(format!("b/{}", unquote_if_quoted(name)));
                    }
                    *i += 1;
                }
                continue;
            } else if is_diff_header(line) {
                if seen_diff_header {
                    early_return = true;
                    break;
                }
                seen_diff_header = true;
                // index name extraction (parse.js:171-174) — unused by Diff
            }
            *i += 1;
        }
        // Parse file headers if they are defined (parse.js:180-181)
        if let Some(is_old) = parse_file_header(diffstr, i) {
            let name = file_name_of(diffstr.get(i.wrapping_sub(1)).copied().unwrap_or_default());
            if is_old {
                old_file_name = Some(name);
            } else {
                new_file_name = Some(name);
            }
        }
        if let Some(is_old) = parse_file_header(diffstr, i) {
            let name = file_name_of(diffstr.get(i.wrapping_sub(1)).copied().unwrap_or_default());
            if is_old {
                old_file_name = Some(name);
            } else {
                new_file_name = Some(name);
            }
        }
        // Malformed patch: one header without the other (parse.js:183-186)
        if old_file_name.is_none() != new_file_name.is_none() {
            let name = old_file_name.as_ref().or(new_file_name.as_ref());
            return Err(format!(
                "Missing {} file header for {}",
                if old_file_name.is_some() {
                    "\"+++ ...\""
                } else {
                    "\"--- ...\""
                },
                name.map(String::as_str).unwrap_or("undefined")
            ));
        }
        // parseIndex returns when a NEW file's diff header was reached
        // mid-metadata (parse.js:65-68, 151-154) — the pushed index keeps
        // whatever hunks it has (none, on this path)
        if early_return {
            return Ok(());
        }
        let mut hunks: Vec<Hunk> = Vec::new();
        while *i < diffstr.len() {
            let line = diffstr[*i];
            if is_diff_header(line) || is_file_header(line) || line.starts_with(EQUALS_RULE) {
                break;
            } else if is_hunk_header(line) {
                hunks.push(parse_hunk(diffstr, i)?);
            } else {
                // Skip blank lines and other unrecognized content (parse.js:196-203)
                *i += 1;
            }
        }
        list[list_len].hunks = hunks;
        Ok(())
    }

    fn file_name_of(header_line: &str) -> String {
        // parseFileHeader's data[0] (parse.js:424-431)
        let data = header_line[3..].trim();
        let file_name = data.split('\t').next().unwrap_or_default();
        if file_name.starts_with('"') {
            unquote_if_quoted(file_name)
        } else {
            // replace(/\\\\/g, '\\') — collapse double backslashes
            file_name.replace("\\\\", "\\")
        }
    }

    fn unquote_if_quoted(s: &str) -> String {
        if s.starts_with('"') {
            if let Some((name, _)) = parse_quoted_file_name(s) {
                return name;
            }
        }
        s.to_owned()
    }

    while i < diffstr.len() {
        parse_index(&diffstr, &mut i, &mut list)?;
    }
    Ok(list)
}

/// JS number → string for the error text (`${hunk.oldLines}`).
fn format_js_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_owned()
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

// ---------------------------------------------------------------------------
// DiffRenderable — the view builders.
// ---------------------------------------------------------------------------

/// `view` — Diff.ts:25 ("unified" | "split").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffView {
    #[default]
    Unified,
    Split,
}

impl DiffView {
    pub fn parse(s: &str) -> DiffView {
        match s {
            "split" => DiffView::Split,
            _ => DiffView::Unified,
        }
    }
}

/// `DiffRenderableOptions` — Diff.ts:22-53 (colors pre-parsed like the
/// constructor, Diff.ts:110-151).
#[derive(Debug, Clone)]
pub struct DiffOptions {
    pub diff: String,
    pub view: DiffView,
    // CodeRenderable options
    pub filetype: Option<String>,
    pub wrap_mode: Option<WrapMode>,
    pub conceal: bool,
    pub fg: Option<Rgba>,
    // LineNumberRenderable options
    pub show_line_numbers: bool,
    pub line_number_fg: Rgba,
    pub line_number_bg: Rgba,
    // Diff styling (Diff.ts:135-145)
    pub added_bg: Rgba,
    pub removed_bg: Rgba,
    pub context_bg: Rgba,
    pub added_content_bg: Option<Rgba>,
    pub removed_content_bg: Option<Rgba>,
    pub context_content_bg: Option<Rgba>,
    pub added_sign_color: Rgba,
    pub removed_sign_color: Rgba,
    pub added_line_number_bg: Rgba,
    pub removed_line_number_bg: Rgba,
    // root layout (RenderableOptions)
    pub width: Dimension,
    pub height: Dimension,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions {
            diff: String::new(),
            view: DiffView::Unified,
            filetype: None,
            wrap_mode: None,
            conceal: false,
            fg: None,
            show_line_numbers: true,
            line_number_fg: parse_color("#888888"),
            line_number_bg: parse_color("transparent"),
            added_bg: parse_color("#1a4d1a"),
            removed_bg: parse_color("#4d1a1a"),
            context_bg: parse_color("transparent"),
            added_content_bg: None,
            removed_content_bg: None,
            context_content_bg: None,
            added_sign_color: parse_color("#22c55e"),
            removed_sign_color: parse_color("#ef4444"),
            added_line_number_bg: parse_color("transparent"),
            removed_line_number_bg: parse_color("transparent"),
            width: Dimension::Percent(1.0),
            height: Dimension::Percent(1.0),
        }
    }
}

/// `LogicalLine` — Diff.ts:12-20.
#[derive(Debug, Clone, Default, PartialEq)]
struct LogicalLine {
    content: String,
    line_num: Option<f64>,
    hide_line_number: bool,
    #[allow(dead_code)]
    color: Option<Rgba>,
    sign: Option<LineSign>,
    kind: LineKind,
    hunk_start: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum LineKind {
    #[default]
    Context,
    Add,
    Remove,
    Empty,
}

/// The per-side inputs a settled view carries — what
/// `createOrUpdateSide` forwards (Diff.ts:427-484).
#[derive(Debug, Clone, Default)]
pub struct SideSpec {
    pub content: String,
    pub line_colors: HashMap<i64, LineColorValue>,
    pub line_signs: HashMap<i64, LineSign>,
    pub line_numbers: HashMap<i64, i64>,
    pub hide_line_numbers: HashSet<i64>,
    /// `"100%"` (unified left) | `"50%"` (split) — Diff.ts:434.
    pub width: Dimension,
}

/// The settled `buildView` output.
#[derive(Debug, Clone, Default)]
pub struct DiffBuild {
    /// `Error parsing diff: <message>` (Diff.ts:322)
    pub error: Option<String>,
    pub sides: Vec<SideSpec>,
    /// `_hunkStartLines` (Diff.ts:62, pushed at Diff.ts:513/833-836)
    pub hunk_start_lines: Vec<usize>,
}

/// `parseDiff` — Diff.ts:153-175: `patches[0]`, `[]` → none, throw →
/// error. Returns (patch, error-message).
fn parse_diff(text: &str) -> (Option<StructuredPatch>, Option<String>) {
    if text.is_empty() {
        return (None, None);
    }
    match parse_patch(text) {
        Ok(patches) => {
            if patches.is_empty() {
                (None, None)
            } else {
                (Some(patches[0].clone()), None)
            }
        }
        Err(message) => (None, Some(message)),
    }
}

/// `buildView` — Diff.ts:177-195 + the two view builders. The
/// alignment counts are the live CodeRenderables' per-logical-line
/// visual row counts (`leftVisualCounts`/`rightVisualCounts`,
/// Diff.ts:762-770) — `None` until the first layout provides widths
/// (`canDoWrapAlignment`, Diff.ts:718).
pub fn build_diff_view(
    options: &DiffOptions,
    visual_counts: Option<&SideVisualCounts>,
) -> DiffBuild {
    let (parsed, error) = parse_diff(&options.diff);
    let mut build = DiffBuild::default();
    if let Some(message) = error {
        build.error = Some(message);
        return build;
    }
    let Some(parsed) = parsed else {
        return build;
    };
    if parsed.hunks.is_empty() {
        return build;
    }
    match options.view {
        DiffView::Unified => {
            build.sides = vec![build_unified_side(
                options,
                &parsed,
                &mut build.hunk_start_lines,
            )];
        }
        DiffView::Split => {
            let (left, right, hunk_first_left) = split_logical_lines(options, &parsed);
            let (mut final_left, final_right) = align_split_lines(&left, &right, visual_counts);
            // Mark after wrap-alignment so padding does not lose the hunk
            // start (Diff.ts:712-716 — pre-padding flags carried through)
            // → the padded indices land in _hunkStartLines (Diff.ts:832-836)
            for &start in &hunk_first_left {
                if let Some(first) = final_left.get_mut(start) {
                    first.hunk_start = true;
                }
            }
            let (left_spec, right_spec) = split_side_specs(options, &final_left, &final_right);
            build.sides = vec![left_spec, right_spec];
            for (index, line) in final_left.iter().enumerate() {
                if line.hunk_start {
                    build.hunk_start_lines.push(index);
                }
            }
        }
    }
    build
}

/// The measured alignment inputs — one count per LOGICAL line
/// (`leftVisualCounts`, Diff.ts:762-770: occurrences of each logical
/// line in the rendered lineSources).
#[derive(Debug, Clone, Default)]
pub struct SideVisualCounts {
    pub left: Vec<u32>,
    pub right: Vec<u32>,
}

/// `buildUnifiedView`'s line stream — Diff.ts:504-588: hunks flatten
/// straight into one content string with per-line colors/signs/numbers.
fn build_unified_side(
    options: &DiffOptions,
    parsed: &StructuredPatch,
    hunk_start_lines: &mut Vec<usize>,
) -> SideSpec {
    let mut content_lines: Vec<String> = Vec::new();
    let mut line_colors = HashMap::new();
    let mut line_signs = HashMap::new();
    let mut line_numbers = HashMap::new();
    let mut line_index = 0usize;

    for hunk in &parsed.hunks {
        // Unified view flattens hunks directly into the left
        // CodeRenderable line stream (Diff.ts:512)
        hunk_start_lines.push(line_index);

        let mut old_line_num = hunk.old_start;
        let mut new_line_num = hunk.new_start;

        for line in &hunk.lines {
            let first_char = line.chars().next();
            let content = first_char.map_or(line.as_str(), |c| &line[c.len_utf8()..]);

            match first_char {
                Some('+') => {
                    content_lines.push(content.to_owned());
                    let config = LineColorConfig {
                        gutter: Some(options.added_line_number_bg),
                        content: Some(options.added_content_bg.unwrap_or(options.added_bg)),
                    };
                    line_colors.insert(line_index as i64, LineColorValue::Config(config));
                    line_signs.insert(
                        line_index as i64,
                        LineSign {
                            after: Some(" +".to_owned()),
                            after_color: Some(options.added_sign_color),
                            ..Default::default()
                        },
                    );
                    line_numbers.insert(line_index as i64, new_line_num as i64);
                    new_line_num += 1.0;
                    line_index += 1;
                }
                Some('-') => {
                    content_lines.push(content.to_owned());
                    let config = LineColorConfig {
                        gutter: Some(options.removed_line_number_bg),
                        content: Some(options.removed_content_bg.unwrap_or(options.removed_bg)),
                    };
                    line_colors.insert(line_index as i64, LineColorValue::Config(config));
                    line_signs.insert(
                        line_index as i64,
                        LineSign {
                            after: Some(" -".to_owned()),
                            after_color: Some(options.removed_sign_color),
                            ..Default::default()
                        },
                    );
                    line_numbers.insert(line_index as i64, old_line_num as i64);
                    old_line_num += 1.0;
                    line_index += 1;
                }
                Some(' ') => {
                    content_lines.push(content.to_owned());
                    let config = LineColorConfig {
                        gutter: Some(options.line_number_bg),
                        content: Some(options.context_content_bg.unwrap_or(options.context_bg)),
                    };
                    line_colors.insert(line_index as i64, LineColorValue::Config(config));
                    line_numbers.insert(line_index as i64, new_line_num as i64);
                    old_line_num += 1.0;
                    new_line_num += 1.0;
                    line_index += 1;
                }
                _ => {}
            }
        }
    }

    SideSpec {
        content: content_lines.join("\n"),
        line_colors,
        line_signs,
        line_numbers,
        hide_line_numbers: HashSet::new(),
        width: Dimension::Percent(1.0),
    }
}

/// `buildSplitView`'s logical pass — Diff.ts:608-710: hunks pair
/// remove/add runs index-by-index, padding the shorter side with
/// empty/hideLineNumber lines.
fn split_logical_lines(
    options: &DiffOptions,
    parsed: &StructuredPatch,
) -> (Vec<LogicalLine>, Vec<LogicalLine>, Vec<usize>) {
    let mut left: Vec<LogicalLine> = Vec::new();
    let mut right: Vec<LogicalLine> = Vec::new();
    let mut hunk_first_left: Vec<usize> = Vec::new();

    for hunk in &parsed.hunks {
        // Split view may insert padding later, so carry hunk starts
        // through LogicalLine metadata (Diff.ts:613)
        hunk_first_left.push(left.len());

        let mut old_line_num = hunk.old_start;
        let mut new_line_num = hunk.new_start;

        let mut i = 0usize;
        while i < hunk.lines.len() {
            let line = &hunk.lines[i];
            let first_char = line.chars().next();

            if first_char == Some(' ') {
                let content = &line[1..];
                left.push(LogicalLine {
                    content: content.to_owned(),
                    line_num: Some(old_line_num),
                    color: Some(options.context_bg),
                    kind: LineKind::Context,
                    ..Default::default()
                });
                right.push(LogicalLine {
                    content: content.to_owned(),
                    line_num: Some(new_line_num),
                    color: Some(options.context_bg),
                    kind: LineKind::Context,
                    ..Default::default()
                });
                old_line_num += 1.0;
                new_line_num += 1.0;
                i += 1;
            } else if first_char == Some('\\') {
                i += 1;
            } else {
                let mut removes: Vec<(String, f64)> = Vec::new();
                let mut adds: Vec<(String, f64)> = Vec::new();

                while i < hunk.lines.len() {
                    let current = &hunk.lines[i];
                    let current_char = current.chars().next();
                    if current_char == Some(' ') || current_char == Some('\\') {
                        break;
                    }
                    let content = &current[1..];
                    match current_char {
                        Some('-') => {
                            removes.push((content.to_owned(), old_line_num));
                            old_line_num += 1.0;
                        }
                        Some('+') => {
                            adds.push((content.to_owned(), new_line_num));
                            new_line_num += 1.0;
                        }
                        _ => {}
                    }
                    i += 1;
                }

                let max_length = removes.len().max(adds.len());
                for j in 0..max_length {
                    if let Some((content, line_num)) = removes.get(j) {
                        left.push(LogicalLine {
                            content: content.clone(),
                            line_num: Some(*line_num),
                            color: Some(options.removed_bg),
                            sign: Some(LineSign {
                                after: Some(" -".to_owned()),
                                after_color: Some(options.removed_sign_color),
                                ..Default::default()
                            }),
                            kind: LineKind::Remove,
                            ..Default::default()
                        });
                    } else {
                        left.push(LogicalLine {
                            content: String::new(),
                            hide_line_number: true,
                            kind: LineKind::Empty,
                            ..Default::default()
                        });
                    }

                    if let Some((content, line_num)) = adds.get(j) {
                        right.push(LogicalLine {
                            content: content.clone(),
                            line_num: Some(*line_num),
                            color: Some(options.added_bg),
                            sign: Some(LineSign {
                                after: Some(" +".to_owned()),
                                after_color: Some(options.added_sign_color),
                                ..Default::default()
                            }),
                            kind: LineKind::Add,
                            ..Default::default()
                        });
                    } else {
                        right.push(LogicalLine {
                            content: String::new(),
                            hide_line_number: true,
                            kind: LineKind::Empty,
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
    (left, right, hunk_first_left)
}

/// The wrap-alignment pass — Diff.ts:755-821: walk both logical streams
/// keeping the VISUAL positions in lockstep, padding the shorter side
/// with empty/hideLineNumber lines. `None` counts → no alignment
/// (`canDoWrapAlignment` false: width 0 or wrapMode none, Diff.ts:718);
/// concealed logical lines count ZERO visual rows (Diff.ts:782-784).
fn align_split_lines(
    left: &[LogicalLine],
    right: &[LogicalLine],
    counts: Option<&SideVisualCounts>,
) -> (Vec<LogicalLine>, Vec<LogicalLine>) {
    let Some(counts) = counts else {
        return (left.to_vec(), right.to_vec());
    };
    let left_count = |i: usize| counts.left.get(i).copied().unwrap_or(0) as i64;
    let right_count = |i: usize| counts.right.get(i).copied().unwrap_or(0) as i64;

    let mut final_left: Vec<LogicalLine> = Vec::new();
    let mut final_right: Vec<LogicalLine> = Vec::new();
    let mut left_visual_pos = 0i64;
    let mut right_visual_pos = 0i64;

    let empty = || LogicalLine {
        content: String::new(),
        hide_line_number: true,
        kind: LineKind::Empty,
        ..Default::default()
    };

    for (i, left_line) in left.iter().enumerate() {
        if left_visual_pos < right_visual_pos {
            let pad = right_visual_pos - left_visual_pos;
            for _ in 0..pad {
                final_left.push(empty());
            }
            left_visual_pos += pad;
        } else if right_visual_pos < left_visual_pos {
            let pad = left_visual_pos - right_visual_pos;
            for _ in 0..pad {
                final_right.push(empty());
            }
            right_visual_pos += pad;
        }

        final_left.push(left_line.clone());
        final_right.push(right.get(i).cloned().unwrap_or_else(empty));

        left_visual_pos += left_count(i);
        right_visual_pos += right_count(i);
    }

    if left_visual_pos < right_visual_pos {
        let pad = right_visual_pos - left_visual_pos;
        for _ in 0..pad {
            final_left.push(empty());
        }
    } else if right_visual_pos < left_visual_pos {
        let pad = left_visual_pos - right_visual_pos;
        for _ in 0..pad {
            final_right.push(empty());
        }
    }
    (final_left, final_right)
}

/// The final map assembly — Diff.ts:823-925: per final line index, the
/// colors/signs/numbers/hide sets each side's gutter receives.
fn split_side_specs(
    options: &DiffOptions,
    final_left: &[LogicalLine],
    final_right: &[LogicalLine],
) -> (SideSpec, SideSpec) {
    let mut left_spec = SideSpec {
        width: Dimension::Percent(0.5),
        ..Default::default()
    };
    let mut right_spec = SideSpec {
        width: Dimension::Percent(0.5),
        ..Default::default()
    };

    for (index, line) in final_left.iter().enumerate() {
        let index = index as i64;
        if let Some(line_num) = line.line_num {
            left_spec.line_numbers.insert(index, line_num as i64);
        }
        if line.hide_line_number {
            left_spec.hide_line_numbers.insert(index);
        }
        match line.kind {
            LineKind::Remove => {
                left_spec.line_colors.insert(
                    index,
                    LineColorValue::Config(LineColorConfig {
                        gutter: Some(options.removed_line_number_bg),
                        content: Some(options.removed_content_bg.unwrap_or(options.removed_bg)),
                    }),
                );
            }
            LineKind::Context => {
                left_spec.line_colors.insert(
                    index,
                    LineColorValue::Config(LineColorConfig {
                        gutter: Some(options.line_number_bg),
                        content: Some(options.context_content_bg.unwrap_or(options.context_bg)),
                    }),
                );
            }
            _ => {}
        }
        if let Some(sign) = &line.sign {
            left_spec.line_signs.insert(index, sign.clone());
        }
    }

    for (index, line) in final_right.iter().enumerate() {
        let index = index as i64;
        if let Some(line_num) = line.line_num {
            right_spec.line_numbers.insert(index, line_num as i64);
        }
        if line.hide_line_number {
            right_spec.hide_line_numbers.insert(index);
        }
        match line.kind {
            LineKind::Add => {
                right_spec.line_colors.insert(
                    index,
                    LineColorValue::Config(LineColorConfig {
                        gutter: Some(options.added_line_number_bg),
                        content: Some(options.added_content_bg.unwrap_or(options.added_bg)),
                    }),
                );
            }
            LineKind::Context => {
                right_spec.line_colors.insert(
                    index,
                    LineColorValue::Config(LineColorConfig {
                        gutter: Some(options.line_number_bg),
                        content: Some(options.context_content_bg.unwrap_or(options.context_bg)),
                    }),
                );
            }
            _ => {}
        }
        if let Some(sign) = &line.sign {
            right_spec.line_signs.insert(index, sign.clone());
        }
    }

    left_spec.content = final_left
        .iter()
        .map(|l| l.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    right_spec.content = final_right
        .iter()
        .map(|l| l.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    (left_spec, right_spec)
}

/// `computeHunkRowOffsets` — Diff.ts:1261-1281: resolve each hunk-start
/// LOGICAL line to its VISUAL row (concealed hunk lines navigate to the
/// next visible row). `sources` — the rendered lineSources array.
#[must_use]
pub fn compute_hunk_row_offsets(hunk_start_lines: &[usize], sources: &[u32]) -> Vec<usize> {
    if hunk_start_lines.is_empty() {
        return Vec::new();
    }
    if sources.is_empty() {
        return hunk_start_lines.to_vec();
    }

    let mut offsets = Vec::new();
    let mut visual_row = 0usize;

    for &hunk_start_line in hunk_start_lines {
        // If the exact hunk line is concealed, navigate to the next
        // visible row (Diff.ts:1273-1277)
        while visual_row < sources.len() && (sources[visual_row] as usize) < hunk_start_line {
            visual_row += 1;
        }
        offsets.push(if visual_row < sources.len() {
            visual_row
        } else {
            hunk_start_line
        });
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_DIFF: &str = "--- a/test.js\n+++ b/test.js\n@@ -1,3 +1,3 @@\n function hello() {\n-  console.log(\"Hello\");\n+  console.log(\"Hello, World!\");\n }";

    #[test]
    fn parse_patch_simple_hunk() {
        // parse.js:445-510 — counts, prefixes, quirk-free path
        let patches = parse_patch(SIMPLE_DIFF).expect("parses");
        assert_eq!(patches.len(), 1);
        let hunk = &patches[0].hunks[0];
        assert_eq!(hunk.old_start, 1.0);
        assert_eq!(hunk.old_lines, 3.0);
        assert_eq!(hunk.new_start, 1.0);
        assert_eq!(hunk.new_lines, 3.0);
        assert_eq!(
            hunk.lines,
            vec![
                " function hello() {",
                "-  console.log(\"Hello\");",
                "+  console.log(\"Hello, World!\");",
                " }"
            ]
        );
    }

    #[test]
    fn parse_patch_zero_count_quirk() {
        // parse.js:458-463 — chunk size 0 bumps the start by one
        let patch = "--- a/new.js\n+++ b/new.js\n@@ -0,0 +1,2 @@\n+a\n+b";
        let hunk = &parse_patch(patch).unwrap()[0].hunks[0];
        assert_eq!(hunk.old_start, 1.0); // 0 + 1
        assert_eq!(hunk.new_start, 1.0);
        assert_eq!(hunk.old_lines, 0.0);
        assert_eq!(hunk.lines, vec!["+a", "+b"]);
    }

    #[test]
    fn parse_patch_count_mismatch_errors() {
        // parse.js:492-497 — the exact JS error text (Diff.ts:322 renders it)
        let patch = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n x";
        assert_eq!(
            parse_patch(patch),
            Err("Added line count did not match for hunk at line 3".to_owned())
        );
        let patch = "--- a\n+++ b\n@@ -1,3 +1,1 @@\n-x\n-y\n+z";
        assert_eq!(
            parse_patch(patch),
            Err("Removed line count did not match for hunk at line 3".to_owned())
        );
    }

    #[test]
    fn parse_patch_extra_lines_error_matches_snapshot() {
        // The invalid-diff error Diff.test.ts pins (snapshot line 556):
        // "Hunk at line 3 has more lines than expected (expected 1 old
        // lines and 1 new lines)"
        let patch = "--- a/test.js\n+++ b/test.js\n@@ -a,b +c,d @@\n function hello() {\n-  console.log(\"Hello\");\n+  console.log(\"Hello, World!\");\n }";
        assert_eq!(
            parse_patch(patch),
            Err("Hunk at line 3 has more lines than expected (expected 1 old lines and 1 new lines)".to_owned())
        );
    }

    #[test]
    fn parse_patch_invalid_line_error() {
        // parse.js:481 — unparseable body line
        let patch = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n context\nbroken";
        assert_eq!(
            parse_patch(patch),
            Err("Hunk at line 3 contained invalid line broken".to_owned())
        );
    }

    #[test]
    fn parse_patch_missing_file_header_error() {
        // parse.js:183-186
        let patch = "--- a\n@@ -1,1 +1,1 @@\n x";
        assert_eq!(
            parse_patch(patch),
            Err("Missing \"+++ ...\" file header for a".to_owned())
        );
    }

    #[test]
    fn parse_patch_git_header_and_rename() {
        // parse.js:65-149 — git extended headers consumed between the
        // diff --git line and the file headers
        let patch = "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1,2 +1,2 @@\n x\n!y";
        // hunk body '!y' is invalid while counts are unmet → line 6
        assert_eq!(
            parse_patch(patch),
            Err("Hunk at line 6 contained invalid line !y".to_owned())
        );
        // rename headers carry the filenames (parse.js:105-113)
        let patch = "diff --git a/old.rs b/new.rs\nrename from old.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1,1 +1,1 @@\n x";
        assert!(parse_patch(patch).is_ok());
    }

    #[test]
    fn parse_patch_no_newline_marker_consumed() {
        // parse.js:465 — a '\\ No newline' line joins hunk.lines after counts
        let patch = "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n\\ No newline at end of file\n+y";
        let hunk = &parse_patch(patch).unwrap()[0].hunks[0];
        assert_eq!(hunk.lines, vec!["-x", "\\ No newline at end of file", "+y"]);
    }

    #[test]
    fn parse_patch_empty_last_line_is_context_when_needed() {
        // parse.js:466 — an empty NON-LAST line reads as ' ' context
        // (counts must cover it, else the extra-lines check fires)
        let patch = "--- a\n+++ b\n@@ -1,3 +1,3 @@\n x\n\n y";
        let hunk = &parse_patch(patch).unwrap()[0].hunks[0];
        assert_eq!(hunk.lines, vec![" x", "", " y"]);
    }

    #[test]
    fn parse_patch_multi_file_splits_on_second_header() {
        // parse.js:65-67/151-154 — a second diff header starts a new index
        let patch = "--- a\n+++ b\n@@ -1,1 +1,1 @@\n x\ndiff --git a/c b/d\n--- a/c\n+++ b/d\n@@ -1,1 +1,1 @@\n z";
        let patches = parse_patch(patch).unwrap();
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].hunks.len(), 1);
        assert_eq!(patches[1].hunks.len(), 1);
        // Diff.ts only consumes patches[0]
    }

    #[test]
    fn unified_side_flattens_hunks() {
        // buildUnifiedView — Diff.ts:504-588
        let options = DiffOptions {
            diff: SIMPLE_DIFF.to_owned(),
            ..Default::default()
        };
        let build = build_diff_view(&options, None);
        assert!(build.error.is_none());
        assert_eq!(build.sides.len(), 1);
        let side = &build.sides[0];
        assert_eq!(
            side.content,
            "function hello() {\n  console.log(\"Hello\");\n  console.log(\"Hello, World!\");\n}"
        );
        // line numbers: context→new, remove→old, add→new (Diff.ts:537,555,569)
        assert_eq!(side.line_numbers[&0], 1);
        assert_eq!(side.line_numbers[&1], 2); // old
        assert_eq!(side.line_numbers[&2], 2); // new
                                              // signs (Diff.ts:533-536, 551-554)
        assert_eq!(side.line_signs[&1].after.as_deref(), Some(" -"));
        assert_eq!(side.line_signs[&2].after.as_deref(), Some(" +"));
        // colors: remove → removedBg (Diff.ts:542-550)
        match &side.line_colors[&1] {
            LineColorValue::Config(config) => {
                assert_eq!(config.content, Some(parse_color("#4d1a1a")));
            }
            _ => panic!("config expected"),
        }
        // two hunks push two hunk-start anchors (Diff.ts:513)
        let options = DiffOptions {
            diff: "--- a\n+++ b\n@@ -1,1 +1,1 @@\n x\n@@ -5,1 +6,1 @@\n-y\n+z".to_owned(),
            ..Default::default()
        };
        let build = build_diff_view(&options, None);
        assert_eq!(build.hunk_start_lines, vec![0, 1]);
    }

    #[test]
    fn split_pairs_remove_add_runs() {
        // buildSplitView — Diff.ts:644-707: index-paired with empty padding
        let options = DiffOptions {
            diff: "--- a\n+++ b\n@@ -1,3 +1,5 @@\n ctx\n-r1\n-r2\n+a1\n+a2\n+a3\n+a4".to_owned(),
            view: DiffView::Split,
            ..Default::default()
        };
        let build = build_diff_view(&options, None);
        assert_eq!(build.sides.len(), 2);
        let (left, right) = (&build.sides[0], &build.sides[1]);
        assert_eq!(left.content, "ctx\nr1\nr2\n\n");
        assert_eq!(right.content, "ctx\na1\na2\na3\na4");
        // the shorter left side's padding lines hide their numbers
        assert!(left.hide_line_numbers.contains(&3));
        assert!(left.hide_line_numbers.contains(&4));
        assert!(!left.line_numbers.contains_key(&3));
        // numbers: left keeps old numbering, right new (Diff.ts:655-663)
        assert_eq!(left.line_numbers[&1], 2);
        assert_eq!(left.line_numbers[&2], 3);
        assert_eq!(right.line_numbers[&4], 5);
    }

    #[test]
    fn split_alignment_pads_shorter_visual_side() {
        // align_split_lines — Diff.ts:755-821
        let options = DiffOptions {
            diff: "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-a\n+b".to_owned(),
            view: DiffView::Split,
            ..Default::default()
        };
        // no counts → unaligned (canDoWrapAlignment false, Diff.ts:718)
        let build = build_diff_view(&options, None);
        assert_eq!(build.sides[0].content, "a");
        assert_eq!(build.sides[1].content, "b");

        // left line 0 wraps to 3 visual rows, right 1 → left side keeps,
        // right padded with 2 empty lines (Diff.ts:792-798 + tail :812-817)
        let counts = SideVisualCounts {
            left: vec![3],
            right: vec![1],
        };
        let build = build_diff_view(&options, Some(&counts));
        assert_eq!(build.sides[0].content, "a");
        assert_eq!(build.sides[1].content, "b\n\n");
        assert!(build.sides[1].hide_line_numbers.contains(&1));
        assert!(build.sides[1].hide_line_numbers.contains(&2));
    }

    #[test]
    fn hunk_row_offsets_resolve_concealed_starts() {
        // computeHunkRowOffsets — Diff.ts:1261-1281
        assert_eq!(compute_hunk_row_offsets(&[], &[0, 1]), Vec::<usize>::new());
        assert_eq!(compute_hunk_row_offsets(&[1], &[]), vec![1]);
        // sources [0,0,1,2,2]: hunk at logical 2 → first visual row 3
        assert_eq!(compute_hunk_row_offsets(&[2], &[0, 0, 1, 2, 2]), vec![3]);
        // a concealed hunk line navigates to the next visible row (:1273-1277)
        assert_eq!(compute_hunk_row_offsets(&[2], &[0, 3, 3]), vec![1]);
        // past the end → the raw line (:1277)
        assert_eq!(compute_hunk_row_offsets(&[5], &[0, 1]), vec![5]);
    }

    #[test]
    fn error_build_carries_message() {
        // parseDiff + buildErrorView — Diff.ts:153-175, 310-365
        let options = DiffOptions {
            diff: "not a diff at all".to_owned(),
            ..Default::default()
        };
        let build = build_diff_view(&options, None);
        assert!(build.sides.is_empty());
        // "not a diff..." parses as a hunk-less index → no error, no sides
        assert!(build.error.is_none());

        let options = DiffOptions {
            diff: "--- a\n+++ b\n@@ -1,2 +1,2 @@\n x".to_owned(),
            ..Default::default()
        };
        let build = build_diff_view(&options, None);
        assert_eq!(
            build.error.as_deref(),
            Some("Added line count did not match for hunk at line 3")
        );
    }

    #[test]
    fn backslash_lines_skip_in_both_views() {
        // unified: no branch matches (Diff.ts:518-573); split: skipped
        // before pairing (Diff.ts:641-642, 651-653) — the marker line
        // sits between -x and +y, so left keeps a trailing pad and
        // right a leading one
        let marker = "\\ No newline at end of file";
        let options = DiffOptions {
            diff: format!("--- a\n+++ b\n@@ -1,1 +1,1 @@\n-x\n{marker}\n+y"),
            ..Default::default()
        };
        let build = build_diff_view(&options, None);
        assert_eq!(build.sides[0].content, "x\ny");

        let options = DiffOptions {
            view: DiffView::Split,
            ..options
        };
        let build = build_diff_view(&options, None);
        assert_eq!(build.sides[0].content, "x\n");
        assert_eq!(build.sides[1].content, "\ny");
    }
}
