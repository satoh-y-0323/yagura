#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon reports `status_reports_missing`, `status_boot`, `task_report` and
//! `mark` on its window records for the GUI; the MCP surface is an external
//! contract that must not grow with them, so `list_windows` leaves them out
//! whatever their value.

use std::sync::Mutex;

use serde_json::json;

use yagura_daemon::mcp::tools::list_windows;
use yagura_daemon::mcp::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{
    ClientMessage, DaemonMessage, ReportState, StatusBoot, TaskReportMark, WindowInfo, WindowStatus,
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

/// A window record as the daemon sends it, read through the wire form so that the
/// GUI-only `mark` is set the way the daemon sets it on every window.
fn window(window_id: &str, status_reports_missing: Option<bool>, state: ReportState) -> WindowInfo {
    serde_json::from_value(json!({
        "window_id": window_id,
        "name": "cli",
        "cwd": "/work",
        "status": WindowStatus::Running,
        "status_rev": 1,
        "status_boot": StatusBoot { origin: 7, seq: 3 },
        "mcp_connected": true,
        "status_reports_missing": status_reports_missing,
        "task_report": TaskReportMark { seq: 2, state },
        "mark": { "cells": 12097, "hue": 6 },
    }))
    .expect("a window record decodes")
}

#[tokio::test]
async fn list_windows_does_not_surface_status_reports_missing() {
    let scripted = vec![
        window("w1", Some(true), ReportState::Standing),
        window("w2", Some(false), ReportState::Instruction),
        window("w3", None, ReportState::NoReader),
    ];
    for record in &scripted {
        assert!(
            serde_json::to_value(record).expect("a window record encodes")["mark"].is_object(),
            "fixture premise: the daemon's record carries the mark, got: {record:?}"
        );
    }
    let transport = OneReply(Mutex::new(Some(DaemonMessage::WindowList {
        windows: scripted,
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
            entry.get("mark").is_none(),
            "the mark is drawn by the GUI and must not reach an agent, got: {entry}"
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
