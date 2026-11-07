//! The context every window's holder is started in: the executable, the
//! endpoint whose server it lets in, the directory it keeps its files in,
//! the log it writes to, and the daemon run it is told of.
//!
//! One [`HolderHost`] serves every window a [`crate::WindowRegistry`] or a
//! caller of [`crate::PtySession`] opens. A daemon hands it its own endpoint
//! and its instance's `holders` directory ([`HolderPlace`]); without one, the
//! host makes a place of its own on its first open: an endpoint only it
//! serves and an owner-only directory under the OS temp directory, both gone
//! once the host and its holders are.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::{OnceCell, oneshot};
use yagura_os::{DetachedStderr, IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::holder::HolderScopes;

use crate::config::DaemonConfig;
use crate::pty::{PtyError, lock_recover};

/// The file stem of the holder executable shipped beside the daemon.
pub const HOLDER_EXE_STEM: &str = "yagura-holder";

/// How long a host without a [`HolderPlace`] waits, as it is dropped, for
/// its holders to end, so its directory is removed with them. Bounds a
/// teardown; nothing is synchronised on it.
const PRIVATE_PLACE_DRAIN_LIMIT: Duration = Duration::from_secs(10);

/// How long a connection to a private endpoint is kept open before it is
/// closed unread: a holder only asks the OS who serves it, then closes.
const PRIVATE_PEER_LIMIT: Duration = Duration::from_secs(10);

/// The pause before accepting again after a private endpoint failed to offer
/// a connection, so a broken endpoint does not spin.
const PRIVATE_ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// The name of the holder executable on this platform.
#[must_use]
pub fn holder_file_name() -> String {
    format!("{HOLDER_EXE_STEM}{}", std::env::consts::EXE_SUFFIX)
}

/// Where a daemon's holders keep their files and whom they let in.
#[derive(Debug, Clone)]
pub struct HolderPlace {
    /// The daemon's own endpoint: a holder lets in only the process serving it.
    pub daemon_endpoint: String,
    /// The instance's `holders` directory, spelled the one way every launch
    /// names it (a holder's endpoint name is derived from the spelling).
    pub holders_dir: PathBuf,
    /// The daemon's log, which each holder appends its messages to, opening
    /// it for each line so a holder that outlives the daemon keeps nothing of
    /// it open; `None` discards them.
    pub log_file: Option<PathBuf>,
}

/// Counts the links of a host that are still running.
#[derive(Default)]
struct LiveLinks {
    count: Mutex<usize>,
    idle: Condvar,
}

/// One running link, counted until dropped.
pub(crate) struct LiveLink(Arc<LiveLinks>);

impl Drop for LiveLink {
    fn drop(&mut self) {
        let mut count = lock_recover(&self.0.count);
        *count = count.saturating_sub(1);
        self.0.idle.notify_all();
    }
}

/// See the module documentation.
pub struct HolderHost {
    boot: String,
    holder_exe: Option<PathBuf>,
    /// The instance argument: its id, empty for the default instance.
    instance: OsString,
    place: Place,
    anonymous_windows: AtomicU64,
    fallback_logged: Arc<AtomicBool>,
    links: Arc<LiveLinks>,
}

enum Place {
    Daemon(HolderPlace),
    Private(OnceCell<Arc<PrivatePlace>>),
}

/// What one holder is started with, resolved as its window opens.
pub(crate) struct LinkContext {
    pub(crate) holder_exe: PathBuf,
    pub(crate) instance: OsString,
    pub(crate) boot: String,
    pub(crate) daemon_endpoint: String,
    pub(crate) holders_dir: PathBuf,
    pub(crate) stderr: DetachedStderr,
    /// The log the holder appends its messages to; `None` leaves them on its
    /// standard error.
    pub(crate) log_file: Option<PathBuf>,
    /// Whether a next daemon may come for the window, so a lost connection
    /// leaves the window running.
    pub(crate) awaits_successor: bool,
    /// Set once a holder that could not leave the daemon's job was logged.
    pub(crate) fallback_logged: Arc<AtomicBool>,
    /// Keeps a private place until the holder is gone.
    pub(crate) keep: Option<Arc<PrivatePlace>>,
    pub(crate) live: LiveLink,
}

impl HolderHost {
    /// The host for windows opened under `config`, with a daemon run of its
    /// own ([`HolderHost::boot`]). Binds and creates nothing.
    #[must_use]
    pub fn from_config(config: &DaemonConfig) -> Arc<Self> {
        let place = match &config.holder_place {
            Some(place) => Place::Daemon(place.clone()),
            None => Place::Private(OnceCell::new()),
        };
        Arc::new(Self {
            boot: crate::window::boot_nonce(),
            holder_exe: config.holder_exe.clone(),
            instance: OsString::from(config.instance_id.clone().unwrap_or_default()),
            place,
            anonymous_windows: AtomicU64::new(0),
            fallback_logged: Arc::new(AtomicBool::new(false)),
            links: Arc::default(),
        })
    }

    /// Identifies this daemon run: what every holder is told in `Hello`.
    #[must_use]
    pub fn boot(&self) -> &str {
        &self.boot
    }

    /// The directory this host's holders keep their files in; `None` for a
    /// host whose private place is not made yet, which no holder has used.
    pub(crate) fn holders_dir(&self) -> Option<PathBuf> {
        match &self.place {
            Place::Daemon(place) => Some(place.holders_dir.clone()),
            Place::Private(cell) => cell.get().map(|private| private.holders_dir.clone()),
        }
    }

    /// A window id for a session opened outside a registry, unique within
    /// this host.
    pub(crate) fn anonymous_window_id(&self) -> String {
        let n = self.anonymous_windows.fetch_add(1, Ordering::Relaxed) + 1;
        format!("p{n}-{}", self.boot)
    }

    /// New scope names for a start of `window_id`'s holder (Linux), to write
    /// to the window's row before the holder is handed them; `None` where
    /// windows have no scopes, or when no names can be made.
    #[cfg(target_os = "linux")]
    pub(crate) fn scope_names(&self, window_id: &str) -> Option<HolderScopes> {
        let instance = (!self.instance.is_empty()).then_some(self.instance.as_os_str());
        let names = yagura_os::resolve_instance_from(instance)
            .map_err(|error| error.to_string())
            .and_then(|namespace| {
                yagura_os::platform_unix::scope_names(&namespace, window_id)
                    .map_err(|error| error.to_string())
            });
        match names {
            Ok(names) => Some(HolderScopes {
                holder: names.holder,
                child: names.child,
            }),
            Err(error) => {
                tracing::warn!(window_id, %error, "cannot name a window's scopes; it opens without them");
                None
            }
        }
    }

    /// See the Linux version: windows elsewhere have no scopes.
    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::unused_self)]
    pub(crate) fn scope_names(&self, _window_id: &str) -> Option<HolderScopes> {
        None
    }

    /// Resolves what the next holder is started with; a host without a
    /// [`HolderPlace`] makes its private place the first time.
    pub(crate) async fn link_context(&self) -> Result<LinkContext, PtyError> {
        let Some(holder_exe) = self.holder_exe.clone() else {
            tracing::error!("no window holder executable is resolved; refusing to open a window");
            return Err(PtyError::SpawnFailed);
        };
        // A daemon's place outlives the daemon: the next daemon of its instance
        // comes for the window. A private place goes with its host.
        let awaits_successor = matches!(self.place, Place::Daemon(_));
        let (daemon_endpoint, holders_dir, stderr, log_file, keep) = match &self.place {
            Place::Daemon(place) => {
                let holders_dir = place.holders_dir.clone();
                let prepared = tokio::task::spawn_blocking(move || {
                    yagura_os::ensure_private_dir(&holders_dir)
                })
                .await
                .map_err(|_| PtyError::SpawnFailed)?;
                prepared.map_err(|error| {
                    tracing::error!(
                        %error,
                        dir = %place.holders_dir.display(),
                        "the holders directory is not usable; refusing to open a window"
                    );
                    PtyError::SpawnFailed
                })?;
                // Messages go to the log line by line, so the holder holds
                // no handle to the daemon's log.
                (
                    place.daemon_endpoint.clone(),
                    place.holders_dir.clone(),
                    DetachedStderr::Null,
                    place.log_file.clone(),
                    None,
                )
            }
            Place::Private(cell) => {
                let private =
                    cell.get_or_try_init(PrivatePlace::create)
                        .await
                        .map_err(|error| {
                            tracing::error!(%error, "cannot make a place for window holders");
                            PtyError::SpawnFailed
                        })?;
                (
                    private.endpoint.clone(),
                    private.holders_dir.clone(),
                    DetachedStderr::Inherit,
                    None,
                    Some(Arc::clone(private)),
                )
            }
        };
        *lock_recover(&self.links.count) += 1;
        Ok(LinkContext {
            holder_exe,
            instance: self.instance.clone(),
            boot: self.boot.clone(),
            daemon_endpoint,
            holders_dir,
            stderr,
            log_file,
            awaits_successor,
            fallback_logged: Arc::clone(&self.fallback_logged),
            keep,
            live: LiveLink(Arc::clone(&self.links)),
        })
    }

    /// What a window a previous daemon of the instance left is taken back
    /// with; `None` for a host without a [`HolderPlace`], whose holders never
    /// outlive it.
    pub(crate) fn take_back_context(&self) -> Option<TakeBackContext> {
        let Place::Daemon(place) = &self.place else {
            return None;
        };
        Some(TakeBackContext {
            instance: self.instance.clone(),
            boot: self.boot.clone(),
            daemon_endpoint: place.daemon_endpoint.clone(),
            holders_dir: place.holders_dir.clone(),
        })
    }
}

/// What a window left by a previous daemon is taken back with: the holder is
/// already running, at a place that outlives every daemon of the instance.
pub(crate) struct TakeBackContext {
    pub(crate) instance: OsString,
    pub(crate) boot: String,
    pub(crate) daemon_endpoint: String,
    pub(crate) holders_dir: PathBuf,
}

impl Drop for HolderHost {
    fn drop(&mut self) {
        // A private place is removed with its last holder; waiting here lets
        // that happen before whoever dropped the host goes on (or exits).
        if !matches!(&self.place, Place::Private(cell) if cell.initialized()) {
            return;
        }
        let count = lock_recover(&self.links.count);
        let (count, waited) = self
            .links
            .idle
            .wait_timeout_while(count, PRIVATE_PLACE_DRAIN_LIMIT, |count| *count > 0)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if waited.timed_out() {
            tracing::warn!(
                running = *count,
                "window holders were still running when their host was dropped"
            );
        }
    }
}

/// An endpoint only this process serves, and an owner-only directory under
/// the OS temp directory: the place of the holders of a host without a
/// [`HolderPlace`]. Removed once dropped.
pub(crate) struct PrivatePlace {
    endpoint: String,
    holders_dir: PathBuf,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    acceptor: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl PrivatePlace {
    async fn create() -> Result<Arc<Self>, String> {
        let ipc = PlatformIpc::default();
        let user = ipc
            .current_user_id()
            .map_err(|error| format!("cannot identify the current user: {error}"))?;
        let mut random = [0u8; 8];
        yagura_os::fill_random(&mut random).map_err(|error| error.to_string())?;
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = format!("yagura-holders-{}-{suffix}", std::process::id());
        let holders_dir = std::env::temp_dir().join(&name);
        yagura_os::ensure_private_dir(&holders_dir).map_err(|error| {
            format!(
                "cannot create the holders directory {}: {error}",
                holders_dir.display()
            )
        })?;
        let endpoint = private_endpoint(&user, &name, &holders_dir);
        let (bound_tx, bound_rx) = oneshot::channel();
        let (stop_tx, stop_rx) = oneshot::channel();
        let acceptor = std::thread::Builder::new()
            .name("holder-host-endpoint".to_owned())
            .spawn({
                let endpoint = endpoint.clone();
                move || serve_private_endpoint(&endpoint, bound_tx, stop_rx)
            });
        let place = Self {
            endpoint,
            holders_dir,
            stop: Mutex::new(Some(stop_tx)),
            acceptor: Mutex::new(None),
        };
        let acceptor = acceptor.map_err(|error| format!("cannot start a thread: {error}"))?;
        *lock_recover(&place.acceptor) = Some(acceptor);
        bound_rx
            .await
            .map_err(|_| "the endpoint thread ended".to_owned())??;
        Ok(Arc::new(place))
    }
}

impl Drop for PrivatePlace {
    fn drop(&mut self) {
        if let Some(stop) = lock_recover(&self.stop).take() {
            let _ = stop.send(());
        }
        if let Some(acceptor) = lock_recover(&self.acceptor).take() {
            let _ = acceptor.join();
        }
        if let Err(error) = std::fs::remove_dir_all(&self.holders_dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                %error,
                dir = %self.holders_dir.display(),
                "cannot remove a window holders directory"
            );
        }
    }
}

#[cfg(windows)]
fn private_endpoint(user: &str, name: &str, _holders_dir: &Path) -> String {
    format!(r"\\.\pipe\{name}-{user}")
}

// A dot keeps the name off every holder's `<window_id>.sock`.
#[cfg(not(windows))]
fn private_endpoint(_user: &str, _name: &str, holders_dir: &Path) -> String {
    holders_dir
        .join(".peer.sock")
        .to_string_lossy()
        .into_owned()
}

/// Binds `endpoint` for this user only and keeps each connection open until
/// its peer closes it, never reading a frame; on a runtime of its own, so the
/// caller's runtime being blocked never holds a holder's check up.
fn serve_private_endpoint(
    endpoint: &str,
    bound: oneshot::Sender<Result<(), String>>,
    mut stop: oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = bound.send(Err(format!("cannot start a runtime: {error}")));
            return;
        }
    };
    runtime.block_on(async {
        let ipc = PlatformIpc::default();
        let listener = match ipc.endpoint_security() {
            Ok(security) => ipc.bind(endpoint, security).await,
            Err(error) => Err(error),
        };
        let mut listener = match listener {
            Ok(listener) => listener,
            Err(error) => {
                let _ = bound.send(Err(format!("cannot bind {endpoint}: {error}")));
                return;
            }
        };
        if bound.send(Ok(())).is_err() {
            return;
        }
        loop {
            tokio::select! {
                _ = &mut stop => return,
                accepted = ipc.accept(&mut listener) => match accepted {
                    Ok(mut connection) => {
                        tokio::spawn(async move {
                            let mut byte = [0u8; 64];
                            let _ = tokio::time::timeout(PRIVATE_PEER_LIMIT, async {
                                while matches!(connection.read(&mut byte).await, Ok(n) if n > 0) {}
                            })
                            .await;
                        });
                    }
                    Err(_) => tokio::select! {
                        _ = &mut stop => return,
                        () = tokio::time::sleep(PRIVATE_ACCEPT_RETRY) => {}
                    },
                },
            }
        }
    });
}
