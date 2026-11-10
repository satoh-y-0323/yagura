//! `yagura-mcp`: the relay binary a CLI starts as its stdio MCP server.
//!
//! It resolves which daemon to reach and which window it runs in, then hands
//! its stdin and stdout to [`yagura_mcp::relay::run`]. The daemon is reached
//! through the operating-system boundary ([`yagura_os`]), so this binary has a
//! single `main` and no per-platform build of its own.

use std::process::ExitCode;

use yagura_mcp::relay::{self, RelayEnd};
use yagura_os::{PeerAuth, PlatformIpc, default_endpoint, resolve_instance_from};
use yagura_protocol::{INSTANCE_ENV, process_window_credentials};

fn main() -> ExitCode {
    // Resolved before anything derived from it: the endpoint name carries the
    // namespace. A rejected identifier stops the relay here rather than
    // letting it fall back to the default instance's daemon. The store-path
    // override conflict is not checked: this binary never opens the store.
    let namespace = match resolve_instance_from(std::env::var_os(INSTANCE_ENV).as_deref()) {
        Ok(namespace) => namespace,
        Err(err) => {
            eprintln!("yagura-mcp: failed to resolve {INSTANCE_ENV}: {err}");
            return ExitCode::FAILURE;
        }
    };
    // Fail-closed: without the user identifier there is no endpoint name to
    // reach the daemon on, so the relay stops rather than guessing one.
    let user_id = match PlatformIpc::default().current_user_id() {
        Ok(user_id) => user_id,
        Err(err) => {
            eprintln!("yagura-mcp: failed to resolve the current user identifier: {err}");
            return ExitCode::FAILURE;
        }
    };
    // The relay is a client, so it never creates the Unix runtime directory;
    // only the daemon does.
    let endpoint = default_endpoint(&user_id, &namespace);
    // The same decision the hook doors make: a partial or empty environment
    // pair is no claim, and `--credentials <path>` names the window otherwise.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let window = process_window_credentials(&args);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("yagura-mcp: failed to start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    let end = runtime.block_on(relay::run(
        endpoint,
        window,
        tokio::io::stdin(),
        tokio::io::stdout(),
    ));
    // The stdin read may still be blocked in its thread; waiting for it would
    // keep a relay whose session ended alive until the CLI writes again.
    runtime.shutdown_background();
    match end {
        RelayEnd::InputClosed => ExitCode::SUCCESS,
        RelayEnd::OutputClosed | RelayEnd::SessionMismatch => ExitCode::FAILURE,
    }
}
