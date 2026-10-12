//! What keeps this daemon from doing its job, as `get_daemon_health` reports it.

use yagura_protocol::{DaemonProblem, DaemonProblemKind};

/// Decides the problems to report from the two facts the daemon holds.
///
/// `conpty_fault` is the text of the bundled pseudoconsole check's failure, if
/// it failed; `initial_window_failure` is why the window opened at start-up
/// could not be opened, if it could not. The initial window's failure is left
/// out when the pseudoconsole is faulty, because that is its cause and a second
/// entry would only repeat it.
#[must_use]
pub fn daemon_problems(
    conpty_fault: Option<&str>,
    initial_window_failure: Option<&str>,
) -> Vec<DaemonProblem> {
    match (conpty_fault, initial_window_failure) {
        (Some(fault), _) => vec![DaemonProblem {
            kind: DaemonProblemKind::WindowsUnavailable,
            detail: fault.to_string(),
        }],
        (None, Some(reason)) => vec![DaemonProblem {
            kind: DaemonProblemKind::InitialWindowFailed,
            detail: reason.to_string(),
        }],
        (None, None) => Vec::new(),
    }
}

/// Blocking: runs the bundled pseudoconsole check beside the running
/// executable and returns the failure's text, `None` when it passes (always
/// off Windows).
#[must_use]
pub fn conpty_fault_now() -> Option<String> {
    yagura_os::bundled_conpty::verify_beside_executable()
        .err()
        .map(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_facts_means_no_problems() {
        assert!(daemon_problems(None, None).is_empty());
    }

    #[test]
    fn a_pseudoconsole_fault_is_reported_with_its_text() {
        let problems = daemon_problems(Some("conpty.dll missing"), None);
        assert_eq!(
            problems,
            vec![DaemonProblem {
                kind: DaemonProblemKind::WindowsUnavailable,
                detail: "conpty.dll missing".to_string(),
            }]
        );
    }

    #[test]
    fn an_initial_window_failure_alone_is_reported_with_its_reason() {
        let problems = daemon_problems(None, Some("spawn failed"));
        assert_eq!(
            problems,
            vec![DaemonProblem {
                kind: DaemonProblemKind::InitialWindowFailed,
                detail: "spawn failed".to_string(),
            }]
        );
    }

    #[test]
    fn the_initial_window_failure_is_left_out_when_the_pseudoconsole_is_faulty() {
        let problems = daemon_problems(Some("conpty.dll missing"), Some("spawn failed"));
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems.first().map(|p| p.kind),
            Some(DaemonProblemKind::WindowsUnavailable)
        );
    }
}
