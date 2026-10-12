#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What the core hands the WebView when a window is drawn: the `attach`
//! command's result (the child's exit state only -- the screen follows as
//! frames, delivered by the `next_frames` command).

use yagura_gui::events::AttachResponse;

#[test]
fn the_attach_result_carries_the_exit_state_and_no_screen() {
    let value = serde_json::to_value(AttachResponse {
        child_exited: true,
        exit_code: Some(3),
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "child_exited": true, "exit_code": 3 })
    );
}
