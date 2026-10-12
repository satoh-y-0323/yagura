#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The per-window `StatusEvent` authentication token: the `StatusToken` primitive, its
//! per-window verification entry point, PTY env injection, and the distribution of
//! `YAGURA_STATUS_TOKEN` to spawned children. The IPC-side acceptance / rejection of
//! `ClientMessage::StatusEvent` lives in `tests/ipc_control.rs` and the wire format in
//! the `yagura-protocol` tests; this file never opens a pipe.
//!
//! `DaemonConfig::status_token_override` is the only supported way for a test to hold a
//! valid token: no raw-value getter is exposed.
//!
//! The "no injection while the token is unverifiable" case cannot be constructed at this
//! level -- that state is only reachable when the OS randomness source fails, and a
//! malformed override falls back to generation by design. It is substituted by the
//! stronger property that the value handed to the child is the very value that window
//! verifies against.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use yagura_daemon::token::StatusToken;
use yagura_daemon::{DaemonConfig, OpenWindowRequest, PtySession, WindowEntry, WindowRegistry};

/// A well-formed hex-32 value fed to `DaemonConfig::status_token_override`. Fixed rather
/// than random so a failure message is reproducible; it never reaches a real daemon.
const KNOWN_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A second well-formed hex-32 value, so a rejection can never be explained by the
/// candidate merely being malformed.
const OTHER_TOKEN: &str = "fedcba9876543210fedcba9876543210";

/// Marker value delivered through `extra_env` (never concatenated into the
/// command line -- see the deviation note on the test that uses it).
const ENV_MARKER_VALUE: &str = "yagura-env-marker-2f6a1c";

fn token_override_config(value: &str) -> DaemonConfig {
    DaemonConfig {
        status_token_override: Some(value.into()),
        ..DaemonConfig::default()
    }
}

/// Opens a window whose child stays alive (an interactive shell sitting at its prompt)
/// so the returned entry can be inspected. The caller terminates the child.
async fn open_long_lived_window(registry: &WindowRegistry) -> Arc<WindowEntry> {
    #[cfg(windows)]
    let shell = vec!["cmd.exe".to_string()];
    #[cfg(unix)]
    let shell = vec!["/bin/sh".to_string()];

    let window_id = registry
        .open(OpenWindowRequest {
            command: shell,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("registry.open must spawn the child")
        .window_id;
    registry
        .get(&window_id)
        .expect("the opened window must be in the registry")
}

/// Extracts the first run of exactly 32 lowercase hex characters from `text`, so the
/// assertion compares what crossed the process boundary rather than the fixture value.
fn find_hex32(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let is_hex = |c: char| matches!(c, '0'..='9' | 'a'..='f');
    let mut start = 0usize;
    while start + 32 <= chars.len() {
        let run_len = chars[start..].iter().take_while(|c| is_hex(**c)).count();
        if run_len == 32 {
            let before_ok = start == 0 || !is_hex(chars[start - 1]);
            if before_ok {
                return Some(chars[start..start + 32].iter().collect());
            }
        }
        start += 1;
    }
    None
}

#[test]
fn generate_produces_lowercase_hex_of_exactly_32_chars() {
    let token = StatusToken::generate();
    let hex = token
        .as_hex()
        .expect("generate must yield a usable token on a machine where ProcessPrng works");
    assert_eq!(
        hex.len(),
        32,
        "a 128-bit token must render as exactly 32 hex characters, got {hex:?}"
    );
    assert!(
        hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        "token must be lowercase hex ([0-9a-f]), got {hex:?}"
    );
}

#[test]
fn generate_produces_a_different_value_on_each_call() {
    let first = StatusToken::generate();
    let second = StatusToken::generate();
    let first_hex = first.as_hex().expect("first generate must yield a token");
    let second_hex = second.as_hex().expect("second generate must yield a token");
    assert_ne!(
        first_hex, second_hex,
        "two generated tokens must differ; an equal pair means the value is not random"
    );
}

#[test]
fn generate_verifies_its_own_hex_value() {
    let token = StatusToken::generate();
    let hex = token
        .as_hex()
        .expect("generate must yield a token")
        .to_string();
    assert!(
        token.verify(&hex),
        "the value handed out by as_hex must be the value verify accepts"
    );
}

#[test]
fn from_override_accepts_a_valid_hex32_value() {
    let token =
        StatusToken::from_override(KNOWN_TOKEN).expect("a well-formed hex-32 value must be Some");
    assert_eq!(
        token.as_hex(),
        Some(KNOWN_TOKEN),
        "an adopted override must be kept verbatim"
    );
}

#[test]
fn from_override_rejects_values_of_the_wrong_length() {
    for value in [
        "",
        "0123456789abcdef0123456789abcde",   // 31 chars
        "0123456789abcdef0123456789abcdef0", // 33 chars
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", // 64 chars
    ] {
        assert!(
            StatusToken::from_override(value).is_none(),
            "a {}-character override must be rejected",
            value.len()
        );
    }
}

#[test]
fn from_override_rejects_non_hex_values() {
    for value in [
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz", // 32 chars, none of them hex
        "0123456789abcdef0123456789abcde!", // 32 chars, last one not hex
        "0123456789abcdef 123456789abcdef", // 32 chars with an embedded space
    ] {
        assert!(
            StatusToken::from_override(value).is_none(),
            "a non-hex override must be rejected: {value:?}"
        );
    }
}

#[test]
fn verify_accepts_the_exact_token_value() {
    let token = StatusToken::from_override(KNOWN_TOKEN).expect("fixture must be a valid override");
    assert!(
        token.verify(KNOWN_TOKEN),
        "verify must accept the token's own value"
    );
}

#[test]
fn verify_rejects_a_mismatched_value() {
    let token = StatusToken::from_override(KNOWN_TOKEN).expect("fixture must be a valid override");
    assert!(
        !token.verify(OTHER_TOKEN),
        "verify must reject an equally well-formed but different token"
    );
}

#[test]
fn verify_rejects_empty_oversized_and_non_hex_candidates() {
    let token = StatusToken::from_override(KNOWN_TOKEN).expect("fixture must be a valid override");

    // Each candidate doubles as a "must not panic" case: a length- or charset-sensitive
    // comparison that indexed the candidate would abort here instead of returning false.
    let oversized_65 = "a".repeat(65);
    let oversized_long = "b".repeat(4096);
    for candidate in [
        "",
        "0123456789abcdef",                  // valid hex, too short
        "0123456789ABCDEF0123456789ABCDEF0", // 33 chars
        "not-hex-at-all",                    // non-hex
        oversized_65.as_str(),
        oversized_long.as_str(),
        "\u{1F600}\u{1F600}\u{1F600}", // multi-byte input must not panic either
    ] {
        assert!(
            !token.verify(candidate),
            "verify must reject candidate {candidate:?}"
        );
    }
}

#[test]
fn debug_output_does_not_leak_the_token_value() {
    let token = StatusToken::from_override(KNOWN_TOKEN).expect("fixture must be a valid override");
    let rendered = format!("{token:?}");
    assert!(
        !rendered.contains(KNOWN_TOKEN),
        "Debug must mask the token value; got {rendered:?}"
    );
    assert!(
        !rendered.is_empty(),
        "Debug must still render something identifiable (Rust API Guidelines C-COMMON-TRAITS)"
    );
}

#[tokio::test]
async fn window_verify_status_token_accepts_the_configured_override() {
    let registry = WindowRegistry::new(token_override_config(KNOWN_TOKEN));
    let entry = open_long_lived_window(&registry).await;
    assert!(
        entry.verify_status_token(KNOWN_TOKEN),
        "a window must verify against the token given via status_token_override"
    );
    entry.session().terminate();
}

#[tokio::test]
async fn window_verify_status_token_rejects_a_value_other_than_the_configured_override() {
    let registry = WindowRegistry::new(token_override_config(KNOWN_TOKEN));
    let entry = open_long_lived_window(&registry).await;
    assert!(
        !entry.verify_status_token(OTHER_TOKEN),
        "a window must reject a token the registry was not configured with"
    );
    entry.session().terminate();
}

#[tokio::test]
async fn spawn_with_command_in_env_delivers_extra_env_to_the_child() {
    // Deviation from the "never spawn through a shell with a concatenated command string"
    // rule: only the shell's built-in echo can show whether a value reached the child's
    // environment. The argv is fixed literals; the marker travels through extra_env.
    #[cfg(windows)]
    let argv = ["cmd.exe", "/c", "echo [%YAGURA_TEST_ENV_MARKER%]"];
    #[cfg(unix)]
    let argv = ["/bin/sh", "-c", "echo [$YAGURA_TEST_ENV_MARKER]"];
    let config = DaemonConfig::default();
    let session = PtySession::spawn_with_command_in_env(
        &argv,
        None::<&Path>,
        &[("YAGURA_TEST_ENV_MARKER", ENV_MARKER_VALUE)],
        &config,
    )
    .await
    .expect("spawn_with_command_in_env must spawn the shell");

    let text =
        common::wait_for_scrollback_containing(&session, ENV_MARKER_VALUE, Duration::from_secs(10))
            .await;
    assert!(
        text.contains(&format!("[{ENV_MARKER_VALUE}]")),
        "the child must observe the injected variable's value, not the unexpanded name; got {text:?}"
    );
    session.terminate();
}

#[tokio::test]
async fn registry_open_injects_the_status_token_that_verify_status_token_accepts() {
    // Same deviation as above: the shell's built-in echo is the only way to observe that
    // an environment variable arrived. The argv is fixed literals.
    #[cfg(windows)]
    let command = vec![
        "cmd.exe".to_string(),
        "/c".to_string(),
        "echo %YAGURA_STATUS_TOKEN%".to_string(),
    ];
    #[cfg(unix)]
    let command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo $YAGURA_STATUS_TOKEN".to_string(),
    ];

    let registry = WindowRegistry::new(token_override_config(KNOWN_TOKEN));
    let window_id = registry
        .open(OpenWindowRequest {
            command,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("registry.open must spawn the child")
        .window_id;
    let entry = registry
        .get(&window_id)
        .expect("the opened window must be in the registry");

    let text = common::wait_for_scrollback_containing(
        entry.session(),
        KNOWN_TOKEN,
        Duration::from_secs(10),
    )
    .await;

    // The value is read back out of the child's own output, so this confirms the child
    // received exactly what the window verifies against.
    let Some(observed) = find_hex32(&text) else {
        panic!("the child's output must contain a 32-character hex token; got {text:?}");
    };
    assert_eq!(
        observed, KNOWN_TOKEN,
        "the value delivered to the child must be the configured token"
    );
    assert!(
        entry.verify_status_token(&observed),
        "the token the child received must be the very token its window verifies against"
    );

    entry.session().terminate();
}

#[tokio::test]
async fn registry_open_injects_the_window_id_alongside_the_status_token() {
    // YAGURA_WINDOW_ID is injected in the same extra_env batch as YAGURA_STATUS_TOKEN and
    // in the same divergence-gated branch. Same deviation as above: the shell's built-in
    // echo is the only way to observe that an environment variable arrived.
    #[cfg(windows)]
    let command = vec![
        "cmd.exe".to_string(),
        "/c".to_string(),
        "echo %YAGURA_STATUS_TOKEN% %YAGURA_WINDOW_ID%".to_string(),
    ];
    #[cfg(unix)]
    let command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo $YAGURA_STATUS_TOKEN $YAGURA_WINDOW_ID".to_string(),
    ];

    let registry = WindowRegistry::new(token_override_config(KNOWN_TOKEN));
    let window_id = registry
        .open(OpenWindowRequest {
            command,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("registry.open must spawn the child")
        .window_id;
    let entry = registry
        .get(&window_id)
        .expect("the opened window must be in the registry");

    // The wait is on the window_id, not the token: the token is a bare 32-char hex run
    // with nothing tying it to this child's own output, so the window_id is what proves
    // the line the token is read from is the one YAGURA_WINDOW_ID landed in.
    let text = common::wait_for_scrollback_containing(
        entry.session(),
        &window_id,
        Duration::from_secs(10),
    )
    .await;

    let Some(observed_token) = find_hex32(&text) else {
        panic!("the child's output must contain a 32-character hex token; got {text:?}");
    };
    assert_eq!(
        observed_token, KNOWN_TOKEN,
        "YAGURA_WINDOW_ID must be injected alongside YAGURA_STATUS_TOKEN, in the same env \
         batch, so a child that received one also received the other"
    );

    entry.session().terminate();
}

#[test]
fn token_source_never_passes_as_hex_to_logging_or_formatting() {
    // Line comments are stripped and the rest split into `;`-separated statements; every
    // statement mentioning `as_hex` must mention no logging or formatting macro. Splitting
    // by statement rather than by line catches calls spread over several lines.
    let token_src = include_str!("../src/token.rs");
    let code: String = token_src
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    const SINKS: &[&str] = &[
        "tracing::",
        "info!",
        "warn!",
        "error!",
        "debug!",
        "trace!",
        "format!",
        "println!",
        "eprintln!",
        "write!",
        "writeln!",
        "panic!",
    ];

    for statement in code.split(';') {
        if !statement.contains("as_hex") {
            continue;
        }
        for sink in SINKS {
            assert!(
                !statement.contains(sink),
                "src/token.rs must never route as_hex() into {sink} (the token must \
                 not appear in logs, errors or Debug output). If a masked rendering needs the \
                 token's state, compute the mask in a separate statement. Offending statement: \
                 {statement}"
            );
        }
    }
}

#[test]
fn handle_identify_never_lets_the_candidate_token_reach_a_logging_or_formatting_sink() {
    // Same statement-scoped scan as `token_source_never_passes_as_hex_to_logging_or_formatting`
    // above, applied to the `identify` request path in `ipc.rs`: the
    // candidate token a client sends must never be routed into a logging or
    // formatting sink either. An absent `handle_identify` body means the
    // wiring this scan is meant to guard does not exist yet.
    let ipc_src = include_str!("../src/ipc.rs");
    let body = common::extract_fn_body(ipc_src, "handle_identify").unwrap_or_else(|| {
        panic!(
            "src/ipc.rs must define a handle_identify function body that verifies the identify \
             token"
        )
    });
    let code: String = body
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    const SINKS: &[&str] = &[
        "tracing::",
        "info!",
        "warn!",
        "error!",
        "debug!",
        "trace!",
        "format!",
        "println!",
        "eprintln!",
        "write!",
        "writeln!",
        "panic!",
    ];

    for statement in code.split(';') {
        if !statement.contains("token") {
            continue;
        }
        for sink in SINKS {
            assert!(
                !statement.contains(sink),
                "handle_identify must never route the candidate token into {sink} (the token \
                 must not appear in logs, errors or Debug output). Offending statement: \
                 {statement}"
            );
        }
    }
}
