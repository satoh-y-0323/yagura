#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon reports `status_reports_missing`, `status_boot` and `task_report`
//! on its window records for the GUI; the MCP surface is an external contract
//! that must not grow with them, so `list_windows` leaves them out whatever
//! their value.

use std::sync::Mutex;

use yagura_daemon::mcp::tools::list_windows;
use yagura_daemon::mcp::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{
    ClientMessage, DaemonMessage, StatusBoot, TaskReportMark, WindowInfo, WindowStatus,
};

struct OneReply(Mutex<Option<DaemonMessage>>);

impl DaemonTransport for OneReply {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::External
    }

    async fn round_trip(&self, _msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        Ok(self
            .0
            .lock()
            .expect("reply mutex must not be poisoned")
            .take()
            .expect("one reply is scripted"))
    }
}

fn window(window_id: &str, status_reports_missing: Option<bool>) -> WindowInfo {
    WindowInfo {
        window_id: window_id.to_string(),
        name: "cli".to_string(),
        vendor: None,
        cwd: "/work".to_string(),
        status: WindowStatus::Running,
        exit_code: None,
        layout_json: None,
        room: None,
        unknown_reason: None,
        detection_warning: None,
        status_rev: Some(1),
        status_boot: Some(StatusBoot { origin: 7, seq: 3 }),
        mcp_connected: Some(true),
        status_reports_missing,
        queued_count: None,
        task_report: Some(TaskReportMark {
            seq: 2,
            standing: true,
        }),
    }
}

#[tokio::test]
async fn list_windows_does_not_surface_status_reports_missing() {
    let transport = OneReply(Mutex::new(Some(DaemonMessage::WindowList {
        windows: vec![
            window("w1", Some(true)),
            window("w2", Some(false)),
            window("w3", None),
        ],
    })));

    let result = list_windows(&transport)
        .await
        .expect("list_windows must succeed");

    let windows = result["windows"].as_array().expect("windows is an array");
    assert_eq!(windows.len(), 3);
    for entry in windows {
        assert!(
            entry.get("status_reports_missing").is_none(),
            "the mark is for the GUI only and must not reach an agent, got: {entry}"
        );
        assert!(
            entry.get("status_boot").is_none(),
            "the boot the GUI orders status by must not reach an agent, got: {entry}"
        );
        assert!(
            entry.get("task_report").is_none(),
            "the standing-report fact is for the GUI only and must not reach an agent, got: {entry}"
        );
        assert_eq!(
            entry.get("mcp_connected"),
            Some(&serde_json::json!(true)),
            "the fields agents already read stay as they were, got: {entry}"
        );
    }
}
