#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon reports `status_reports_missing` on its window records so the
//! GUI can mark a window; the MCP surface is an external contract that must
//! not grow with it, so `list_windows` leaves the fact out whatever its value.

use std::sync::Mutex;

use yagura_mcp::tools::list_windows;
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowInfo, WindowStatus};

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
        mcp_connected: Some(true),
        status_reports_missing,
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
        assert_eq!(
            entry.get("mcp_connected"),
            Some(&serde_json::json!(true)),
            "the fields agents already read stay as they were, got: {entry}"
        );
    }
}
