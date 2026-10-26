//! Transport abstraction between the pure MCP tool logic (`tools.rs`) and
//! the daemon's IPC, which is reached through the operating-system boundary
//! ([`yagura_os`]).
//!
//! `DaemonTransport` is a plain AFIT trait (no `async-trait`, no `dyn`).

use yagura_protocol::{ClientMessage, DaemonMessage, FrameError, frame_decode};

/// Names the variant of `message` without dumping its payload: a
/// `DaemonMessage` can carry a window id or a `send_message` body, which must
/// never end up embedded in an error string via a `{:?}` of the whole value.
pub(crate) fn message_kind(message: &DaemonMessage) -> &'static str {
    match message {
        DaemonMessage::Attached { .. } => "Attached",
        DaemonMessage::Frame(_) => "Frame",
        DaemonMessage::SelectionText { .. } => "SelectionText",
        DaemonMessage::Exit { .. } => "Exit",
        DaemonMessage::Pong => "Pong",
        DaemonMessage::Error { .. } => "Error",
        DaemonMessage::StatusChanged { .. } => "StatusChanged",
        DaemonMessage::WindowOpened { .. } => "WindowOpened",
        DaemonMessage::WindowList { .. } => "WindowList",
        DaemonMessage::WindowStatus { .. } => "WindowStatus",
        DaemonMessage::TextSent { .. } => "TextSent",
        DaemonMessage::KeysSent { .. } => "KeysSent",
        DaemonMessage::WindowInterrupted { .. } => "WindowInterrupted",
        DaemonMessage::WindowClosed { .. } => "WindowClosed",
        DaemonMessage::WaitResult { .. } => "WaitResult",
        DaemonMessage::WindowRenamed { .. } => "WindowRenamed",
        DaemonMessage::WindowOutput { .. } => "WindowOutput",
        DaemonMessage::DetectionList { .. } => "DetectionList",
        DaemonMessage::RestoreCandidateList { .. } => "RestoreCandidateList",
        DaemonMessage::WindowLayoutUpdated { .. } => "WindowLayoutUpdated",
        DaemonMessage::ProfileList { .. } => "ProfileList",
        DaemonMessage::Identified { .. } => "Identified",
        DaemonMessage::ApproverRegistered { .. } => "ApproverRegistered",
        DaemonMessage::ApprovalRequested { .. } => "ApprovalRequested",
        DaemonMessage::ApprovalWithdrawn { .. } => "ApprovalWithdrawn",
        DaemonMessage::ApprovalResolved { .. } => "ApprovalResolved",
        DaemonMessage::HandoffSent { .. } => "HandoffSent",
        DaemonMessage::ChatPosted { .. } => "ChatPosted",
        DaemonMessage::ChatMessages { .. } => "ChatMessages",
        DaemonMessage::ChatChanged { .. } => "ChatChanged",
        DaemonMessage::ChatParticipantsChanged => "ChatParticipantsChanged",
        DaemonMessage::ChatHistoryCleared { .. } => "ChatHistoryCleared",
        DaemonMessage::ChatRoomCleared { .. } => "ChatRoomCleared",
        DaemonMessage::ChatRoomDeleted { .. } => "ChatRoomDeleted",
        DaemonMessage::ChatRooms { .. } => "ChatRooms",
        DaemonMessage::ProtocolRevision { .. } => "ProtocolRevision",
        DaemonMessage::DaemonHealth { .. } => "DaemonHealth",
        DaemonMessage::Delivery { .. } => "Delivery",
        DaemonMessage::SessionRecorded { .. } => "SessionRecorded",
        DaemonMessage::PlanDetail { .. } => "PlanDetail",
        DaemonMessage::PlanList { .. } => "PlanList",
        DaemonMessage::PlanDeleted { .. } => "PlanDeleted",
        DaemonMessage::RoleList { .. } => "RoleList",
        DaemonMessage::TaskWindowOpened { .. } => "TaskWindowOpened",
        DaemonMessage::PlanChanged { .. } => "PlanChanged",
        DaemonMessage::ReportSent { .. } => "ReportSent",
        DaemonMessage::Reports { .. } => "Reports",
        DaemonMessage::ModelChoicesRequested { .. } => "ModelChoicesRequested",
        DaemonMessage::ModelChoices { .. } => "ModelChoices",
        DaemonMessage::ModelAliasesSaved { .. } => "ModelAliasesSaved",
        DaemonMessage::ProfileEnabledSet { .. } => "ProfileEnabledSet",
        DaemonMessage::ProfileDetail { .. } => "ProfileDetail",
        DaemonMessage::ProfileCommandSet { .. } => "ProfileCommandSet",
        DaemonMessage::ApprovalSettings { .. } => "ApprovalSettings",
        DaemonMessage::AutoApproveSet { .. } => "AutoApproveSet",
        DaemonMessage::PlanApprovalRequiredSet { .. } => "PlanApprovalRequiredSet",
        DaemonMessage::McpRegistrationsListed { .. } => "McpRegistrationsListed",
        DaemonMessage::McpRegistrationStatus { .. } => "McpRegistrationStatus",
        DaemonMessage::McpRegistrationRequested { .. } => "McpRegistrationRequested",
        DaemonMessage::McpRegistrationResult { .. } => "McpRegistrationResult",
        DaemonMessage::TicketWorkspaceList { .. } => "TicketWorkspaceList",
        DaemonMessage::TicketList { .. } => "TicketList",
        DaemonMessage::TicketDetail { .. } => "TicketDetail",
        DaemonMessage::MilestoneSet { .. } => "MilestoneSet",
        DaemonMessage::TicketChanged { .. } => "TicketChanged",
    }
}

/// The error code a daemon answers with for a message type that did not exist
/// when that daemon was built.
///
/// A peer will not learn the type later, so this answer settles what the peer
/// can do as firmly as a positive one does.
pub(crate) const UNKNOWN_TYPE_CODE: &str = "unknown_type";

/// The consequence of the remedy every "the daemon is too old" refusal names.
///
/// Kept as one string so that each refusal spells the same consequence out,
/// rather than one of them leaving the caller to discover it.
pub(crate) const DAEMON_RESTART_CONSEQUENCE: &str =
    "restarting the daemon closes every window it is running";

/// How many connections to the daemon one [`EndpointTransport`] holds open at
/// once.
///
/// The daemon may hold a request open for as long as its receiver shows
/// nothing, so each request needs a connection of its own for the agent's other
/// calls to get through; the bound keeps one bridge from opening them without
/// limit. A call beyond it waits for a connection to come free.
pub const MAX_DAEMON_CONNECTIONS: usize = 4;

/// Failure modes a [`DaemonTransport`] implementation can report.
///
/// The mapping onto MCP tool error codes is fixed:
/// `Unavailable` -> `daemon_unavailable`, `Protocol` -> `internal`,
/// `Unsupported` -> `unsupported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The daemon could not be reached at all: no endpoint to connect to,
    /// the connection dropped, or a reconnect attempt failed.
    Unavailable(String),
    /// The daemon was reachable but its response could not be decoded as a
    /// well-formed [`DaemonMessage`] (a contract violation by the daemon).
    Protocol(String),
    /// The daemon was reached and answered, but the wire revision it speaks
    /// is older than the one the request needs, so the request was never
    /// written. Carries an English explanation for the caller.
    Unsupported(String),
}

/// A window's claim on a connection: its ID and its per-boot status token,
/// the same credential a `status_event` hook presents.
#[derive(Debug, Clone)]
pub struct WindowIdentity {
    /// The window ID claiming this connection.
    pub window_id: String,
    /// The window's per-boot status token.
    pub token: String,
}

/// How the daemon settled a connection's identity claim, as reported by
/// [`DaemonTransport::requester_identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequesterIdentity {
    /// No claim was made; the connection is an external client.
    External,
    /// The daemon accepted the claim. Carries the window ID it settled on.
    Window(String),
    /// The daemon refused the claim. Carries an English warning for the
    /// caller; the connection still works as an external client.
    Refused(String),
}

/// Abstraction over a single request/response round trip against the
/// daemon's IPC. Implementations own the underlying connections (lazy
/// connect, reconnect-once-on-failure); callers only see the failure modes
/// above.
// Plain AFIT (no async-trait, no dyn): callers inject implementations
// generically rather than as trait objects.
#[allow(async_fn_in_trait)]
pub trait DaemonTransport: Send + Sync {
    /// This connection's identity as the daemon last settled it. Synchronous
    /// so the tool layer can call it right after a round trip without
    /// awaiting a second time.
    fn requester_identity(&self) -> RequesterIdentity;

    /// Sends `msg` to the daemon and returns its reply.
    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError>;
}

/// Reassembles length-prefixed frames from an arbitrarily chunked byte
/// stream.
///
/// It holds whatever bytes arrived after the last complete frame, so a caller
/// can tell one arriving frame from a second one tagging along. It performs no
/// I/O.
#[derive(Debug, Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    /// Creates an empty reader.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `bytes` to the internal buffer and returns the bodies of
    /// every frame that is now complete, in arrival order.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::TooLarge`] as soon as a length prefix declares
    /// a body above `MAX_FRAME_BODY_LEN`; the oversized frame is never
    /// buffered to completion (fail-loud rather than silently draining).
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        self.buffer.extend_from_slice(bytes);
        let mut bodies = Vec::new();
        while let Some(body) = self.next_frame()? {
            bodies.push(body);
        }
        Ok(bodies)
    }

    /// Whether any bytes of an incomplete frame are still buffered.
    pub fn has_remaining(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Removes and returns the first complete frame body, if any. The body
    /// is copied out so that the borrow of the buffer ends before the
    /// consumed bytes are drained.
    fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let Some((body, consumed)) = frame_decode(&self.buffer)? else {
            return Ok(None);
        };
        let body = body.to_vec();
        self.buffer.drain(..consumed);
        Ok(Some(body))
    }
}

/// Whether a failed round trip may be retried on a fresh connection
/// (at-most-once).
///
/// Once the request's bytes all reached the daemon it may already have run a
/// destructive operation, so it must not be sent again; a partially sent
/// request cannot have been executed, as the daemon only acts on whole frames.
pub fn should_resend(sent_completely: bool) -> bool {
    !sent_completely
}

/// The connected side of the transport, reached through the operating-system
/// boundary ([`yagura_os`]) rather than a platform API of its own. Nothing
/// here is conditionally compiled: the boundary's type aliases resolve the
/// platform difference.
mod connection {
    use std::sync::{Mutex as StdMutex, PoisonError};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{Semaphore, TryAcquireError};
    use yagura_os::{IpcTransport, PlatformIpc};
    use yagura_protocol::{
        ClientMessage, DaemonMessage, PROTOCOL_REVISION_BASELINE, decode_daemon_message,
        encode_client_message, frame_encode, required_protocol_revision,
    };

    use super::{
        DAEMON_RESTART_CONSEQUENCE, DaemonTransport, FrameReader, MAX_DAEMON_CONNECTIONS,
        RequesterIdentity, TransportError, UNKNOWN_TYPE_CODE, WindowIdentity, message_kind,
        should_resend,
    };

    /// A client-side connection to the daemon's endpoint.
    type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

    /// Read buffer chunk size while awaiting a single response frame.
    const READ_CHUNK_LEN: usize = 4096;

    /// Pause before the single permitted retry.
    ///
    /// A daemon that rotates server instances leaves short windows in which
    /// nothing listens on the endpoint, so an immediate retry would land in
    /// the same window; the pause stays short enough not to stall when the
    /// daemon is simply not running.
    const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

    /// Borrowed access to the transport's connection slot which abandons the
    /// connection unless the round trip finished cleanly.
    ///
    /// Dropping the guard while still armed -- I/O error, protocol violation,
    /// leftover bytes, or a `round_trip` future cancelled mid-flight -- empties
    /// the slot, so a late or unattributable reply dies with its connection
    /// instead of reaching the next caller.
    struct ConnectionGuard<'a> {
        slot: &'a mut Option<Connection>,
        armed: bool,
    }

    impl<'a> ConnectionGuard<'a> {
        fn new(slot: &'a mut Option<Connection>) -> Self {
            Self { slot, armed: true }
        }

        fn connection(&mut self) -> Result<&mut Connection, TransportError> {
            self.slot.as_mut().ok_or_else(|| {
                TransportError::Unavailable("connection slot unexpectedly empty".to_string())
            })
        }

        /// Keeps the connection alive for the next round trip.
        fn keep(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for ConnectionGuard<'_> {
        fn drop(&mut self) {
            if self.armed {
                *self.slot = None;
            }
        }
    }

    /// A failed attempt plus the at-most-once decision input: whether the
    /// request's bytes had all reached the daemon.
    struct AttemptError {
        error: TransportError,
        sent_completely: bool,
    }

    /// Why an attempt produced no reply.
    enum AttemptFailure {
        /// The exchange itself failed, and [`should_resend`] decides whether
        /// the request may go out again on a fresh connection.
        Exchange(AttemptError),
        /// The peer's wire revision cannot carry this request, so no byte of
        /// it was written. Carries the explanation for the caller.
        Unsupported(String),
    }

    impl From<AttemptError> for AttemptFailure {
        fn from(failed: AttemptError) -> Self {
            Self::Exchange(failed)
        }
    }

    impl AttemptFailure {
        /// Whether the request may be sent again on a fresh connection.
        ///
        /// A refusal never reaches the daemon, but a fresh connection reaches
        /// the same peer and settles on the same revision, so retrying it only
        /// costs another probe.
        fn may_retry(&self) -> bool {
            match self {
                Self::Exchange(failed) => should_resend(failed.sent_completely),
                Self::Unsupported(_) => false,
            }
        }

        fn into_error(self) -> TransportError {
            match self {
                Self::Exchange(failed) => failed.error,
                Self::Unsupported(reason) => TransportError::Unsupported(reason),
            }
        }
    }

    /// The connection this transport is currently on, together with what that
    /// connection's peer has settled about its own wire revision.
    ///
    /// The two are one value under one lock because the answer describes that
    /// one connection: a replacement may reach a different daemon, so a
    /// settled revision must not outlive the connection that gave it.
    #[derive(Default)]
    struct ConnectionSlot {
        connection: Option<Connection>,
        /// The peer's wire revision once it has settled; `None` until then.
        revision: Option<u32>,
    }

    impl ConnectionSlot {
        /// Whether this connection's peer can be relied on to honor a message
        /// that needs wire revision `required`.
        ///
        /// A peer that has not said which wire it speaks counts as the oldest
        /// one anyone has shipped: that is the only thing assumable before it
        /// answers, so an unsettled connection refuses rather than gambles.
        fn carries(&self, required: u32) -> bool {
            self.revision.unwrap_or(PROTOCOL_REVISION_BASELINE) >= required
        }

        /// Explains a refusal in terms of the revisions alone. The transport
        /// knows the integer a message asks for and the one its peer speaks,
        /// never which feature is at stake.
        fn refusal(&self, required: u32) -> String {
            let observed = match self.revision {
                Some(revision) => format!("the connected daemon speaks revision {revision}"),
                None => "the connected daemon has not reported one".to_string(),
            };
            format!(
                "this request needs wire revision {required}, but {observed}. Restart the \
                 daemon from a build that speaks revision {required}; {DAEMON_RESTART_CONSEQUENCE}."
            )
        }
    }

    async fn write_request(
        guard: &mut ConnectionGuard<'_>,
        request: &[u8],
    ) -> Result<(), TransportError> {
        guard
            .connection()?
            .write_all(request)
            .await
            .map_err(|err| TransportError::Unavailable(err.to_string()))
    }

    /// Reads exactly one response frame through a [`FrameReader`].
    ///
    /// The connection is kept only when the daemon answered with exactly one
    /// frame and nothing else: any extra frame or trailing byte makes the
    /// stream unattributable to this request, so the guard stays armed while
    /// the frame that was read is still returned normally.
    async fn read_response(
        guard: &mut ConnectionGuard<'_>,
    ) -> Result<DaemonMessage, TransportError> {
        let mut reader = FrameReader::new();
        let mut chunk = [0u8; READ_CHUNK_LEN];
        loop {
            let read = guard
                .connection()?
                .read(&mut chunk)
                .await
                .map_err(|err| TransportError::Unavailable(err.to_string()))?;
            if read == 0 {
                return Err(TransportError::Unavailable(
                    "the daemon closed the connection".to_string(),
                ));
            }

            let bodies = reader
                .push(&chunk[..read])
                .map_err(|err| TransportError::Protocol(err.to_string()))?;
            let Some(body) = bodies.first() else {
                continue;
            };

            let message = decode_daemon_message(body)
                .map_err(|err| TransportError::Protocol(err.to_string()))?;
            if bodies.len() == 1 && !reader.has_remaining() {
                guard.keep();
            }
            return Ok(message);
        }
    }

    /// One send/receive attempt over the connection currently in `slot`.
    async fn exchange(
        slot: &mut Option<Connection>,
        request: &[u8],
    ) -> Result<DaemonMessage, AttemptError> {
        let mut guard = ConnectionGuard::new(slot);

        if let Err(error) = write_request(&mut guard, request).await {
            return Err(AttemptError {
                error,
                sent_completely: false,
            });
        }

        read_response(&mut guard)
            .await
            .map_err(|error| AttemptError {
                error,
                sent_completely: true,
            })
    }

    /// A lazily connected [`DaemonTransport`] backed by the platform's local
    /// IPC transport.
    ///
    /// Each `round_trip` carries its request over a connection of its own:
    /// an idle one when there is one, otherwise a freshly opened one. At most
    /// [`MAX_DAEMON_CONNECTIONS`] are open at once, and a call beyond that
    /// waits for one to come free. A connection carries one request and its
    /// one reply at a time and goes back to the idle set only when that
    /// exchange finished cleanly; a `round_trip` future dropped mid-flight
    /// drops its connection, which the daemon reads as the requester leaving.
    ///
    /// If a call finds its connection gone *before the request was fully
    /// sent*, it attempts exactly one reconnect and resend before giving up
    /// with [`TransportError::Unavailable`]; a failure after the request was
    /// fully sent is never retried, so a destructive operation cannot run
    /// twice (at-most-once). This crate never spawns the daemon.
    ///
    /// Every connection is asked once which wire revision its peer speaks. A
    /// request needing a newer wire than the peer settled on is refused with
    /// [`TransportError::Unsupported`] without a byte of it being written, and
    /// the answer never outlives the connection that gave it.
    pub struct EndpointTransport {
        ipc: PlatformIpc,
        endpoint: String,
        /// Connections not carrying a request right now.
        idle: StdMutex<Vec<ConnectionSlot>>,
        /// One permit per caller holding or opening a connection. A caller
        /// opens a connection only while it holds a permit and finds `idle`
        /// empty, and puts its connection back before releasing the permit,
        /// so the open connections never exceed [`MAX_DAEMON_CONNECTIONS`].
        permits: Semaphore,
        /// The claim to assert on every freshly opened connection. `None`
        /// means this transport never sends `identify` and stays `External`.
        identity: Option<WindowIdentity>,
        /// The daemon's last decision about `identity`, read synchronously by
        /// [`DaemonTransport::requester_identity`].
        requester: StdMutex<RequesterIdentity>,
    }

    impl EndpointTransport {
        /// Creates a transport bound to the endpoint `endpoint`. No
        /// connection is attempted until the first
        /// [`DaemonTransport::round_trip`] call.
        ///
        /// When `identity` is `Some`, every connection this transport opens
        /// asserts that claim with `identify` before the caller's own
        /// request; when `None`, no `identify` is ever sent and the
        /// connection stays `External`.
        pub fn new(endpoint: impl Into<String>, identity: Option<WindowIdentity>) -> Self {
            Self {
                ipc: PlatformIpc::default(),
                endpoint: endpoint.into(),
                idle: StdMutex::new(Vec::new()),
                permits: Semaphore::new(MAX_DAEMON_CONNECTIONS),
                identity,
                requester: StdMutex::new(RequesterIdentity::External),
            }
        }

        async fn connect(&self) -> Result<Connection, TransportError> {
            self.ipc
                .connect(&self.endpoint)
                .await
                .map_err(|err| TransportError::Unavailable(err.to_string()))
        }

        /// Asserts `identity` over the connection just placed in `slot` and
        /// folds the daemon's decision into `self.requester`.
        ///
        /// Only called once per fresh connection, from
        /// [`Self::connect_and_identify`]:
        /// a settled claim (accepted or refused) holds for as long as the
        /// connection does, and a reconnect re-asserts it from scratch.
        /// A reply that settles nothing therefore has to take the connection
        /// with it, or the caller's own request would go out over a
        /// connection this transport never identified itself on.
        async fn identify(
            &self,
            slot: &mut Option<Connection>,
            identity: &WindowIdentity,
        ) -> Result<(), AttemptError> {
            let request = frame_encode(&encode_client_message(&ClientMessage::Identify {
                window_id: identity.window_id.clone(),
                token: identity.token.clone(),
            }));
            // The caller's own request was never attempted yet, whatever
            // stage this exchange reached, so every failure here is
            // `sent_completely: false`.
            let reply = exchange(slot, &request)
                .await
                .map_err(|failed| AttemptError {
                    error: failed.error,
                    sent_completely: false,
                })?;
            let settled = match reply {
                DaemonMessage::Identified { window_id } => RequesterIdentity::Window(window_id),
                DaemonMessage::Error { code, message, .. } => RequesterIdentity::Refused(format!(
                    "the daemon refused this window identity ({code}): {message}"
                )),
                other => {
                    // Nothing settled, so this connection is unusable: drop it
                    // and let the single retry identify on a fresh one.
                    slot.take();
                    return Err(AttemptError {
                        error: TransportError::Protocol(format!(
                            "unexpected reply to identify: {}",
                            message_kind(&other)
                        )),
                        sent_completely: false,
                    });
                }
            };
            *self
                .requester
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = settled;
            Ok(())
        }

        /// Settles the wire revision of the connection in `slot`, unless it is
        /// settled already.
        ///
        /// Idempotent: a settled connection costs nothing and an unsettled one
        /// costs exactly one round trip, so the result does not depend on how
        /// often, or from where, this is called.
        ///
        /// A revision answer and an `unknown_type` refusal both settle the
        /// question for good and are recorded. Any other daemon error leaves it
        /// open for the next call to ask again, which is why it does not fail
        /// the caller's own request. A reply of another type settles nothing
        /// and takes its connection with it, exactly as an unusable `identify`
        /// reply does.
        async fn settle_revision(slot: &mut ConnectionSlot) -> Result<(), AttemptError> {
            if slot.revision.is_some() {
                return Ok(());
            }
            let request = frame_encode(&encode_client_message(&ClientMessage::GetProtocolRevision));
            // The caller's own request was never attempted yet, whatever stage
            // this exchange reached, so every failure here is
            // `sent_completely: false`.
            let reply = exchange(&mut slot.connection, &request)
                .await
                .map_err(|failed| AttemptError {
                    error: failed.error,
                    sent_completely: false,
                })?;
            match reply {
                DaemonMessage::ProtocolRevision { revision } => slot.revision = Some(revision),
                DaemonMessage::Error { code, .. } if code == UNKNOWN_TYPE_CODE => {
                    slot.revision = Some(PROTOCOL_REVISION_BASELINE);
                }
                DaemonMessage::Error { .. } => {}
                other => {
                    slot.connection.take();
                    return Err(AttemptError {
                        error: TransportError::Protocol(format!(
                            "unexpected reply to the wire revision request: {}",
                            message_kind(&other)
                        )),
                        sent_completely: false,
                    });
                }
            }
            Ok(())
        }

        /// Opens a fresh connection into `slot` and asserts `identity` on it,
        /// if this transport carries one.
        ///
        /// A failed connect reports `sent_completely: false`: nothing reached
        /// the daemon, so a retry cannot execute anything twice.
        async fn connect_and_identify(
            &self,
            slot: &mut ConnectionSlot,
        ) -> Result<(), AttemptError> {
            // A replacement connection inherits nothing: it may reach an
            // older daemon than the one that answered before it.
            slot.revision = None;
            match self.connect().await {
                Ok(connection) => slot.connection = Some(connection),
                Err(error) => {
                    return Err(AttemptError {
                        error,
                        sent_completely: false,
                    });
                }
            }
            if let Some(identity) = &self.identity {
                self.identify(&mut slot.connection, identity).await?;
            }
            Ok(())
        }

        /// Claims this transport's window identity on a connection right
        /// away, instead of waiting for the first [`DaemonTransport::round_trip`].
        ///
        /// The daemon only counts a window as connected once it has
        /// identified, so a bridge that stays silent until its first tool call
        /// would look disconnected. The connection is kept idle for the next
        /// round trip, which neither reconnects nor re-identifies. Does nothing
        /// when this transport carries no identity, already holds an idle
        /// connection, or has every connection in use: each of those is
        /// identified on its own.
        ///
        /// # Errors
        ///
        /// Returns the [`TransportError`] of a failed connect or `identify`
        /// exchange. The transport stays lazily connected after a failure:
        /// the next round trip connects on its own.
        pub async fn eager_identify(&self) -> Result<(), TransportError> {
            if self.identity.is_none() || !self.idle_is_empty() {
                return Ok(());
            }
            let _permit = match self.permits.try_acquire() {
                Ok(permit) => permit,
                Err(TryAcquireError::NoPermits | TryAcquireError::Closed) => return Ok(()),
            };
            let mut slot = ConnectionSlot::default();
            self.connect_and_identify(&mut slot)
                .await
                .map_err(|failed| failed.error)?;
            self.put_idle(slot);
            Ok(())
        }

        fn idle_is_empty(&self) -> bool {
            self.idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        }

        /// An idle connection, or an empty slot the caller connects itself.
        fn take_idle(&self) -> ConnectionSlot {
            self.idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop()
                .unwrap_or_default()
        }

        /// Keeps `slot` for a later round trip when it still holds a
        /// connection; an emptied slot is simply dropped.
        fn put_idle(&self, slot: ConnectionSlot) {
            if slot.connection.is_some() {
                self.idle
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(slot);
            }
        }

        /// One full attempt: connect and (re-)identify if the slot is empty,
        /// settle the connection's wire revision, refuse `msg` if that wire
        /// cannot carry it, and otherwise exchange the caller's own request.
        ///
        /// Settling and refusing both sit here rather than in the "just
        /// connected" branch: a connection that answered an error survives
        /// unsettled, so a check confined to fresh connections would never look
        /// at it again.
        async fn attempt(
            &self,
            slot: &mut ConnectionSlot,
            msg: &ClientMessage,
            request: &[u8],
        ) -> Result<DaemonMessage, AttemptFailure> {
            if slot.connection.is_none() {
                self.connect_and_identify(slot).await?;
            }
            Self::settle_revision(slot).await?;

            let required = required_protocol_revision(msg);
            if !slot.carries(required) {
                return Err(AttemptFailure::Unsupported(slot.refusal(required)));
            }
            Ok(exchange(&mut slot.connection, request).await?)
        }

        /// One attempt, plus the single retry on a fresh connection that
        /// [`AttemptFailure::may_retry`] allows.
        async fn exchange_with_retry(
            &self,
            slot: &mut ConnectionSlot,
            msg: &ClientMessage,
            request: &[u8],
        ) -> Result<DaemonMessage, TransportError> {
            match self.attempt(slot, msg, request).await {
                Ok(response) => Ok(response),
                Err(first) => {
                    // At-most-once: a failure after the request was fully sent
                    // may already have executed a destructive operation, so
                    // only connect/send-phase failures are retried.
                    if !first.may_retry() {
                        return Err(first.into_error());
                    }
                    // The retry always starts from a fresh connection: a failed
                    // connect left the slot empty, and a failed send had its
                    // connection dropped by the guard.
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    self.attempt(slot, msg, request)
                        .await
                        .map_err(AttemptFailure::into_error)
                }
            }
        }
    }

    impl DaemonTransport for EndpointTransport {
        fn requester_identity(&self) -> RequesterIdentity {
            self.requester
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
            // Declared before the slot so that a dropped future closes its
            // connection before the permit lets the next caller open one.
            let _permit = self.permits.acquire().await.map_err(|_closed| {
                TransportError::Unavailable("the connection limit was closed".to_string())
            })?;
            let mut slot = self.take_idle();
            let request = frame_encode(&encode_client_message(&msg));
            let result = self.exchange_with_retry(&mut slot, &msg, &request).await;
            self.put_idle(slot);
            result
        }
    }
}

pub use connection::EndpointTransport;
