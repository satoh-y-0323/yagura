//! Where a holder lives: its instance's `holders` directory, the endpoint it
//! listens on, and the daemon endpoint whose owner it serves.

use std::path::PathBuf;

use yagura_os::{InstanceNamespace, IpcTransport, OsError, PeerAuth, PlatformIpc};

/// The places of one window's holder.
pub(crate) struct Place {
    /// The state base directory, never narrowed itself.
    pub(crate) state_base: PathBuf,
    /// `holders` in the instance's state directory: the record, the exit file
    /// and, on Unix, the socket.
    pub(crate) holders_dir: PathBuf,
    /// The endpoint this holder listens on.
    pub(crate) endpoint: String,
    /// The instance's daemon endpoint.
    pub(crate) daemon_endpoint: String,
    /// The user this holder runs as.
    pub(crate) user_id: String,
}

/// Resolves and creates (owner-only) the places of `window_id`'s holder.
pub(crate) fn resolve(window_id: &str, namespace: &InstanceNamespace) -> Result<Place, String> {
    let ipc = PlatformIpc::default();
    let user_id = ipc
        .current_user_id()
        .map_err(|error| format!("cannot identify the current user: {error}"))?;
    let state_base = yagura_os::default_state_base_dir()
        .ok_or_else(|| "no state directory is configured for this user".to_owned())?;
    let holders_dir = yagura_os::instance_dir(&state_base, namespace).join("holders");
    yagura_os::secure_owned_dir(&state_base, &holders_dir)?;
    std::fs::create_dir_all(&holders_dir)
        .map_err(|error| format!("cannot create {}: {error}", holders_dir.display()))?;
    let endpoint = yagura_os::holder_endpoint(&user_id, namespace, &holders_dir, window_id)
        .map_err(|error| error.to_string())?;
    let daemon_endpoint = yagura_os::default_endpoint(&user_id, namespace);
    Ok(Place {
        state_base,
        holders_dir,
        endpoint,
        daemon_endpoint,
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

/// Removes the socket file a Unix holder leaves when it ends; a pipe leaves
/// nothing behind.
pub(crate) fn remove_endpoint_file(place: &Place) {
    if cfg!(unix) {
        let _ = std::fs::remove_file(&place.endpoint);
    }
}
