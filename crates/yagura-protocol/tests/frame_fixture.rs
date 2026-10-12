#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The frame sample the GUI's TypeScript applier is tested against.
//!
//! `tests/fixtures/frame-sample.json` is an external contract between this
//! crate (which serializes frames) and the renderer (which reads them): the
//! names, the shapes and the way parts are applied. It holds `steps`; each has
//! the `messages` (daemon messages, as they travel) to apply in order to one
//! grid that persists across steps, and the grid they must leave:
//!
//! - `expected.screen_text`: every row's cells joined (a wide cell once) with
//!   trailing spaces removed;
//! - `expected.cells`: the cells worth checking, each with `row`, `col`,
//!   `text`, `width` (2 for a wide cell, 0 for the spacer behind it, else 1),
//!   `fg`, `bg` and `flags`; a cell the frame did not name is a blank one:
//!   text `" "`, width 1, default colors, no flags;
//! - `expected.wrapped_rows`: the rows that go on in the next row.
//!
//! The ordinary test below proves the file is what the types serialize today.
//! `write_frame_sample` writes it; run it by hand when the contract changes:
//! `FRAME_SAMPLE_WRITE=1 cargo test -p yagura-protocol --test frame_fixture -- --ignored`.
//! The reference applier in `yagura-test-support` (`frame::Grid`) is held to the
//! same file by its own test.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use yagura_protocol::{DaemonMessage, decode_daemon_message};

fn sample_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frame-sample.json")
}

/// The sample, authored here as JSON so that the file is exactly what a
/// renderer will see on the wire.
fn sample() -> Value {
    let default_attr = json!({ "fg": "default", "bg": "default" });
    let flagged = json!({
        "fg": "default",
        "bg": { "rgb": [10, 20, 30] },
        "flags": ["underline", "selected"]
    });
    let view = |display_offset: u32, history_rows: u32, mouse: bool, alt: bool| {
        json!({
            "display_offset": display_offset,
            "history_rows": history_rows,
            "mouse_reporting": mouse,
            "alt_screen": alt
        })
    };

    json!({
        "description": "Steps of frames applied in order to one persistent grid; see tests/frame_fixture.rs",
        "steps": [
            {
                "name": "a full frame in two parts: wide and combining cells, attribute tables per part",
                "messages": [
                    {
                        "type": "frame", "seq": 1, "cols": 10, "rows": 3,
                        "reset": true, "last": false,
                        "attrs": [
                            default_attr,
                            { "fg": { "indexed": 1 }, "bg": "default", "flags": ["bold"] }
                        ],
                        "lines": [
                            {
                                "row": 0, "wrapped": true,
                                "runs": [
                                    { "attr": 0, "cells": ["a", "b"] },
                                    { "attr": 1, "cells": ["\u{5168}"], "wide": [0] },
                                    { "attr": 0, "cells": ["e\u{301}"] }
                                ]
                            }
                        ],
                        "cursor": { "row": 0, "col": 5, "visible": true, "shape": "block" },
                        "view": view(0, 0, false, false)
                    },
                    {
                        "type": "frame", "seq": 1, "cols": 10, "rows": 3,
                        "reset": false, "last": true,
                        "attrs": [default_attr, flagged],
                        "lines": [
                            { "row": 2, "runs": [ { "attr": 1, "cells": ["x", "y"] } ] }
                        ],
                        "cursor": { "row": 1, "col": 0, "visible": true, "shape": "block" },
                        "view": view(0, 0, false, false)
                    }
                ],
                "expected": {
                    "cols": 10, "rows": 3,
                    "screen_text": ["ab\u{5168}e\u{301}", "", "xy"],
                    "wrapped_rows": [0],
                    "cursor": { "row": 1, "col": 0, "visible": true, "shape": "block" },
                    "view": view(0, 0, false, false),
                    "overrides": [],
                    "cells": [
                        { "row": 0, "col": 0, "text": "a", "width": 1, "fg": "default", "bg": "default", "flags": [] },
                        { "row": 0, "col": 2, "text": "\u{5168}", "width": 2, "fg": { "indexed": 1 }, "bg": "default", "flags": ["bold"] },
                        { "row": 0, "col": 3, "text": "", "width": 0, "fg": { "indexed": 1 }, "bg": "default", "flags": ["bold"] },
                        { "row": 0, "col": 4, "text": "e\u{301}", "width": 1, "fg": "default", "bg": "default", "flags": [] },
                        { "row": 0, "col": 5, "text": " ", "width": 1, "fg": "default", "bg": "default", "flags": [] },
                        { "row": 2, "col": 0, "text": "x", "width": 1, "fg": "default", "bg": { "rgb": [10, 20, 30] }, "flags": ["underline", "selected"] },
                        { "row": 2, "col": 1, "text": "y", "width": 1, "fg": "default", "bg": { "rgb": [10, 20, 30] }, "flags": ["underline", "selected"] },
                        { "row": 2, "col": 9, "text": " ", "width": 1, "fg": "default", "bg": "default", "flags": [] }
                    ]
                }
            },
            {
                "name": "a diff frame replaces one row and the state around the cells",
                "messages": [
                    {
                        "type": "frame", "seq": 2, "cols": 10, "rows": 3,
                        "reset": false, "last": true,
                        "attrs": [default_attr],
                        "lines": [ { "row": 1, "runs": [ { "attr": 0, "cells": ["z"] } ] } ],
                        "cursor": { "row": 1, "col": 1, "visible": false, "shape": "underline" },
                        "view": view(2, 5, true, true),
                        "overrides": [
                            { "slot": "background", "rgb": [1, 2, 3] },
                            { "slot": { "indexed": 4 }, "rgb": [4, 5, 6] }
                        ]
                    }
                ],
                "expected": {
                    "cols": 10, "rows": 3,
                    "screen_text": ["ab\u{5168}e\u{301}", "z", "xy"],
                    "wrapped_rows": [0],
                    "cursor": { "row": 1, "col": 1, "visible": false, "shape": "underline" },
                    "view": view(2, 5, true, true),
                    "overrides": [
                        { "slot": "background", "rgb": [1, 2, 3] },
                        { "slot": { "indexed": 4 }, "rgb": [4, 5, 6] }
                    ],
                    "cells": [
                        { "row": 0, "col": 2, "text": "\u{5168}", "width": 2, "fg": { "indexed": 1 }, "bg": "default", "flags": ["bold"] },
                        { "row": 1, "col": 0, "text": "z", "width": 1, "fg": "default", "bg": "default", "flags": [] },
                        { "row": 2, "col": 0, "text": "x", "width": 1, "fg": "default", "bg": { "rgb": [10, 20, 30] }, "flags": ["underline", "selected"] }
                    ]
                }
            },
            {
                "name": "a reset frame of another size replaces the whole grid and drops the overrides",
                "messages": [
                    {
                        "type": "frame", "seq": 3, "cols": 4, "rows": 2,
                        "reset": true, "last": true,
                        "attrs": [default_attr],
                        "lines": [ { "row": 0, "runs": [ { "attr": 0, "cells": ["h", "i"] } ] } ],
                        "cursor": { "row": 0, "col": 2, "visible": true, "shape": "bar" },
                        "view": view(0, 0, false, false)
                    }
                ],
                "expected": {
                    "cols": 4, "rows": 2,
                    "screen_text": ["hi", ""],
                    "wrapped_rows": [],
                    "cursor": { "row": 0, "col": 2, "visible": true, "shape": "bar" },
                    "view": view(0, 0, false, false),
                    "overrides": [],
                    "cells": [
                        { "row": 0, "col": 0, "text": "h", "width": 1, "fg": "default", "bg": "default", "flags": [] },
                        { "row": 1, "col": 3, "text": " ", "width": 1, "fg": "default", "bg": "default", "flags": [] }
                    ]
                }
            }
        ]
    })
}

#[test]
fn the_sample_file_is_what_the_types_serialize_today() {
    let path = sample_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{path:?} must be readable: {err}"));
    let file: Value = serde_json::from_str(&text).expect("the sample is JSON");
    assert_eq!(
        file,
        sample(),
        "tests/fixtures/frame-sample.json is out of step with tests/frame_fixture.rs; \
         regenerate it by hand if the wire contract changed on purpose"
    );
}

#[test]
fn every_message_in_the_sample_decodes_and_encodes_back_to_itself() {
    for step in sample()["steps"].as_array().unwrap() {
        for message in step["messages"].as_array().unwrap() {
            let body = serde_json::to_vec(message).unwrap();
            let decoded = decode_daemon_message(&body)
                .unwrap_or_else(|err| panic!("{message} must decode: {err:?}"));
            assert!(matches!(decoded, DaemonMessage::Frame(_)), "{message}");
            let encoded: Value = serde_json::to_value(&decoded).unwrap();
            assert_eq!(
                &encoded, message,
                "a frame the sample holds must be what the types write for it"
            );
        }
    }
}

#[test]
#[ignore = "writes tests/fixtures/frame-sample.json: run with FRAME_SAMPLE_WRITE=1"]
fn write_frame_sample() {
    assert_eq!(
        std::env::var("FRAME_SAMPLE_WRITE").as_deref(),
        Ok("1"),
        "set FRAME_SAMPLE_WRITE=1 to run this; it is not part of the normal test suite"
    );
    let path = sample_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = serde_json::to_string_pretty(&sample()).unwrap();
    text.push('\n');
    std::fs::write(&path, text).unwrap_or_else(|err| panic!("{path:?} must be writable: {err}"));
}
