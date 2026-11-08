//! Entry point of the `yagura-watcher` executable: the watcher a Unix holder
//! starts beside itself for each window (`yagura_holder::watcher`).

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    yagura_holder::watcher::run()
}

/// A Windows window's tree is held by its job object; no watcher runs there.
#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(2)
}
