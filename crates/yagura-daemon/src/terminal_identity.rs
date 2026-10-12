//! What a window's child is told about the terminal it runs in.
//!
//! A window's terminal is yagura's own, not the one the daemon was started
//! from. A CLI picks how it handles keys, the mouse and the clipboard from the
//! variables its terminal sets, so the launching terminal's own variables are
//! taken away and [`DECLARED_TERMINAL`] describes what yagura draws.

use std::ffi::{OsStr, OsString};

/// The variables every window's child is given, after the launching
/// terminal's own are taken away and before a profile's `[env]`, which still
/// wins.
pub const DECLARED_TERMINAL: [(&str, &str); 2] =
    [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// Variables a terminal or a multiplexer sets to name itself: the union of
/// what Codex reads to tell terminals apart and what Claude Code takes away
/// from its own children.
const LAUNCHER_NAMES: [&str; 22] = [
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "__CFBundleIdentifier",
    "COLORFGBG",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "VTE_VERSION",
    "TERMINAL_EMULATOR",
    "ZED_TERM",
    "TILIX_ID",
    "TERMINATOR_UUID",
    "XTERM_VERSION",
    "XTERM_SHELL",
    "XTERM_LOCALE",
    "WINDOWID",
    "CURSOR_TRACE_ID",
    "TMUX",
    "TMUX_PANE",
    "STY",
];

/// Prefixes of the variable families the same terminals set.
const LAUNCHER_PREFIXES: [&str; 10] = [
    "VSCODE_",
    "ITERM_",
    "KITTY_",
    "WEZTERM_",
    "GHOSTTY_",
    "ALACRITTY_",
    "KONSOLE_",
    "GNOME_TERMINAL_",
    "ZELLIJ",
    "CONEMU",
];

/// VS Code points `GIT_ASKPASS` at a script that only works with VS Code's
/// own variables beside it, so the two go together.
const VS_CODE_ASKPASS_PREFIX: &str = "VSCODE_GIT_ASKPASS_";
const ASKPASS: &str = "GIT_ASKPASS";

/// Which of `names` (an environment's variable names) belong to the terminal
/// the daemon was started from, and so are taken away from a window's child.
/// Names compare without regard to ASCII case, as Windows compares them.
#[must_use]
pub fn launcher_terminal_names<'a>(names: impl IntoIterator<Item = &'a OsStr>) -> Vec<OsString> {
    let names: Vec<&OsStr> = names.into_iter().collect();
    let has_vs_code_askpass = names
        .iter()
        .any(|name| starts_with_ignoring_case(name, VS_CODE_ASKPASS_PREFIX));
    names
        .into_iter()
        .filter(|name| {
            is_launcher_name(name) || (has_vs_code_askpass && name.eq_ignore_ascii_case(ASKPASS))
        })
        .map(OsStr::to_os_string)
        .collect()
}

fn is_launcher_name(name: &OsStr) -> bool {
    LAUNCHER_NAMES
        .iter()
        .any(|launcher| name.eq_ignore_ascii_case(launcher))
        || LAUNCHER_PREFIXES
            .iter()
            .any(|prefix| starts_with_ignoring_case(name, prefix))
}

fn starts_with_ignoring_case(name: &OsStr, prefix: &str) -> bool {
    name.as_encoded_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripped(names: &[&str]) -> Vec<OsString> {
        launcher_terminal_names(names.iter().map(OsStr::new))
    }

    #[test]
    fn takes_names_and_families_in_any_case_and_leaves_the_rest() {
        assert_eq!(
            stripped(&[
                "Path",
                "term_program",
                "ConEmuANSI",
                "ZELLIJ",
                "KITTY_PID",
                "TERM",
                "HOME"
            ]),
            vec![
                OsString::from("term_program"),
                OsString::from("ConEmuANSI"),
                OsString::from("ZELLIJ"),
                OsString::from("KITTY_PID"),
            ]
        );
    }

    #[test]
    fn takes_git_askpass_only_beside_vs_codes_askpass_variables() {
        assert_eq!(
            stripped(&["GIT_ASKPASS", "VSCODE_GIT_ASKPASS_MAIN"]),
            vec![
                OsString::from("GIT_ASKPASS"),
                OsString::from("VSCODE_GIT_ASKPASS_MAIN")
            ]
        );
        assert!(stripped(&["GIT_ASKPASS"]).is_empty());
    }
}
