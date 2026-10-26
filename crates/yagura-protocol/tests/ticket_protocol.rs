#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for tickets (wire revision 29): the ticket
//! requests and replies, the `ticket_changed` push, and the `ticket` link
//! `create_plan` and `PlanInfo` carry.
//!
//! Frames are written as raw JSON and compared after a decode / encode round
//! trip, so the contract is the wire itself rather than the Rust shape:
//!
//! - every ticket message decodes from its snake_case `type` tag and encodes
//!   back to the same frame, and an optional field left out stays out (it is
//!   not written back as `null`);
//! - `required_protocol_revision` asks for `TICKETS_MIN_REVISION` before any
//!   ticket request or a `create_plan` that names a ticket, and leaves a
//!   `create_plan` without one where it was;
//! - an older peer's `create_plan` / `PlanInfo` frame without `ticket` still
//!   decodes, as no ticket.

use serde_json::{Value, json};
use yagura_protocol::{
    ClientMessage, DaemonMessage, PROTOCOL_REVISION, PROTOCOL_REVISION_BASELINE,
    TASK_INSTRUCTION_MIN_REVISION, TICKETS_MIN_REVISION, decode_client_message,
    decode_daemon_message, encode_client_message, encode_daemon_message,
    required_protocol_revision,
};

fn decode_client(frame: &Value) -> ClientMessage {
    let body = serde_json::to_vec(frame).unwrap();
    decode_client_message(&body)
        .unwrap_or_else(|err| panic!("client frame must decode ({err:?}): {frame}"))
}

fn decode_daemon(frame: &Value) -> DaemonMessage {
    let body = serde_json::to_vec(frame).unwrap();
    decode_daemon_message(&body)
        .unwrap_or_else(|err| panic!("daemon frame must decode ({err:?}): {frame}"))
}

fn assert_client_round_trip(frame: Value) {
    let msg = decode_client(&frame);
    let encoded: Value = serde_json::from_slice(&encode_client_message(&msg)).unwrap();
    assert_eq!(
        encoded, frame,
        "a client frame must encode back to itself, with omitted fields still omitted"
    );
}

fn assert_daemon_round_trip(frame: Value) {
    let msg = decode_daemon(&frame);
    let encoded: Value = serde_json::from_slice(&encode_daemon_message(&msg)).unwrap();
    assert_eq!(
        encoded, frame,
        "a daemon frame must encode back to itself, with omitted fields still omitted"
    );
}

fn task_spec(instruction: Option<&str>) -> Value {
    let mut spec = json!({
        "task_id": "t1",
        "title": "write the tests",
        "role": null,
        "profile": null,
        "model": null,
        "effort": null,
        "depends_on": [],
        "writes": [],
    });
    if let Some(text) = instruction {
        spec["instruction"] = json!(text);
    }
    spec
}

/// One full request frame per ticket request, every optional field filled.
fn full_ticket_requests() -> Vec<Value> {
    vec![
        json!({ "type": "list_ticket_workspaces" }),
        json!({
            "type": "list_tickets",
            "workspace_id": 3,
            "state": "todo",
            "milestone": "m1",
            "limit": 200,
        }),
        json!({ "type": "get_ticket", "workspace_id": 3, "ticket": 12 }),
        json!({
            "type": "create_ticket",
            "title": "Ship the board",
            "body": "line one\nline two",
            "milestone": "m1",
            "state": "considering",
            "fields": { "start_condition": "after the wire lands" },
        }),
        json!({
            "type": "update_ticket",
            "ticket": 12,
            "title": "Ship the board",
            "body": "rewritten",
            "milestone": "",
            "state": "on-hold",
            "fields": { "resume_trigger": "the store is merged", "reason": "" },
        }),
        json!({ "type": "add_ticket_result", "ticket": 12, "text": "merged as abc123" }),
        json!({
            "type": "set_milestone",
            "name": "m1",
            "title": "First milestone",
            "description": "The board shows tickets.",
            "position": 1,
        }),
    ]
}

/// The same requests with every optional field left out.
fn minimal_ticket_requests() -> Vec<Value> {
    vec![
        json!({ "type": "list_tickets" }),
        json!({ "type": "get_ticket", "ticket": 12 }),
        json!({ "type": "create_ticket", "title": "Ship the board", "body": "" }),
        json!({ "type": "update_ticket", "ticket": 12 }),
        json!({ "type": "set_milestone", "name": "m1", "title": "First milestone", "position": -2 }),
    ]
}

#[test]
fn ticket_requests_round_trip_through_their_snake_case_tags() {
    for frame in full_ticket_requests() {
        assert_client_round_trip(frame);
    }
}

#[test]
fn ticket_requests_keep_omitted_optional_fields_off_the_wire() {
    for frame in minimal_ticket_requests() {
        assert_client_round_trip(frame);
    }
}

#[test]
fn ticket_replies_and_push_round_trip_through_their_snake_case_tags() {
    let frames = [
        json!({
            "type": "ticket_workspace_list",
            "workspaces": [
                { "workspace_id": 3, "kind": "git", "path": "/work/yagura" },
                { "workspace_id": 4, "kind": "directory", "path": "/work/notes" },
            ],
        }),
        json!({
            "type": "ticket_list",
            "workspace_id": 3,
            "config": {
                "guide": "One ticket, one purpose.",
                "states": [
                    { "name": "todo", "description": "Decided.", "requires": [] },
                    { "name": "on-hold", "description": "Paused.", "requires": ["resume_trigger"] },
                ],
                "fields": [
                    { "name": "resume_trigger", "description": "What brings it back." },
                ],
            },
            "milestones": [
                { "name": "m1", "position": 1, "title": "First", "description": "The board." },
            ],
            "tickets": [
                { "number": 12, "title": "Ship the board", "state": "todo", "milestone": "m1", "updated_at": 1_790_000_000_000_u64, "plan_count": 0, "fields": {} },
            ],
            "truncated": false,
        }),
        json!({
            "type": "ticket_detail",
            "ticket": {
                "number": 12,
                "title": "Ship the board",
                "body": "line one\nline two",
                "state": "on-hold",
                "milestone": "m1",
                "fields": { "resume_trigger": "the store is merged" },
                "results": [ { "seq": 1, "at": 1_790_000_000_500_u64, "text": "merged" } ],
                "plans": ["p-1", "p-2"],
                "created_at": 1_790_000_000_000_u64,
                "updated_at": 1_790_000_000_900_u64,
            },
        }),
        json!({
            "type": "milestone_set",
            "milestone": { "name": "m1", "position": 1, "title": "First", "description": "The board." },
        }),
        json!({ "type": "ticket_changed", "workspace_id": 3 }),
    ];
    for frame in frames {
        assert_daemon_round_trip(frame);
    }
}

#[test]
fn ticket_replies_keep_omitted_optional_fields_off_the_wire() {
    let frames = [
        json!({
            "type": "ticket_list",
            "config_error": "tickets.toml could not be read",
            "milestones": [ { "name": "m1", "position": 0, "title": "First" } ],
            "tickets": [
                { "number": 1, "title": "No milestone", "state": "gone-state", "updated_at": 5, "plan_count": 0, "fields": {} },
            ],
            "truncated": true,
        }),
        json!({
            "type": "ticket_detail",
            "ticket": {
                "number": 1,
                "title": "Bare",
                "body": "",
                "state": "todo",
                "fields": {},
                "results": [],
                "plans": [],
                "created_at": 5,
                "updated_at": 5,
            },
        }),
        json!({
            "type": "milestone_set",
            "milestone": { "name": "m1", "position": 0, "title": "First" },
        }),
    ];
    for frame in frames {
        assert_daemon_round_trip(frame);
    }
}

#[test]
fn a_ticket_summary_carries_its_plan_count_and_required_field_values() {
    let frame = json!({
        "type": "ticket_list",
        "milestones": [],
        "tickets": [
            {
                "number": 7,
                "title": "Paused work",
                "state": "on-hold",
                "updated_at": 9,
                "plan_count": 2,
                "fields": { "resume_trigger": "after the release" },
            },
        ],
        "truncated": false,
    });
    assert_daemon_round_trip(frame);
}

#[test]
fn the_tickets_revision_is_twenty_nine_and_is_the_current_revision() {
    assert_eq!(TICKETS_MIN_REVISION, 29);
    assert_eq!(PROTOCOL_REVISION, 29);
}

#[test]
fn every_ticket_request_requires_the_tickets_revision() {
    for frame in full_ticket_requests()
        .into_iter()
        .chain(minimal_ticket_requests())
    {
        let msg = decode_client(&frame);
        assert_eq!(
            required_protocol_revision(&msg),
            TICKETS_MIN_REVISION,
            "a ticket request must wait for a daemon that knows tickets: {frame}"
        );
    }
}

#[test]
fn create_plan_requires_the_tickets_revision_only_when_it_names_a_ticket() {
    let with_ticket = decode_client(&json!({
        "type": "create_plan",
        "title": "plan",
        "tasks": [task_spec(None)],
        "ticket": 12,
    }));
    assert_eq!(
        required_protocol_revision(&with_ticket),
        TICKETS_MIN_REVISION
    );

    let with_ticket_and_instruction = decode_client(&json!({
        "type": "create_plan",
        "title": "plan",
        "tasks": [task_spec(Some("read the plan"))],
        "ticket": 12,
    }));
    assert_eq!(
        required_protocol_revision(&with_ticket_and_instruction),
        TICKETS_MIN_REVISION,
        "a plan naming a ticket needs the larger of the two thresholds"
    );

    let instruction_only = decode_client(&json!({
        "type": "create_plan",
        "title": "plan",
        "tasks": [task_spec(Some("read the plan"))],
    }));
    assert_eq!(
        required_protocol_revision(&instruction_only),
        TASK_INSTRUCTION_MIN_REVISION
    );

    let plain = decode_client(&json!({
        "type": "create_plan",
        "title": "plan",
        "tasks": [task_spec(None)],
    }));
    assert_eq!(
        required_protocol_revision(&plain),
        PROTOCOL_REVISION_BASELINE
    );
}

#[test]
fn create_plan_carries_its_ticket_and_still_reads_an_older_frame_without_one() {
    assert_client_round_trip(json!({
        "type": "create_plan",
        "title": "plan",
        "tasks": [task_spec(None)],
        "ticket": 12,
    }));
    assert_client_round_trip(json!({
        "type": "create_plan",
        "title": "plan",
        "tasks": [task_spec(None)],
    }));
}

fn plan_info_frame(ticket: Option<u32>) -> Value {
    let mut plan = json!({
        "plan_id": "p-1",
        "title": "ship the board",
        "cwd": "/work/yagura",
        "owner_window": null,
        "waves": [],
        "tasks": [],
        "roles_warning": null,
        "version": 0,
    });
    if let Some(number) = ticket {
        plan["ticket"] = json!(number);
    }
    json!({ "type": "plan_detail", "plan": plan })
}

#[test]
fn plan_info_carries_its_ticket_and_still_reads_an_older_frame_without_one() {
    assert_daemon_round_trip(plan_info_frame(Some(12)));
    assert_daemon_round_trip(plan_info_frame(None));
}
