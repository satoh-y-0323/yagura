//! Where a holder lives: the `holders` directory its launch names, the
//! endpoint it listens on, and the daemon endpoint whose server it serves.

use std::path::PathBuf;

use yagura_os::{InstanceNamespace, IpcTransport, OsError, PeerAuth, PlatformIpc};
use yagura_protocol::holder::HolderLaunch;

/// The places of one window's holder.
pub(crate) struct Place {
    /// The record and, on Unix, the socket.
    pub(crate) holders_dir: PathBuf,
    /// The endpoint this holder listens on.
    pub(crate) endpoint: String,
    /// The daemon endpoint the launch names.
    pub(crate) daemon_endpoint: String,
    /// The user this holder runs as.
    pub(crate) user_id: String,
}

/// Checks the launch's `holders` directory is private to this user (creating
/// it when only it is missing) and names the endpoint of `window_id`'s holder.
pub(crate) fn resolve(
    window_id: &str,
    namespace: &InstanceNamespace,
    launch: &HolderLaunch,
) -> Result<Place, String> {
    let ipc = PlatformIpc::default();
    let user_id = ipc
        .current_user_id()
        .map_err(|error| format!("cannot identify the current user: {error}"))?;
    let holders_dir = launch.holders_dir.clone();
    yagura_os::ensure_private_dir(&holders_dir).map_err(|error| {
        format!(
            "the holders directory {} is not usable: {error}",
            holders_dir.display()
        )
    })?;
    let endpoint = yagura_os::holder_endpoint(&user_id, namespace, &holders_dir, window_id)
        .map_err(|error| error.to_string())?;
    Ok(Place {
        holders_dir,
        endpoint,
        daemon_endpoint: launch.daemon_endpoint.clone(),
        user_id,
    })
}

/// Claims the holder's endpoint for the current user only (create-first).
pub(crate) async fn bind(
    place: &Place,
) -> Result<<PlatformIpc as IpcTransport>::Listener, OsError> {
    let ipc = PlatformIpc::default();
    let security = ipc.endpoint_security()?;
    ipc.bind(&place.endpoint, security).await
}
