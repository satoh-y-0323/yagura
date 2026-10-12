//! Confirms that gui/src-tauri exposes its fail-safe constants as a public
//! library item, reachable from a crate outside `gui/src-tauri/src`.

use std::time::Duration;

#[test]
fn connect_retry_interval_is_two_hundred_milliseconds() {
    assert_eq!(
        yagura_gui::constants::CONNECT_RETRY_INTERVAL,
        Duration::from_millis(200)
    );
}

#[test]
fn connect_retry_limit_is_twenty_five() {
    assert_eq!(yagura_gui::constants::CONNECT_RETRY_LIMIT, 25);
}

#[test]
fn attach_timeout_is_five_seconds() {
    assert_eq!(
        yagura_gui::constants::ATTACH_TIMEOUT,
        Duration::from_secs(5)
    );
}

#[test]
fn reconnect_attempt_limit_is_one() {
    assert_eq!(yagura_gui::constants::RECONNECT_ATTEMPT_LIMIT, 1);
}

#[test]
fn min_pty_size_is_two_columns_by_one_row() {
    assert_eq!(yagura_gui::constants::MIN_PTY_COLS, 2u16);
    assert_eq!(yagura_gui::constants::MIN_PTY_ROWS, 1u16);
}

#[test]
fn max_pty_dimension_is_one_thousand() {
    assert_eq!(yagura_gui::constants::MAX_PTY_DIMENSION, 1000u16);
}

#[test]
fn read_chunk_len_is_eight_kibibytes() {
    assert_eq!(yagura_gui::constants::READ_CHUNK_LEN, 8192);
}

#[test]
fn window_list_poll_interval_is_two_seconds() {
    assert_eq!(
        yagura_gui::constants::WINDOW_LIST_POLL_INTERVAL,
        Duration::from_secs(2)
    );
}

// The daemon reads the login shell's environment before it binds its
// endpoint, so the wait right after a spawn must outlast that read on top of
// the ordinary connection budget. Each attempt is preceded by one interval
// (the first by the wait before the first attempt).
#[test]
fn post_spawn_connect_budget_covers_login_env_read_and_ordinary_budget() {
    use yagura_gui::constants::{
        CONNECT_RETRY_INTERVAL, CONNECT_RETRY_LIMIT, POST_SPAWN_CONNECT_RETRY_LIMIT,
    };
    let covered = CONNECT_RETRY_INTERVAL * POST_SPAWN_CONNECT_RETRY_LIMIT;
    let required = yagura_os::LOGIN_ENV_READ_TIMEOUT + CONNECT_RETRY_INTERVAL * CONNECT_RETRY_LIMIT;
    assert!(
        covered >= required,
        "post-spawn connection attempts cover {covered:?}, less than the login environment read plus the ordinary budget ({required:?})"
    );
}

// A failed start is reported only once this budget runs out, so it must not
// outlast the login environment read and the ordinary budget by a whole attempt.
#[test]
fn post_spawn_connect_budget_is_the_fewest_attempts_that_cover_the_wait() {
    use yagura_gui::constants::{
        CONNECT_RETRY_INTERVAL, CONNECT_RETRY_LIMIT, POST_SPAWN_CONNECT_RETRY_LIMIT,
    };
    let one_fewer = CONNECT_RETRY_INTERVAL * (POST_SPAWN_CONNECT_RETRY_LIMIT - 1);
    let required = yagura_os::LOGIN_ENV_READ_TIMEOUT + CONNECT_RETRY_INTERVAL * CONNECT_RETRY_LIMIT;
    assert!(
        one_fewer < required,
        "one attempt fewer still covers {one_fewer:?}, at least the {required:?} required"
    );
}
