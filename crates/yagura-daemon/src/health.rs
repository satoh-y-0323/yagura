//! What keeps this daemon from doing its job, as `get_daemon_health` reports it.

use yagura_protocol::{DaemonProblem, DaemonProblemKind};

/// Decides the problems to report from what the daemon knows.
///
/// `conpty_fault` is the text of the bundled pseudoconsole check's failure, if
/// it failed.
#[must_use]
pub fn daemon_problems(conpty_fault: Option<&str>) -> Vec<DaemonProblem> {
    conpty_fault
        .map(|fault| DaemonProblem {
            kind: DaemonProblemKind::WindowsUnavailable,
            detail: fault.to_string(),
        })
        .into_iter()
        .collect()
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
    fn no_fault_means_no_problems() {
        assert!(daemon_problems(None).is_empty());
    }

    #[test]
    fn a_pseudoconsole_fault_is_reported_with_its_text() {
        let problems = daemon_problems(Some("conpty.dll missing"));
        assert_eq!(
            problems,
            vec![DaemonProblem {
                kind: DaemonProblemKind::WindowsUnavailable,
                detail: "conpty.dll missing".to_string(),
            }]
        );
    }
}
