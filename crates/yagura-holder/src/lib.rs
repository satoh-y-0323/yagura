//! `yagura-holder`: the process that keeps one window's PTY and child alive
//! across daemon restarts. It does not interpret the screen.
//!
//! Started as `yagura-holder <window_id> <instance_id>`, with an empty
//! `<instance_id>` for the default instance; everything else, the child's
//! environment and the window's record included, arrives on standard input
//! ([`yagura_protocol::holder::encode_holder_launch`]), which is read to its
//! end. The holder's own environment is left as it was started with: it is
//! the base of the child's. The holder then listens on its endpoint,
//! publishes its record in the `holders` directory the launch names, writes
//! one line to standard output and closes it (a holder that fails before
//! this writes nothing there), starts the child once the first daemon is let
//! in, and ends when the child has exited.

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

/// The line on standard output that tells the starter the holder is ready.
const READY_NOTICE: &[u8] = b"yagura-holder ready\n";

/// Writes one line to standard error in one write, so lines of holders
/// appending to the same log never interleave. A closed or broken standard
/// error loses the line rather than ending the holder, and with it the child.
pub(crate) fn log(message: std::fmt::Arguments<'_>) {
    let line = format!("yagura-holder: {message}\n");
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
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
        let place = endpoint::resolve(&window_id, &namespace, &launch)?;
        let listener = endpoint::bind(&place)
            .await
            .map_err(|error| format!("cannot listen on the holder endpoint: {error}"))?;
        let record = HolderRecord {
            window_id: window_id.clone(),
            endpoint: place.endpoint.clone(),
            holder_pid: std::process::id(),
            child_pid: None,
            protocol: HOLDER_PROTOCOL,
        };
        if let Err(error) = publish(&place, &record) {
            // The endpoint is still this holder's, so its socket file is too.
            record::remove_record(&place, &window_id);
            endpoint::remove_endpoint_file(&place);
            return Err(error);
        }
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

/// Publishes the holder once it owns its endpoint: clears an earlier exit
/// file, writes the record, then tells the starter on standard output and
/// lets go of it.
fn publish(place: &endpoint::Place, record: &HolderRecord) -> Result<(), String> {
    record::remove_stale_exit(place, &record.window_id)
        .map_err(|error| format!("cannot remove an earlier exit file: {error}"))?;
    record::write_record(place, record)
        .map_err(|error| format!("cannot write the holder record: {error}"))?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(READY_NOTICE)
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("cannot tell the starter the holder is ready: {error}"))?;
    drop(stdout);
    if let Err(error) = yagura_os::release_standard_output() {
        log(format_args!("cannot close standard output: {error}"));
    }
    Ok(())
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
    // A relative directory would depend on where the holder was started.
    if !launch.holders_dir.is_absolute() || launch.daemon_endpoint.is_empty() {
        return Err(
            "the launch names no absolute holders directory or no daemon endpoint".to_owned(),
        );
    }
    Ok(launch)
}
