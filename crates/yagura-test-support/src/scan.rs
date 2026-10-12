//! Source-tree scanning helpers used by the repository guard tests.

use std::path::PathBuf;

/// Anchors `relative` at `manifest_dir`, the directory a caller obtains from
/// `env!("CARGO_MANIFEST_DIR")`.
///
/// The anchor is a parameter because a scan lives in the crate it guards and
/// must resolve its corpus against *that* crate's manifest.
#[must_use]
pub fn resolve_from(manifest_dir: &str, relative: &str) -> PathBuf {
    PathBuf::from(manifest_dir).join(relative)
}

/// Counts the non-overlapping occurrences of `needle` in `haystack`.
#[must_use]
pub fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// `true` when `needle` occurs as a contiguous byte subsequence of
/// `haystack`. An empty needle is always contained; a needle longer than the
/// haystack never is.
#[must_use]
pub fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Finds the byte offset, in `text`, of the first occurrence of `marker`
/// whose immediately preceding bytes do not end with `draft_prefix`,
/// skipping any earlier occurrence that is glued to it. `None` if every
/// occurrence found is such a draft (including when `marker` does not occur
/// at all).
///
/// A composer-draw-mode fixture paints the same marker, unmodified,
/// immediately after `draft_prefix`, while the line it belongs to is still
/// an unsubmitted draft sitting in the composer, and repaints that draft any
/// number of times before (and, in principle, after) the accepted
/// occurrence lands. Whether an occurrence is a draft is therefore a
/// property of what precedes it, not of its position -- first or last --
/// among all occurrences in `text`, so this walk checks the former directly
/// rather than assuming an order.
#[must_use]
pub fn find_accepted_marker(text: &str, marker: &str, draft_prefix: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(offset) = text.get(search_from..)?.find(marker) {
        let start = search_from + offset;
        if !text.get(..start)?.ends_with(draft_prefix) {
            return Some(start);
        }
        search_from = start + marker.len();
    }
    None
}

/// The JSON body of the *accepted* `[yagura handoff] ` line in `scrollback`,
/// taken from the occurrence [`find_accepted_marker`] locates rather than
/// the first or last occurrence in the text -- either of which a
/// composer-draw-mode draft repaint of the same envelope can be mistaken
/// for. `draft_prefix` is the caller's composer marker plus separator (see
/// [`crate::process::COMPOSER_MARKER`] and
/// [`crate::process::COMPOSER_SEPARATOR`]).
///
/// # Panics
///
/// Panics if `scrollback` contains no accepted handoff marker, or if the
/// bytes between the marker and the line's end (a CR, or the end of the
/// text) do not parse as JSON.
#[must_use]
pub fn extract_accepted_handoff_envelope_json(
    scrollback: &[u8],
    draft_prefix: &str,
) -> serde_json::Value {
    const MARKER: &str = "[yagura handoff] ";
    let text = String::from_utf8_lossy(scrollback).into_owned();
    let start = find_accepted_marker(&text, MARKER, draft_prefix).unwrap_or_else(|| {
        panic!("scrollback must contain an accepted handoff envelope line: {text:?}")
    });
    let after = &text[start + MARKER.len()..];
    let end = after.find('\r').unwrap_or(after.len());
    let json_part = after[..end].trim_end_matches(|c: char| c != '}');
    serde_json::from_str(json_part).unwrap_or_else(|err| {
        panic!("handoff envelope JSON part must parse: {err}, text: {json_part:?}")
    })
}
