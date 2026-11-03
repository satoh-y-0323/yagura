//! `yagura-holder`: the process that keeps one window's PTY and child alive
//! across daemon restarts. It does not interpret the screen.
//!
//! Started as `yagura-holder <window_id> <instance_id>`, with an empty
//! `<instance_id>` for the default instance; everything else, the child's
//! environment and the window's record included, arrives on standard input
//! ([`yagura_protocol::holder::encode_holder_launch`]), which is read to its
//! end. The holder then publishes its record in the instance's `holders`
//! directory, listens on the endpoint named there, starts the child once the
//! first daemon is let in, and ends when the child has exited.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::ExitCode;

use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderLaunch, HolderRecord, decode_holder_launch, is_plain_window_id,
};
use yagura_protocol::window_size_within_limit;

mod buffer;
mod endpoint;
mod peer;
mod record;
mod session;

/// The most a launch may take on standard input.
const MAX_LAUNCH_BYTES: u64 = 1024 * 1024;

/// Runs the holder with the process's arguments and standard input.
#[must_use]
pub fn run() -> ExitCode {
    match start() {
        Ok(session::Ending::ChildExited | session::Ending::NoHello) => ExitCode::SUCCESS,
        Ok(session::Ending::SpawnFailed) => ExitCode::from(3),
        Err(error) => {
            log(format_args!("{error}"));
            ExitCode::from(2)
        }
    }
}

/// Writes one line to standard error. A closed or broken standard error loses
/// the line rather than ending the holder, and with it the child.
pub(crate) fn log(message: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr().lock(), "yagura-holder: {message}");
}

fn start() -> Result<session::Ending, String> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let [window_id, instance] = args.as_slice() else {
        return Err("usage: yagura-holder <window_id> <instance_id>".to_owned());
    };
    let window_id = window_id
        .to_str()
        .filter(|id| is_plain_window_id(id))
        .ok_or_else(|| "the window id is not a plain identifier".to_owned())?
        .to_owned();
    // An empty instance names the default instance, which has no identifier.
    let instance = (!instance.is_empty()).then_some(instance.as_os_str());
    let namespace =
        yagura_os::resolve_instance_from(instance).map_err(|error| error.to_string())?;
    let launch = read_launch()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the runtime: {error}"))?;
    let ending = runtime.block_on(async {
        let place = endpoint::resolve(&window_id, &namespace)?;
        let listener = endpoint::bind(&place)
            .await
            .map_err(|error| format!("cannot listen on the holder endpoint: {error}"))?;
        record::remove_stale_exit(&place, &window_id)
            .map_err(|error| format!("cannot remove an earlier exit file: {error}"))?;
        let record = HolderRecord {
            window_id: window_id.clone(),
            endpoint: place.endpoint.clone(),
            holder_pid: std::process::id(),
            child_pid: None,
            protocol: HOLDER_PROTOCOL,
        };
        record::write_record(&place, &record)
            .map_err(|error| format!("cannot write the holder record: {error}"))?;
        let window = session::Window {
            window_id,
            launch,
            place,
            record,
        };
        Ok::<_, String>(session::serve(window, listener).await)
    });
    // The terminal reader may still be blocked in a read nobody waits for.
    runtime.shutdown_background();
    ending
}

fn read_launch() -> Result<HolderLaunch, String> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_LAUNCH_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|error| format!("cannot read the launch from standard input: {error}"))?;
    if input.len() as u64 > MAX_LAUNCH_BYTES {
        return Err("the launch on standard input is too large".to_owned());
    }
    // The decoder's message may quote a value, and values here can be secrets.
    let launch = decode_holder_launch(&input)
        .map_err(|_| "the launch on standard input is not valid".to_owned())?;
    if launch.argv.is_empty() || !window_size_within_limit(launch.cols, launch.rows) {
        return Err("the launch names no program or a size out of range".to_owned());
    }
    Ok(launch)
}
