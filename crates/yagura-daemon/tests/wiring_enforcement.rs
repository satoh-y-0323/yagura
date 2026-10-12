#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: source scans for the contracts no behavioral test can
//! reach -- the bounded workspace-key walk in `open_impl`, and the gated tools
//! and error codes the published MCP documentation must list.
//!
//! All source scanning happens at *test run time* via `std::fs::read_to_string`
//! (never `include_str!`), rooted at `env!("CARGO_MANIFEST_DIR")`, so this file
//! compiles whether or not the files it scans exist. A file that cannot be read
//! is itself a contract violation.

use std::path::{Path, PathBuf};

use yagura_daemon::control::DestructiveTool;
use yagura_test_support::scan::count_occurrences;

/// Returns `source` with Rust comments removed: line comments (`//`, including
/// `///` and `//!`) and block comments, which nest in Rust, so an inner closing
/// marker does not end the outer comment.
///
/// String literals, raw string literals and char literals are copied through
/// untouched, so a `//` that is actually part of a string (e.g. a URL) is never
/// mistaken for a comment start, and a char literal containing a quote cannot
/// desynchronize the string-literal state that follows it.
///
/// Line numbering is preserved: every newline consumed inside a comment is
/// re-emitted, so each line of code keeps its original 1-based line number.
///
/// A lexical heuristic, not a full Rust parser.
fn strip_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < len {
        if let Some(end) = raw_string_end(&chars, i) {
            out.extend(chars[i..end].iter());
            i = end;
            continue;
        }
        if let Some(end) = char_literal_end(&chars, i) {
            out.extend(chars[i..end].iter());
            i = end;
            continue;
        }
        match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => {
                i += 2;
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                let mut depth = 1i32;
                while i < len && depth > 0 {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                        continue;
                    }
                    if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        continue;
                    }
                    if chars[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
            '"' => {
                let end = string_literal_end(&chars, i).unwrap_or(len);
                out.extend(chars[i..end].iter());
                i = end;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// If `chars[i..]` opens a raw string literal (`r"..."`, `r#".."#`, ...), returns
/// the index just past its closing delimiter; `None` for everything else,
/// including identifiers that merely start with `r`. An unterminated raw string
/// consumes to end of input rather than looping forever.
fn raw_string_end(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'r') {
        return None;
    }
    let mut j = i + 1;
    let mut hashes = 0usize;
    while chars.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    if chars.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    loop {
        match chars.get(j) {
            None => return Some(chars.len()),
            Some('"') => {
                let mut k = j + 1;
                let mut matched = 0usize;
                while matched < hashes && chars.get(k) == Some(&'#') {
                    matched += 1;
                    k += 1;
                }
                if matched == hashes {
                    return Some(k);
                }
                j += 1;
            }
            Some(_) => j += 1,
        }
    }
}

/// If `chars[i]` opens a string literal (with backslash escapes), returns the
/// index just past its closing quote; `None` if it does not. Shared with the
/// brace-matching helpers below so a brace inside a string literal, e.g. a
/// `{name}` format interpolation, is never miscounted as structural. An
/// unterminated string consumes to end of input rather than looping forever.
fn string_literal_end(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'"') {
        return None;
    }
    let mut j = i + 1;
    while j < chars.len() {
        if chars[j] == '\\' && j + 1 < chars.len() {
            j += 2;
            continue;
        }
        if chars[j] == '"' {
            return Some(j + 1);
        }
        j += 1;
    }
    Some(chars.len())
}

/// If `chars[i]` opens a char literal -- including the escaped forms (a single
/// escaped character, a unicode escape, or a 2-digit ASCII hex escape) -- returns
/// the index just past its closing `'`; `None` otherwise.
///
/// A lifetime annotation (`'a`, `'static`, `'_`) is not one: a char literal's
/// content is a single (possibly escaped) character immediately followed by a
/// closing `'`, while a lifetime is followed by an identifier with no closing `'`.
fn char_literal_end(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'\'') {
        return None;
    }
    let mut j = i + 1;
    if chars.get(j) == Some(&'\\') {
        j += 1;
        if chars.get(j) == Some(&'u') && chars.get(j + 1) == Some(&'{') {
            j += 2;
            while chars.get(j).is_some_and(|c| *c != '}') {
                j += 1;
            }
            if chars.get(j) != Some(&'}') {
                return None;
            }
            j += 1;
        } else if chars.get(j) == Some(&'x') {
            j += 1;
            for _ in 0..2 {
                if !chars.get(j).is_some_and(char::is_ascii_hexdigit) {
                    return None;
                }
                j += 1;
            }
        } else if chars.get(j).is_some() {
            j += 1;
        } else {
            return None;
        }
    } else if chars.get(j).is_some() {
        j += 1;
    } else {
        return None;
    }
    (chars.get(j) == Some(&'\'')).then_some(j + 1)
}

/// If `chars[i]` opens a raw string, string, or char literal, returns the index
/// just past it; `None` otherwise. Shared skip-ahead step for `first_open_brace`
/// and `matching_close`. `strip_comments` handles the string case in a
/// dedicated branch of its own dispatch and does not use this.
fn literal_end(chars: &[char], i: usize) -> Option<usize> {
    raw_string_end(chars, i)
        .or_else(|| string_literal_end(chars, i))
        .or_else(|| char_literal_end(chars, i))
}

/// Returns the index of the first `{` at or after `chars[from]` that is not inside
/// a string, raw-string, or char literal. `None` if no such `{` exists.
fn first_open_brace(chars: &[char], from: usize) -> Option<usize> {
    let len = chars.len();
    let mut i = from;
    while i < len {
        if let Some(end) = literal_end(chars, i) {
            i = end;
            continue;
        }
        if chars[i] == '{' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Returns the index of the `close_ch` that matches the `open_ch` at
/// `chars[open]` (already located by the caller), counting nested depth
/// while skipping literal contents. `None` if depth never returns to 0
/// before the end of `chars`. A byte-offset twin, also named
/// `matching_close_brace`, lives in `tests/common/mod.rs`; kept separate
/// because it walks a byte slice that has no `literal_end`
/// skip step of its own to share with this one.
fn matching_close(chars: &[char], open: usize, open_ch: char, close_ch: char) -> Option<usize> {
    let len = chars.len();
    let mut depth = 0i32;
    let mut i = open;
    while i < len {
        if let Some(end) = literal_end(chars, i) {
            i = end;
            continue;
        }
        if chars[i] == open_ch {
            depth += 1;
        } else if chars[i] == close_ch {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The argument text (the parenthesised part) of every call to `needle` (a
/// callee prefix ending in its opening `(`, e.g. `"warnings.push("` or
/// `"Semaphore::new("`) found in `body`.
fn call_arguments(body: &str, needle: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    let mut arguments = Vec::new();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        if chars[i..i + needle.len()] == needle[..] {
            let open = i + needle.len() - 1;
            if let Some(close) = matching_close(&chars, open, '(', ')') {
                arguments.push(chars[open + 1..close].iter().collect());
            }
            i = open + 1;
        } else {
            i += 1;
        }
    }
    arguments
}

/// Returns the 0-based line index, relative to `lines`, of the closing `}` that
/// matches the first `{` found at or after `lines[start]` -- the shared
/// brace-matching core behind `fn_body_line_range`.
/// Character-level rather than byte-offset based, so it is unaffected by CRLF vs
/// LF line endings, and braces inside literals are skipped. `None` if no `{` is
/// found, or depth never returns to 0, within `lines[start..]`.
fn brace_matched_end_line(lines: &[&str], start: usize) -> Option<usize> {
    let text: String = lines[start..].join("\n");
    let chars: Vec<char> = text.chars().collect();
    let open = first_open_brace(&chars, 0)?;
    let close = matching_close(&chars, open, '{', '}')?;
    let newline_count = chars[..=close].iter().filter(|c| **c == '\n').count();
    Some(start + newline_count)
}

/// Returns the (0-based, inclusive) line range of the body of the first function
/// named `fn_name` in `source`, by brace-matching from the line containing
/// `fn {fn_name}(` or `fn {fn_name}<` -- a generic function puts its type
/// parameter list between the name and the `(`, so both shapes name the same
/// function, and either one still requires an exact break after the name (a
/// same-prefixed name never matches). Line-based, so unaffected by CRLF vs LF
/// line endings.
fn fn_body_line_range(source: &str, fn_name: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    let paren_needle = format!("fn {fn_name}(");
    let angle_needle = format!("fn {fn_name}<");
    let start = lines
        .iter()
        .position(|l| l.contains(paren_needle.as_str()) || l.contains(angle_needle.as_str()))?;
    let end = brace_matched_end_line(&lines, start)?;
    Some((start, end))
}

/// Extracts the full text (signature line through closing brace) of the
/// first function named `fn_name` with a `{ .. }` body. Returns `None` for
/// trait method *declarations* that end in `;` with no body.
fn extract_fn_body_text(source: &str, fn_name: &str) -> Option<String> {
    let (start, end) = fn_body_line_range(source, fn_name)?;
    let lines: Vec<&str> = source.lines().collect();
    Some(lines[start..=end].join("\n"))
}

// Bidirectional detection-capability proof: each pure helper above that does more
// than a plain substring check is exercised against both a violating and a
// non-violating synthetic input.

#[test]
fn char_literal_end_detects_char_literals_but_not_lifetimes() {
    // A char literal containing a quote, or an escape sequence, must be recognized
    // as a whole, closed span.
    let quote_literal: Vec<char> = "'\"'".chars().collect();
    assert_eq!(
        char_literal_end(&quote_literal, 0),
        Some(3),
        "a char literal containing `\"` must be recognized as a whole, 3-char span"
    );
    let escaped_newline: Vec<char> = "'\\n'".chars().collect();
    assert_eq!(
        char_literal_end(&escaped_newline, 0),
        Some(4),
        "an escaped char literal (`'\\n'`) must be recognized as a whole, 4-char span"
    );

    // ...while a lifetime annotation, which has no closing `'` right after its
    // identifier, must not be mistaken for one.
    let lifetime: Vec<char> = "'a".chars().collect();
    assert_eq!(
        char_literal_end(&lifetime, 0),
        None,
        "a lifetime annotation must not be mistaken for a char literal"
    );
    let static_lifetime: Vec<char> = "'static".chars().collect();
    assert_eq!(
        char_literal_end(&static_lifetime, 0),
        None,
        "the `'static` lifetime must not be mistaken for a char literal"
    );
}

#[test]
fn char_literal_end_recognizes_the_ascii_hex_escape() {
    // A 2-digit ASCII hex escape must be recognized as a whole char literal, just
    // like the unicode escape already was.
    let hex_escape: Vec<char> = "'\\x41'".chars().collect();
    assert_eq!(
        char_literal_end(&hex_escape, 0),
        Some(6),
        "a hex-escaped char literal (`'\\x41'`) must be recognized as a whole, 6-char span"
    );

    // A malformed escape (fewer than 2 hex digits after `x`) must not be
    // mistaken for a closed literal.
    let short_escape: Vec<char> = "'\\x4'".chars().collect();
    assert_eq!(
        char_literal_end(&short_escape, 0),
        None,
        "a `\\x` escape with fewer than 2 hex digits must not be recognized as a char literal"
    );
}

#[test]
fn fn_body_line_range_ignores_braces_inside_string_literals() {
    // A string literal containing an unbalanced brace (e.g. a `{name}` format
    // interpolation, or an incidental brace in prose) must not be miscounted as a
    // structural brace that shifts the boundary into a following function.
    let source = "fn caller() {\n    let s = \"unbalanced { brace\";\n    let x = 1;\n}\n\nfn next_fn() {\n    let y = 2;\n}";
    let (start, end) = fn_body_line_range(source, "caller").expect("body must be found");
    let lines: Vec<&str> = source.lines().collect();
    assert_eq!(
        start, 0,
        "caller()'s body must start at its own signature line"
    );
    assert_eq!(
        end,
        3,
        "the extra `{{` inside the string literal on line 1 must not be counted, so \
         caller()'s body must still end at its own closing `}}` (0-based line 3), not bleed \
         into next_fn(); found end line index {end} ({:?})",
        lines.get(end)
    );
    assert!(
        !lines[start..=end].join("\n").contains("next_fn"),
        "the extracted body must not include the following, unrelated function"
    );
}

#[test]
fn strip_comments_removes_line_comments_but_preserves_preceding_code() {
    let source = "let x = 1; // tracing::warn!(\"fake\")\n";
    let stripped = strip_comments(source);
    assert!(
        !stripped.contains("tracing::warn!"),
        "a `//` line comment's content must be removed"
    );
    assert!(
        stripped.contains("let x = 1;"),
        "code preceding the comment on the same line must be preserved"
    );
}

#[test]
fn strip_comments_removes_block_comments_but_preserves_surrounding_code() {
    let source = "let x = 1; /* tracing::warn!(\"fake\") */ let y = 2;";
    let stripped = strip_comments(source);
    assert!(
        !stripped.contains("tracing::warn!"),
        "a `/* .. */` block comment's content must be removed"
    );
    assert!(
        stripped.contains("let x = 1;") && stripped.contains("let y = 2;"),
        "code before and after a block comment must be preserved"
    );
}

#[test]
fn strip_comments_removes_nested_block_comments_as_a_single_unit() {
    // Rust block comments nest, so the first closing marker -- the one that ends
    // the inner comment -- must not be mistaken for the end of the whole comment.
    let source = "let x = 1; /* outer /* inner */ still comment */ let y = 2;";
    let stripped = strip_comments(source);
    assert!(
        !stripped.contains("still comment"),
        "text after the inner `*/` but still inside the outer comment must be removed, \
         found: {stripped:?}"
    );
    assert!(
        stripped.contains("let x = 1;") && stripped.contains("let y = 2;"),
        "code before and after the whole nested comment must be preserved, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_does_not_over_consume_a_non_nested_block_comment() {
    // Guards against the nesting depth counter over-consuming the ordinary,
    // non-nested case.
    let source = "let x = 1; /* just one comment */ let y = 2;";
    let stripped = strip_comments(source);
    assert!(
        stripped.contains("let x = 1;") && stripped.contains("let y = 2;"),
        "a plain, non-nested block comment must still be removed without disturbing \
         surrounding code, found: {stripped:?}"
    );
    assert!(
        !stripped.contains("just one comment"),
        "the comment's own content must be removed, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_preserves_double_slash_inside_string_literals() {
    // A `//` that is actually part of a string literal (a URL, or this codebase's
    // own event names) must survive stripping, and the real code after it on the
    // same line must not be dropped as if the rest of the line were a comment.
    let source = "let u = \"http://example.com\"; let real_call = tracing::warn!(\"x\");";
    let stripped = strip_comments(source);
    assert!(
        stripped.contains("\"http://example.com\""),
        "a `//` inside a string literal must not be treated as a comment start; the string \
         literal must survive stripping intact, found: {stripped:?}"
    );
    assert!(
        stripped.contains("tracing::warn!"),
        "code following a `//` that is actually inside a string literal must not be \
         incorrectly dropped as if the rest of the line were a comment, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_preserves_raw_string_literals_with_backslashes() {
    // Mirrors the production pattern in `crates/yagura-daemon/src/lib.rs`: a raw
    // string's backslashes must not be misread as escape sequences that could
    // desynchronize the scanner.
    let source = r#"let p = r"\\?\UNC\"; // strip me
"#;
    let stripped = strip_comments(source);
    assert!(
        stripped.contains(r#"r"\\?\UNC\""#),
        "a raw string literal must survive stripping intact even though it contains \
         backslashes, found: {stripped:?}"
    );
    assert!(
        !stripped.contains("strip me"),
        "the trailing line comment must still be removed, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_recognizes_char_literals_containing_a_double_quote() {
    // Without char-literal recognition, the quote inside such a literal would be
    // mistaken for the start of a string literal, silently absorbing the rest of
    // the source into what the scanner believes is still inside a string.
    let source = "let c = '\"'; let real_call = tracing::warn!(\"x\"); // strip me\n";
    let stripped = strip_comments(source);
    assert!(
        stripped.contains("tracing::warn!"),
        "code after a char literal containing `\"` must not be swallowed as if still inside a \
         string, found: {stripped:?}"
    );
    assert!(
        !stripped.contains("strip me"),
        "a real trailing line comment must still be stripped once the char literal is \
         correctly recognized, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_does_not_mistake_a_lifetime_annotation_for_a_char_literal() {
    // A `'` followed by an identifier with no closing `'` right after it is a
    // lifetime, not a char literal, and must survive stripping untouched.
    let source = "fn f<'a>(x: &'a str) -> &'static str { x } // strip me\n";
    let stripped = strip_comments(source);
    assert!(
        stripped.contains("fn f<'a>(x: &'a str) -> &'static str"),
        "lifetime annotations must survive stripping untouched, found: {stripped:?}"
    );
    assert!(
        !stripped.contains("strip me"),
        "the trailing line comment must still be stripped, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_preserves_byte_char_literals_containing_brace_like_bytes() {
    // Byte char literals must not desync the string/comment state: the `b` prefix
    // is copied through as an ordinary character, then the literal that follows is
    // recognized as its own closed char literal span.
    let source = "let x = b'['; let y = b']'; let real = endpoint_security(); // strip me\n";
    let stripped = strip_comments(source);
    assert!(
        stripped.contains("endpoint_security()"),
        "code following byte char literals containing brace-like bytes must not be swallowed, \
         found: {stripped:?}"
    );
    assert!(
        !stripped.contains("strip me"),
        "a real trailing line comment must still be stripped, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_does_not_let_block_comment_markers_inside_a_string_literal_affect_nesting_depth()
{
    // A string literal containing text that merely looks like block-comment markers
    // must not perturb the nested block-comment depth counter: `string_literal_end`
    // consumes the whole literal before that content is visible to comment matching.
    let source = "let s = \"/* not a comment */\"; let real = endpoint_security(); // strip me\n";
    let stripped = strip_comments(source);
    assert!(
        stripped.contains("\"/* not a comment */\""),
        "a string literal containing block-comment-like markers must survive stripping intact, \
         found: {stripped:?}"
    );
    assert!(
        stripped.contains("endpoint_security()"),
        "code following the string literal must not be swallowed, found: {stripped:?}"
    );
    assert!(
        !stripped.contains("strip me"),
        "a real trailing line comment must still be stripped, found: {stripped:?}"
    );
}

#[test]
fn strip_comments_preserves_line_numbers_across_a_multiline_block_comment() {
    let source = "line1\n/* a\nmultiline\ncomment */\nline5";
    let stripped = strip_comments(source);
    let lines: Vec<&str> = stripped.lines().collect();
    assert_eq!(
        lines.len(),
        5,
        "line count must be unchanged by comment removal, found: {lines:?}"
    );
    assert_eq!(lines[0], "line1");
    assert_eq!(
        lines[4], "line5",
        "code after a multiline block comment must keep its original line number"
    );
}

// File-system helpers (I/O), rooted at CARGO_MANIFEST_DIR (this crate).

/// Anchors a relative path at *this* crate's manifest directory, which is the
/// only place a caller can supply it from.
fn resolve(rel_from_manifest: &str) -> PathBuf {
    yagura_test_support::scan::resolve_from(env!("CARGO_MANIFEST_DIR"), rel_from_manifest)
}

/// Reads a source file relative to this crate's manifest directory.
/// A read failure is itself a contract violation (the SSOT requires the
/// scanned files to exist), so this panics with a distinguishable message
/// rather than returning a `Result`.
fn read_source(rel_from_manifest: &str) -> String {
    let path = resolve(rel_from_manifest);
    let text = read_text(&path).unwrap_or_else(|e| {
        panic!(
            "contract violation: cannot read {} required by the OS boundary contract: {e}",
            path.display()
        )
    });
    strip_comments(&text)
}

/// Reads `path` with its line endings normalized to `\n`: a scan compares
/// texts rebuilt from `lines()` against what it read, so a CRLF checkout must
/// not read differently from an LF one.
fn read_text(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path).map(|text| text.replace("\r\n", "\n"))
}

// A window request that will be rejected by `enforce_limits_before_open`
// (the daemon already at its concurrent-window cap) must not first pay for
// resolving the request's chat room key: `open_impl` should run the limit
// check before the blocking key resolution, not after it.
//
// Machine-scan disclosure (three points): (a) the scanned object is
// `src/window.rs`'s `strip_comments`-ed full text, cut to the single
// `open_impl` function body `extract_fn_body_text` returns; what is
// compared is the position of the first `enforce_limits_before_open(` call
// against the first `resolve_key_blocking(` call within that body. (b) no
// version ordering applies -- there is only one `open_impl` body. (c) what
// this scan measures is source-level call order, not that a rejected
// request actually skips the key resolution at runtime -- no integration
// test drives the daemon to its window cap and observes the resolver being
// skipped.
#[test]
fn open_impl_checks_the_open_limit_before_resolving_the_chat_room_key() {
    let source = read_source("src/window.rs");
    let body = extract_fn_body_text(&source, "open_impl")
        .unwrap_or_else(|| panic!("src/window.rs must define an open_impl function body"));
    let enforce_at = body
        .find("enforce_limits_before_open(")
        .unwrap_or_else(|| panic!("open_impl must call enforce_limits_before_open: {body:?}"));
    let resolve_at = body
        .find("resolve_key_blocking(")
        .unwrap_or_else(|| panic!("open_impl must call resolve_key_blocking: {body:?}"));
    assert!(
        enforce_at < resolve_at,
        "open_impl must check the open limit before resolving the chat room key, so a request \
         rejected for being over the limit never pays for the resolution: \
         enforce_limits_before_open at offset {enforce_at}, resolve_key_blocking at offset \
         {resolve_at}"
    );
}

// Non-Rust text helpers (Markdown / TOML) used by the documentation checks below. Kept separate
// from `read_source` / `strip_comments`, which understand only Rust lexical
// syntax.

/// Reads a non-Rust text file (Markdown / TOML) relative to this crate's
/// manifest directory verbatim -- `strip_comments` understands only Rust
/// lexical syntax and would be wrong to apply here (a TOML `#` comment is
/// exactly where the playbook check below expects to find a tool name). A
/// read failure is itself a contract violation, mirroring `read_source`.
fn read_doc_verbatim(rel_from_manifest: &str) -> String {
    let path = resolve(rel_from_manifest);
    read_text(&path).unwrap_or_else(|e| {
        panic!(
            "contract violation: cannot read {} required by the docs/distribution contract: {e}",
            path.display()
        )
    })
}

/// Returns the text of the Markdown section that starts at the line exactly
/// equal to `heading_line` (e.g. `"## Error Handling"`), up to but excluding
/// the next line that starts a new `"## "` (level-2) heading, or the end of
/// the document if there is no such line. `None` if `heading_line` is not
/// found as a whole line.
///
/// Scoped to `"## "` specifically, so a `"### <tool>"` subsection heading
/// inside the section does not end it early.
fn markdown_section_text(source: &str, heading_line: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let start = lines.iter().position(|l| *l == heading_line)?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |offset| start + 1 + offset);
    Some(lines[start..end].join("\n"))
}

/// Returns the text starting at the first line in `source` that begins with
/// `marker` (e.g. `"**Delivery**:"` -- a bold in-paragraph label, not a
/// Markdown heading), up to but excluding the next line that starts any
/// heading (`"#"` at any level), or the end of the document if there is no
/// such line. `None` if no line starts with `marker`.
///
/// A label like `"**Delivery**:"` marks a sub-topic within a `"### <tool>"`
/// subsection rather than a heading of its own, so this bounds the block by
/// the next real heading rather than by another bold label -- an unrelated
/// bold phrase elsewhere in the same paragraph (e.g. `` **resend_max** ``)
/// must not end the block early.
fn markdown_block_from_marker(source: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let start = lines.iter().position(|l| l.starts_with(marker))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with('#'))
        .map_or(lines.len(), |offset| start + 1 + offset);
    Some(lines[start..end].join("\n"))
}

/// Returns the first sentence (up to and including the first `". "`, or the
/// whole line if there is none) of the first non-empty line after `section`'s
/// own heading line -- the section's opening claim. `None` if `section` is
/// empty, or no non-empty line follows its heading line.
fn first_sentence_after_heading(section: &str) -> Option<&str> {
    let mut lines = section.lines();
    lines.next()?; // the heading line itself
    let body_line = lines.find(|l| !l.trim().is_empty())?;
    Some(match body_line.find(". ") {
        Some(period_index) => &body_line[..=period_index],
        None => body_line,
    })
}

/// Extracts the string literal from every `=> "..."` match arm in `body`, in
/// source order, using a minimal quote-pair scan deliberately narrower than
/// `string_literal_end`: every arm this function is applied to is a plain
/// lower_snake_case error code with no escape sequences, so a byte-offset
/// quote pair is sufficient. A line without `=>` (the match's own signature
/// line, or a closing brace) contributes nothing.
fn extract_match_arm_string_literals(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        if !line.contains("=>") {
            continue;
        }
        let Some(start) = line.find('"') else {
            continue;
        };
        let Some(len) = line[start + 1..].find('"') else {
            continue;
        };
        out.push(line[start + 1..start + 1 + len].to_string());
    }
    out
}

#[test]
fn markdown_section_text_stops_at_the_next_level_2_heading_but_not_a_level_3_one() {
    let doc =
        "# Title\n\n## Section A\nline a1\n### A subsection\nline a2\n\n## Section B\nline b1\n";
    let section = markdown_section_text(doc, "## Section A").expect("section must be found");
    assert!(
        section.contains("### A subsection") && section.contains("line a2"),
        "a level-3 subsection heading inside the section must not end it early, found: \
         {section:?}"
    );
    assert!(
        !section.contains("Section B") && !section.contains("line b1"),
        "the next level-2 heading must end the section, found: {section:?}"
    );
}

#[test]
fn markdown_block_from_marker_stops_at_the_next_heading_of_any_level() {
    let doc = "### send_handoff\nintro line\n**Delivery**: outcome a, outcome b.\nmore delivery prose\n\n### open_window\nunrelated line\n";
    let block = markdown_block_from_marker(doc, "**Delivery**:").expect("block must be found");
    assert!(
        block.contains("outcome a, outcome b") && block.contains("more delivery prose"),
        "the block must include everything from the marker line up to the next heading, \
         found: {block:?}"
    );
    assert!(
        !block.contains("open_window") && !block.contains("unrelated line"),
        "the next heading of any level must end the block, found: {block:?}"
    );
}

#[test]
fn markdown_block_from_marker_runs_to_end_of_document_when_no_further_heading_exists() {
    let doc = "**Delivery**: only paragraph, no heading follows.\nsecond line\n";
    let block = markdown_block_from_marker(doc, "**Delivery**:").expect("block must be found");
    assert!(
        block.contains("second line"),
        "with no following heading, the block must run to the end of the document, \
         found: {block:?}"
    );
}

#[test]
fn markdown_section_text_runs_to_end_of_document_when_no_further_heading_exists() {
    let doc = "## Only Section\nlast line\n";
    let section = markdown_section_text(doc, "## Only Section").expect("section must be found");
    assert!(
        section.contains("last line"),
        "with no following heading, the section must run to the end of the document, \
         found: {section:?}"
    );
}

#[test]
fn first_sentence_after_heading_stops_at_the_first_sentence_break() {
    let section = "## Heading\n\nFirst sentence here. Second sentence here.\n";
    assert_eq!(
        first_sentence_after_heading(section),
        Some("First sentence here."),
        "must return only the first sentence, not the whole opening paragraph"
    );
}

#[test]
fn first_sentence_after_heading_returns_the_whole_line_when_it_has_no_sentence_break() {
    let section = "## Heading\nonly one clause with no period\n";
    assert_eq!(
        first_sentence_after_heading(section),
        Some("only one clause with no period"),
        "with no `\". \"` in the opening line, the whole line must be returned"
    );
}

#[test]
fn extract_match_arm_string_literals_ignores_lines_without_a_match_arm() {
    let body = "fn code(&self) -> &'static str {\n    match self {\n        Self::A(_) => \"a_code\",\n        Self::B(_) => \"b_code\",\n    }\n}";
    assert_eq!(
        extract_match_arm_string_literals(body),
        vec!["a_code".to_string(), "b_code".to_string()],
        "must extract exactly the two arm literals, in source order, and ignore the \
         signature and closing-brace lines"
    );
}

// Every gated tool in `DestructiveTool::ALL` is documented in
// docs/mcp-setup.md (a "### <tool>" heading, and a mention in the "## Approval
// Flow" section's opening sentence) and in playbook.toml's comments, and every
// error code yielded by yagura-mcp's `tools.rs::code()` appears within
// docs/mcp-setup.md's "## Error Handling" section. Both checks read the
// distributed artifacts themselves (never a copy), so a tool added to
// `DestructiveTool::ALL`, or an error code added to `ErrorCode`, that is not
// also documented is caught here rather than only in a human doc review.

#[test]
fn destructive_tools_are_documented_in_docs_and_playbook() {
    let docs = read_doc_verbatim("../../docs/mcp-setup.md");
    let playbook = read_doc_verbatim("../../playbook.toml");

    let approval_flow_section = markdown_section_text(&docs, "## Approval Flow")
        .unwrap_or_else(|| panic!("docs/mcp-setup.md must have an \"## Approval Flow\" section"));
    let approval_flow_opening = first_sentence_after_heading(&approval_flow_section)
        .unwrap_or_else(|| {
            panic!("the \"## Approval Flow\" section must have a non-empty opening paragraph")
        });

    let playbook_comments: String = playbook
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let mut violations = Vec::new();
    for tool in DestructiveTool::ALL {
        let name = tool.as_tool_name();
        let heading = format!("### {name}");
        if !docs.lines().any(|l| l == heading) {
            violations.push(format!(
                "docs/mcp-setup.md is missing a \"{heading}\" heading"
            ));
        }
        if !approval_flow_opening.contains(name) {
            violations.push(format!(
                "the \"## Approval Flow\" section's opening sentence does not mention {name}: \
                 {approval_flow_opening:?}"
            ));
        }
        if !playbook_comments.contains(name) {
            violations.push(format!("playbook.toml's comments do not mention {name}"));
        }
    }
    assert!(
        violations.is_empty(),
        "every gated tool in DestructiveTool::ALL must have a \"### <tool>\" heading and an \
         \"## Approval Flow\" opening-sentence mention in docs/mcp-setup.md, and a mention in \
         playbook.toml's comments; violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn tools_rs_error_codes_are_documented_in_error_handling_section() {
    let tools_rs = read_source("../yagura-mcp/src/tools.rs");
    let code_fn_body = extract_fn_body_text(&tools_rs, "code")
        .unwrap_or_else(|| panic!("yagura-mcp/src/tools.rs must define a `fn code(` method"));
    let codes = extract_match_arm_string_literals(&code_fn_body);
    assert!(
        !codes.is_empty(),
        "fn code()'s match arms must yield at least one error code string"
    );

    let docs = read_doc_verbatim("../../docs/mcp-setup.md");
    let error_handling_section = markdown_section_text(&docs, "## Error Handling")
        .unwrap_or_else(|| panic!("docs/mcp-setup.md must have an \"## Error Handling\" section"));

    let missing: Vec<&String> = codes
        .iter()
        .filter(|code| !error_handling_section.contains(code.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "every error code returned by yagura-mcp/src/tools.rs's `fn code()` must appear within \
         docs/mcp-setup.md's \"## Error Handling\" section; missing: {missing:?}"
    );
}

// docs/mcp-setup.md's send_handoff section documents its four delivery
// outcomes (confirmed / unconfirmed / not_observed / not_written) directly
// under the "**Delivery**:" label that introduces them, so a value added to,
// or dropped from, that outcome set without updating the documented list is
// caught here, the same discipline already applied to gated tools and error
// codes.

#[test]
fn send_handoff_delivery_outcomes_are_documented() {
    let docs = read_doc_verbatim("../../docs/mcp-setup.md");
    let delivery_block = markdown_block_from_marker(&docs, "**Delivery**:").unwrap_or_else(|| {
        panic!(
            "docs/mcp-setup.md's send_handoff section must have a \"**Delivery**:\" label \
             introducing the delivery outcomes"
        )
    });

    let missing: Vec<&str> = ["confirmed", "unconfirmed", "not_observed", "not_written"]
        .into_iter()
        .filter(|value| !delivery_block.contains(value))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/mcp-setup.md's \"**Delivery**:\" block must document all four delivery outcomes \
         (confirmed / unconfirmed / not_observed / not_written); missing: {missing:?}"
    );
}

// The workspace walk `open_impl` runs outside `open_lock` has no cap of its
// own -- the early rejection only counts windows
// already registered, so a caller with no intent to succeed can still queue
// an unbounded number of concurrent filesystem walks against the shared
// `spawn_blocking` pool. `WindowRegistry` must hold a `tokio::sync::Semaphore`
// sized to `max_windows`, and `open_impl` must acquire one of its permits
// after the early rejection and before resolving the workspace key.
//
// Machine-scan disclosure (three points): (a) the scanned objects are, cut
// from `src/window.rs`'s `strip_comments`-ed text, the single `open_impl`
// function body and the single `WindowRegistry::new` function body, both
// via `extract_fn_body_text`; what is counted inside `open_impl` is the
// occurrence count and position of the literal `workspace_walks.acquire(`
// relative to the first `enforce_limits_before_open(` and the first
// `resolve_key_blocking(`, and inside `new` the occurrence count of
// `Semaphore::new(` and whether its argument text contains `max_windows`.
// (b) no version ordering applies -- there is one `open_impl` body and, of
// the two functions named `new` in the file, `extract_fn_body_text` returns
// the first, which is `WindowRegistry::new`; the second, later in the file,
// belongs to an unrelated test-only type. (c) what this scan measures is
// source-level presence, count and position, not that the permit is
// actually held while the walk runs, or released afterward, at runtime --
// no integration test drives `max_windows` concurrent opens and observes
// one more stall on the permit.
#[test]
fn open_impl_acquires_a_workspace_walk_permit_sized_to_max_windows_before_resolving_the_key() {
    let source = read_source("src/window.rs");

    let new_body = extract_fn_body_text(&source, "new")
        .unwrap_or_else(|| panic!("src/window.rs must define a WindowRegistry::new function body"));
    let semaphore_args = call_arguments(&new_body, "Semaphore::new(");
    assert_eq!(
        semaphore_args.len(),
        1,
        "WindowRegistry::new must build its workspace-walk permit exactly once: {new_body:?}"
    );
    assert!(
        semaphore_args[0].contains("max_windows"),
        "the workspace-walk semaphore must be sized to max_windows, got argument {:?}: \
         {new_body:?}",
        semaphore_args[0]
    );

    let body = extract_fn_body_text(&source, "open_impl")
        .unwrap_or_else(|| panic!("src/window.rs must define an open_impl function body"));
    assert_eq!(
        count_occurrences(&body, "workspace_walks.acquire("),
        1,
        "open_impl must acquire a workspace-walk permit exactly once: {body:?}"
    );

    let enforce_at = body
        .find("enforce_limits_before_open(")
        .unwrap_or_else(|| panic!("open_impl must call enforce_limits_before_open: {body:?}"));
    let acquire_at = body
        .find("workspace_walks.acquire(")
        .unwrap_or_else(|| panic!("open_impl must acquire a workspace-walk permit: {body:?}"));
    let resolve_at = body
        .find("resolve_key_blocking(")
        .unwrap_or_else(|| panic!("open_impl must call resolve_key_blocking: {body:?}"));
    assert!(
        enforce_at < acquire_at && acquire_at < resolve_at,
        "open_impl must acquire the workspace-walk permit after the early rejection and before \
         resolving the workspace key: enforce_limits_before_open at offset {enforce_at}, \
         workspace_walks.acquire( at offset {acquire_at}, resolve_key_blocking at offset \
         {resolve_at}"
    );
}
