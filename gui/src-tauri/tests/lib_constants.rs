//! The connection budget after the GUI spawns the daemon, measured against the
//! constants `gui/src-tauri` exposes as a public library item.

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
