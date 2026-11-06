//! `yagura-holder`: the process that keeps one window's PTY, child and
//! terminal alive across daemon restarts. It feeds the child's output to the
//! window's terminal model and sends the daemon copies of the screen and the
//! frames a client is shown; the raw output never leaves it.
//!
//! Started as `yagura-holder <window_id> <instance_id>`, with an empty
//! `<instance_id>` for the default instance; everything else, the child's
//! environment and the window's record included, arrives on standard input
//! ([`yagura_protocol::holder::encode_holder_launch`]), which is read to its
//! end. The holder's own environment is left as it was started with: it is
//! the base of the child's. The holder then listens on its endpoint,
//! publishes its record in the `holders` directory the launch names, writes
//! one line to standard output and closes it (a holder that fails before
//! this writes nothing there), and starts the child once the first daemon is
//! let in. It lives as long as the window, past the child's exit: it ends on
//! `Terminate`, when no daemon comes by the first `Hello`'s deadline, when the
//! child cannot start, or when a connection is lost and no next daemon will
//! come.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;

use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderLaunch, HolderRecord, decode_holder_launch, is_plain_window_id,
};
use yagura_protocol::window_size_within_limit;

mod buffer;
mod conn;
mod endpoint;
mod peer;
mod push;
mod record;
mod session;
mod terminal;
mod view;

/// The most a launch may take on standard input.
const MAX_LAUNCH_BYTES: u64 = 1024 * 1024;

/// Runs the holder with the process's arguments and standard input. A launch
/// that names a fault is refused as a child that could not start.
#[must_use]
pub fn run() -> ExitCode {
    run_with(session::Faults::Refused)
}

/// Runs the holder as [`run`] does, but acting out the fault its launch
/// names. For the fault-injecting binary (`holder_fault`) that tests start;
/// the shipped binary never calls it.
#[must_use]
pub fn run_fault_injection() -> ExitCode {
    run_with(session::Faults::ActedOut)
}

fn run_with(faults: session::Faults) -> ExitCode {
    match start(faults) {
        Ok(session::Ending::WindowClosed | session::Ending::NoHello) => ExitCode::SUCCESS,
        Ok(session::Ending::SpawnFailed) => ExitCode::from(3),
        Err(error) => {
            log(format_args!("{error}"));
            ExitCode::from(2)
        }
    }
}

/// The line on standard output that tells the starter the holder is ready.
const READY_NOTICE: &[u8] = b"yagura-holder ready\n";

/// The log the launch names, once the launch is read.
static LOG_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Writes one line in one write, so lines of holders appending to the same
/// log never interleave: to the launch's log, opened for appending for this
/// line alone so the holder never keeps the daemon's log open, or to standard
/// error before the launch is read or when it names no log. A missing log or
/// a broken standard error loses the line rather than ending the holder, and
/// with it the child. Control characters in `message` are escaped: it can
/// carry a path or a program name, and the log it lands in is the daemon's,
/// read line by line.
pub(crate) fn log(message: std::fmt::Arguments<'_>) {
    let line = log_line(&message.to_string());
    match LOG_FILE.get() {
        Some(path) => {
            if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) {
                let _ = file.write_all(line.as_bytes());
            }
        }
        None => {
            let _ = std::io::stderr().lock().write_all(line.as_bytes());
        }
    }
}

/// Sends the holder's messages, a panic's included, to `log_file` from now
/// on, when the launch names one.
fn log_to(log_file: Option<PathBuf>) {
    if let Some(path) = log_file {
        let _ = LOG_FILE.set(path);
        std::panic::set_hook(Box::new(|panic| log(format_args!("{panic}"))));
    }
}

fn log_line(message: &str) -> String {
    let mut line = String::from("yagura-holder: ");
    for c in message.chars() {
        if c.is_control() {
            line.extend(c.escape_default());
        } else {
            line.push(c);
        }
    }
    line.push('\n');
    line
}

fn start(faults: session::Faults) -> Result<session::Ending, String> {
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
    log_to(launch.log_file.clone());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the runtime: {error}"))?;
    let ending = runtime.block_on(async {
        let place = endpoint::resolve(&window_id, &namespace, &launch)?;
        let listener = endpoint::bind(&place)
            .await
            .map_err(|error| format!("cannot listen on the holder endpoint: {error}"))?;
        let holder_pid = std::process::id();
        let mut record = record::RecordFile::new(
            &place,
            HolderRecord {
                window_id,
                endpoint: place.endpoint.clone(),
                holder_pid,
                child_pid: None,
                protocol: HOLDER_PROTOCOL,
                holder_start: record::start_of(holder_pid),
                child_start: None,
            },
        );
        if let Err(error) = publish(&record) {
            record.remove();
            return Err(error);
        }
        let window = session::Window {
            succession: session::Succession::new(&launch),
            launch,
            place,
            record,
            faults,
        };
        Ok::<_, String>(session::serve(window, listener).await)
    });
    // The terminal reader may still be blocked in a read nobody waits for.
    runtime.shutdown_background();
    ending
}

/// Publishes the holder once it owns its endpoint: writes the record, then
/// tells the starter on standard output and lets go of it.
fn publish(record: &record::RecordFile) -> Result<(), String> {
    record
        .write()
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

#[cfg(test)]
mod tests {
    use super::log_line;

    // A path or program name in the message must not end the line early or
    // drive the terminal of whoever reads the daemon's log.
    #[test]
    fn a_log_line_carries_no_control_character_but_its_end_and_keeps_printable_text() {
        let printable = r"C:\work dir/é 日本 [31m";
        let message = format!("a\nforged line\r{printable}\u{1b}[31m\u{7}\t\u{85}\0");

        let line = log_line(&message);

        assert!(line.ends_with('\n'), "the line has no end: {line:?}");
        let body = &line[..line.len() - 1];
        assert!(
            !body.chars().any(char::is_control),
            "a control character passed through: {line:?}"
        );
        assert!(
            body.contains(printable),
            "printable text was changed: {line:?}"
        );
    }
}
