#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `VtScreen::feed` / `VtScreen::resize` report screen integrity via `ScreenIntegrity`.
//! Kept apart from `tests/detector_failsafe.rs`, which states its contracts without
//! naming that type.

use yagura_daemon::detector::{ScreenIntegrity, VtScreen};

/// Screen-corruption stimulus: fed twice before a degenerate resize.
const RESIZE_CHUNK: &[u8] = b"redrawn screen chunk";

#[test]
fn canonical_recovery_reports_rebuilt_and_a_plain_call_reports_intact() {
    let mut screen = VtScreen::new(80, 24);
    assert_eq!(screen.feed(RESIZE_CHUNK), ScreenIntegrity::Intact);
    assert_eq!(screen.feed(RESIZE_CHUNK), ScreenIntegrity::Intact);
    assert_eq!(screen.resize(46, 1), ScreenIntegrity::Intact);
    screen.poison_after_calls(0);
    assert_eq!(
        screen.feed(RESIZE_CHUNK),
        ScreenIntegrity::Rebuilt,
        "the canonical resize-then-feed sequence must report Rebuilt once recovery runs"
    );

    let mut plain = VtScreen::new(80, 24);
    assert_eq!(
        plain.feed(b"ordinary output"),
        ScreenIntegrity::Intact,
        "an ordinary feed with no degenerate resize must report Intact"
    );
    assert_eq!(
        plain.resize(60, 20),
        ScreenIntegrity::Intact,
        "an ordinary resize with no panic condition must report Intact"
    );
}
