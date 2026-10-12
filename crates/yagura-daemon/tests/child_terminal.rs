#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a window's child is told it runs in yagura's terminal, not in the
//! terminal yagura was started from.
//!
//! A CLI decides how it handles keys, the mouse and the clipboard from the variables its
//! terminal sets. A window's terminal is yagura's own, so the variables naming the
//! launching terminal must not reach the child, and `TERM` / `COLORTERM` describe what
//! yagura draws. A profile's `[env]` still has the last word.
//!
//! Every property is measured through a real daemon process
//! ([`common::real_daemon`]) and a real child (`env_probe`).

mod common;

use common::real_daemon::Sandbox;

/// A terminal launch from VS Code's integrated terminal running inside kitty.
const LAUNCHER_ENV: &[(&str, &str)] = &[
    ("TERM", "xterm-kitty"),
    ("COLORTERM", "launcher-colorterm"),
    ("TERM_PROGRAM", "vscode"),
    ("TERM_PROGRAM_VERSION", "1.105.0"),
    ("VSCODE_INJECTION", "1"),
    ("VSCODE_GIT_IPC_HANDLE", "launcher-ipc-handle"),
    ("VSCODE_GIT_ASKPASS_MAIN", "launcher-askpass-main"),
    ("GIT_ASKPASS", "launcher-askpass"),
    ("WT_SESSION", "launcher-wt-session"),
    ("KITTY_WINDOW_ID", "7"),
    ("ConEmuANSI", "ON"),
    ("TMUX", "launcher-tmux"),
];

/// Opens one `env_probe` window per name under the launch in `launcher_env` and returns
/// what each child reports, in order.
async fn probe_after_launch(
    tag: &str,
    launcher_env: &[(&str, &str)],
    names: &[&str],
    env_table: &str,
) -> Vec<Option<String>> {
    let sandbox = Sandbox::new(tag);
    for (index, name) in names.iter().enumerate() {
        sandbox.write_var_probe_profile(&format!("probe{index}"), name, env_table);
    }
    let mut daemon = sandbox.start_from(launcher_env).await;
    let mut reports = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        reports.push(
            daemon
                .probed_var(&sandbox, &format!("probe{index}"), name)
                .await,
        );
    }
    reports
}

#[tokio::test]
async fn a_window_child_is_told_the_terminal_is_yaguras_not_the_launchers() {
    let names = ["TERM", "COLORTERM"];
    let reports = probe_after_launch("child-terminal-declared", LAUNCHER_ENV, &names, "").await;
    assert_eq!(
        reports,
        vec![
            Some("xterm-256color".to_string()),
            Some("truecolor".to_string())
        ],
        "TERM and COLORTERM must describe yagura's terminal whatever the launcher set"
    );
}

#[tokio::test]
async fn the_launching_terminals_own_variables_do_not_reach_a_window_child() {
    let names = [
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "VSCODE_INJECTION",
        "VSCODE_GIT_IPC_HANDLE",
        "WT_SESSION",
        "KITTY_WINDOW_ID",
        "ConEmuANSI",
        "TMUX",
    ];
    let reports = probe_after_launch("child-terminal-stripped", LAUNCHER_ENV, &names, "").await;
    let leaked: Vec<(&str, String)> = names
        .iter()
        .zip(reports)
        .filter_map(|(name, report)| report.map(|value| (*name, value)))
        .collect();
    assert!(
        leaked.is_empty(),
        "no variable of the launching terminal may reach the child; leaked: {leaked:?}"
    );
}

#[tokio::test]
async fn vs_codes_askpass_goes_with_its_variables_and_any_other_askpass_stays() {
    let from_vs_code = probe_after_launch(
        "child-terminal-askpass-vscode",
        LAUNCHER_ENV,
        &["GIT_ASKPASS"],
        "",
    )
    .await;
    assert_eq!(
        from_vs_code,
        vec![None],
        "VS Code's askpass cannot work without the VS Code variables taken away with it"
    );

    let own_askpass = [("GIT_ASKPASS", "user-askpass")];
    let from_elsewhere = probe_after_launch(
        "child-terminal-askpass-own",
        &own_askpass,
        &["GIT_ASKPASS"],
        "",
    )
    .await;
    assert_eq!(
        from_elsewhere,
        vec![Some("user-askpass".to_string())],
        "an askpass the user set for themselves is theirs to keep"
    );
}

#[tokio::test]
async fn a_profiles_env_still_sets_any_of_these_variables() {
    let names = ["TERM", "COLORTERM", "TERM_PROGRAM"];
    let env_table = "\n[env]\nTERM = 'dumb'\nCOLORTERM = '24bit'\nTERM_PROGRAM = 'chosen'\n";
    let reports =
        probe_after_launch("child-terminal-profile", LAUNCHER_ENV, &names, env_table).await;
    assert_eq!(
        reports,
        vec![
            Some("dumb".to_string()),
            Some("24bit".to_string()),
            Some("chosen".to_string())
        ],
        "a profile's [env] must win over what yagura declares and strips"
    );
}
