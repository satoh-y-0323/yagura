#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The wire form of the daemon's view layer: attaching to a window
//! (`attach_view`), the `frame` that carries the window's cells, its
//! acknowledgement, the palette the GUI sends, the selection copy and the
//! scroll message. The old raw-byte stream (`output`, `resync`, `attach_at`,
//! `screen_b64`) is gone.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use yagura_protocol::{
    CellAttr, CellColor, CellFlag, ClientMessage, ColorOverride, CursorShape, CursorState,
    DaemonMessage, FRAME_BUDGET_BYTES, FRAME_MIN_REVISION, Frame, FrameLine, FrameRun,
    MAX_FRAME_BODY_LEN, MessageError, Palette, PaletteSlot, TerminalSize, ViewState,
    client_message_wire_tags, daemon_message_wire_tags, decode_client_message,
    decode_daemon_message, encode_client_message, encode_daemon_message,
    required_protocol_revision,
};

fn wire(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("a frame body is JSON")
}

fn client_round_trip(msg: &ClientMessage) -> Value {
    let body = encode_client_message(msg);
    assert_eq!(decode_client_message(&body).as_ref(), Ok(msg));
    wire(&body)
}

fn daemon_round_trip(msg: &DaemonMessage) -> Value {
    let body = encode_daemon_message(msg);
    assert_eq!(decode_daemon_message(&body).as_ref(), Ok(msg));
    wire(&body)
}

// --- attaching ---------------------------------------------------------

#[test]
fn attach_view_carries_the_window_and_the_size() {
    let msg = ClientMessage::AttachView {
        window_id: "w1".to_string(),
        size: Some(TerminalSize {
            cols: 120,
            rows: 40,
        }),
    };
    assert_eq!(
        client_round_trip(&msg),
        json!({ "type": "attach_view", "window_id": "w1", "size": { "cols": 120, "rows": 40 } })
    );
}

#[test]
fn attach_view_without_a_size_decodes_with_none_and_omits_it() {
    let decoded = decode_client_message(br#"{"type":"attach_view","window_id":"w1"}"#);
    let msg = ClientMessage::AttachView {
        window_id: "w1".to_string(),
        size: None,
    };
    assert_eq!(decoded.as_ref(), Ok(&msg));
    assert_eq!(
        client_round_trip(&msg),
        json!({ "type": "attach_view", "window_id": "w1" })
    );
}

#[test]
fn attach_view_without_a_window_is_invalid_json() {
    assert_eq!(
        decode_client_message(br#"{"type":"attach_view","size":{"cols":80,"rows":24}}"#),
        Err(MessageError::InvalidJson)
    );
}

#[test]
fn a_gui_that_still_sends_attach_at_is_refused_as_an_unknown_type() {
    assert_eq!(
        decode_client_message(br#"{"type":"attach_at","window_id":"w1"}"#),
        Err(MessageError::UnknownType)
    );
    assert!(!client_message_wire_tags().contains(&"attach_at"));
    assert!(client_message_wire_tags().contains(&"attach_view"));
}

#[test]
fn attached_carries_only_the_exit_state() {
    let msg = DaemonMessage::Attached {
        child_exited: true,
        exit_code: Some(5),
    };
    assert_eq!(
        daemon_round_trip(&msg),
        json!({ "type": "attached", "child_exited": true, "exit_code": 5 })
    );
}

#[test]
fn the_raw_output_stream_is_no_longer_part_of_the_wire() {
    for body in [
        br#"{"type":"output","data_b64":"aGVsbG8="}"#.as_slice(),
        br#"{"type":"resync","screen_b64":"c2NyZWVu"}"#.as_slice(),
    ] {
        assert_eq!(decode_daemon_message(body), Err(MessageError::UnknownType));
    }
    let tags = daemon_message_wire_tags();
    assert!(!tags.contains(&"output"));
    assert!(!tags.contains(&"resync"));
    assert!(tags.contains(&"frame"));
    assert!(tags.contains(&"selection_text"));
}

// --- the frame ---------------------------------------------------------

fn sample_frame() -> Frame {
    Frame {
        seq: 7,
        cols: 10,
        rows: 3,
        reset: true,
        last: false,
        attrs: vec![
            CellAttr::default(),
            CellAttr {
                fg: CellColor::Indexed(1),
                bg: CellColor::Default,
                flags: BTreeSet::from([CellFlag::Bold, CellFlag::Underline]),
            },
            CellAttr {
                fg: CellColor::Default,
                bg: CellColor::Rgb([10, 20, 30]),
                flags: BTreeSet::from([CellFlag::Selected]),
            },
        ],
        lines: vec![
            FrameLine {
                row: 0,
                wrapped: true,
                runs: vec![
                    FrameRun {
                        attr: 0,
                        cells: vec!["a".to_string(), "b".to_string()],
                        wide: Vec::new(),
                    },
                    FrameRun {
                        attr: 1,
                        cells: vec!["\u{5168}".to_string(), "e\u{301}".to_string()],
                        wide: vec![0],
                    },
                ],
            },
            FrameLine {
                row: 2,
                wrapped: false,
                runs: vec![FrameRun {
                    attr: 2,
                    cells: vec!["x".to_string()],
                    wide: Vec::new(),
                }],
            },
        ],
        cursor: CursorState {
            row: 1,
            col: 4,
            visible: true,
            shape: CursorShape::Bar,
        },
        view: ViewState {
            display_offset: 3,
            history_rows: 120,
            mouse_reporting: true,
            alt_screen: false,
        },
        overrides: vec![
            ColorOverride {
                slot: PaletteSlot::Background,
                rgb: [1, 2, 3],
            },
            ColorOverride {
                slot: PaletteSlot::Indexed(4),
                rgb: [4, 5, 6],
            },
        ],
    }
}

#[test]
fn a_frame_part_has_the_documented_json_shape() {
    let msg = DaemonMessage::Frame(sample_frame());
    assert_eq!(
        daemon_round_trip(&msg),
        json!({
            "type": "frame",
            "seq": 7,
            "cols": 10,
            "rows": 3,
            "reset": true,
            "last": false,
            "attrs": [
                { "fg": "default", "bg": "default" },
                { "fg": { "indexed": 1 }, "bg": "default", "flags": ["bold", "underline"] },
                { "fg": "default", "bg": { "rgb": [10, 20, 30] }, "flags": ["selected"] }
            ],
            "lines": [
                {
                    "row": 0,
                    "wrapped": true,
                    "runs": [
                        { "attr": 0, "cells": ["a", "b"] },
                        { "attr": 1, "cells": ["\u{5168}", "e\u{301}"], "wide": [0] }
                    ]
                },
                { "row": 2, "runs": [ { "attr": 2, "cells": ["x"] } ] }
            ],
            "cursor": { "row": 1, "col": 4, "visible": true, "shape": "bar" },
            "view": {
                "display_offset": 3,
                "history_rows": 120,
                "mouse_reporting": true,
                "alt_screen": false
            },
            "overrides": [
                { "slot": "background", "rgb": [1, 2, 3] },
                { "slot": { "indexed": 4 }, "rgb": [4, 5, 6] }
            ]
        })
    );
}

#[test]
fn a_frame_without_overrides_omits_the_key_and_decodes_without_it() {
    let mut frame = sample_frame();
    frame.overrides.clear();
    let value = daemon_round_trip(&DaemonMessage::Frame(frame));
    assert!(value.get("overrides").is_none(), "{value}");
}

#[test]
fn a_frame_part_that_misses_a_required_field_is_invalid_json() {
    let mut value = serde_json::to_value(DaemonMessage::Frame(sample_frame())).unwrap();
    for field in [
        "seq", "cols", "rows", "reset", "last", "attrs", "lines", "cursor", "view",
    ] {
        let mut broken = value.clone();
        broken.as_object_mut().unwrap().remove(field);
        assert_eq!(
            decode_daemon_message(&serde_json::to_vec(&broken).unwrap()),
            Err(MessageError::InvalidJson),
            "a frame without {field:?}"
        );
    }
    value["cursor"]["shape"] = json!("sparkles");
    assert_eq!(
        decode_daemon_message(&serde_json::to_vec(&value).unwrap()),
        Err(MessageError::InvalidJson),
        "an unknown cursor shape"
    );
}

#[test]
fn the_frame_budget_fits_the_frame_limit_with_room_for_the_envelope() {
    assert_eq!(FRAME_BUDGET_BYTES, 8 * 1024 * 1024);
    const { assert!(FRAME_BUDGET_BYTES < MAX_FRAME_BODY_LEN) };
}

// --- the other new messages -------------------------------------------

#[test]
fn frame_ack_carries_the_frame_number() {
    assert_eq!(
        client_round_trip(&ClientMessage::FrameAck { seq: 7 }),
        json!({ "type": "frame_ack", "seq": 7 })
    );
}

#[test]
fn set_palette_carries_the_theme_colors_flat() {
    let msg = ClientMessage::SetPalette(Palette {
        foreground: [0xc0, 0xc1, 0xc2],
        background: [0x1e, 0x1f, 0x2e],
        cursor: [0xff, 0xee, 0xdd],
        ansi: vec![[0, 0, 0], [1, 2, 3]],
    });
    assert_eq!(
        client_round_trip(&msg),
        json!({
            "type": "set_palette",
            "foreground": [192, 193, 194],
            "background": [30, 31, 46],
            "cursor": [255, 238, 221],
            "ansi": [[0, 0, 0], [1, 2, 3]]
        })
    );
}

#[test]
fn copy_selection_has_no_fields_and_is_answered_by_selection_text() {
    assert_eq!(
        client_round_trip(&ClientMessage::CopySelection),
        json!({ "type": "copy_selection" })
    );
    assert_eq!(
        daemon_round_trip(&DaemonMessage::SelectionText {
            text: Some("one\ntwo".to_string())
        }),
        json!({ "type": "selection_text", "text": "one\ntwo" })
    );
    assert_eq!(
        daemon_round_trip(&DaemonMessage::SelectionText { text: None }),
        json!({ "type": "selection_text", "text": null })
    );
}

#[test]
fn clear_selection_has_no_fields() {
    assert_eq!(
        client_round_trip(&ClientMessage::ClearSelection),
        json!({ "type": "clear_selection" })
    );
}

#[test]
fn scroll_view_carries_signed_lines_and_positive_goes_back_into_history() {
    for lines in [5, -5, 0, i32::MAX, i32::MIN] {
        assert_eq!(
            client_round_trip(&ClientMessage::ScrollView { lines }),
            json!({ "type": "scroll_view", "lines": lines })
        );
    }
}

// --- revisions ---------------------------------------------------------

#[test]
fn the_view_messages_need_the_frame_revision_and_this_build_speaks_it() {
    let palette = Palette {
        foreground: [0; 3],
        background: [0; 3],
        cursor: [0; 3],
        ansi: Vec::new(),
    };
    let messages = [
        ClientMessage::AttachView {
            window_id: "w1".to_string(),
            size: None,
        },
        ClientMessage::FrameAck { seq: 1 },
        ClientMessage::SetPalette(palette),
        ClientMessage::CopySelection,
        ClientMessage::ClearSelection,
        ClientMessage::ScrollView { lines: 1 },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            FRAME_MIN_REVISION,
            "{msg:?}"
        );
    }
    assert_eq!(FRAME_MIN_REVISION, 19);
}

// --- the built-in palette ---------------------------------------------

/// The daemon's built-in palette is the GUI's built-in dark theme, read from
/// the one file both take it from, color by color.
#[test]
fn the_built_in_dark_palette_is_the_dark_theme_of_the_shared_file() {
    let file: Value = serde_json::from_str(include_str!("../built-in-palettes.json")).unwrap();
    let dark = &file["dark"];
    let rgb = |token: &str| -> [u8; 3] {
        let hex = dark[token].as_str().unwrap().trim_start_matches('#');
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap();
        [channel(0), channel(2), channel(4)]
    };
    let ansi: Vec<[u8; 3]> = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ]
    .iter()
    .map(|name| format!("term-ansi-{name}"))
    .chain(
        [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ]
        .iter()
        .map(|name| format!("term-ansi-bright-{name}")),
    )
    .map(|token| rgb(&token))
    .collect();

    assert_eq!(
        yagura_protocol::built_in_dark_palette(),
        Palette {
            foreground: rgb("color-foreground"),
            background: rgb("color-background"),
            cursor: rgb("color-primary"),
            ansi,
        }
    );
}
