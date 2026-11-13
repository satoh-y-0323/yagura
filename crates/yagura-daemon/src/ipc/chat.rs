//! The chat rooms' IPC surface: [`ChatRuntime`] (store lifecycle, limits,
//! the intro a participant's start-up door carries), the
//! room a request belongs to ([`resolve_room`]), the window-opened hook
//! ([`install_intro_hook`]) and the `send_chat` / `broadcast_chat` /
//! `read_chat` / `list_chat_rooms` handlers.
//!
//! A post reaches each addressed window through that window's inbox: its CLI
//! takes it through its own doors, or the window's agent pushes it the way the
//! window's profile declares. A post is accepted only once every inbox it went
//! in has written its row; one that could not be written is refused, and
//! shows in no inbox.
//!
//! A window belongs to exactly one room -- the one its workspace key maps to
//! -- and `"main"` is the one room every caller may reach. Which room a
//! request acts on is decided in one place ([`resolve_room`]), and who is in
//! a room in another ([`room_members`]).
//!
//! `halted` / `needs_user` / a message's delivery list all have exactly one
//! source of truth: `chat_store.rs`'s `RoomStatus` and delivery getters.
//! Nothing in this module mirrors them, so every reader -- a fresh boot's
//! first `read_chat` included -- sees what the previous boot last wrote.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use yagura_protocol::{
    CHAT_READ_LIMIT_MAX, ChatDeliveryInfo, ChatHalt, ChatMessage, ChatParticipant, ChatRoomInfo,
    DaemonMessage, WindowStatus, is_valid_token, validate_chat_body,
};

use crate::approval::{ApprovalBroker, ApprovalDraft, ApprovalRoomRef, SubmitError};
use crate::chat_store::{self, ChatStore, NewChatMessage, ParcelEndRecord, PostEffects, Share};
use crate::config::DaemonConfig;
use crate::control::{self, DestructiveTool, GateVerdict, RequestOrigin};
use crate::handoff::with_envelope_id;
use crate::inbox::{Deposited, Parcel, ParcelEnd, SaveOutcome, Unwritten};
use crate::live_policy::{self, PolicyCell};
use crate::parcel_store::ChatDelivery;
use crate::room::{self, ChatLimits, DeliveryOutcome};
use crate::store::StoreError;
use crate::window::{HooksRoute, WindowEntry, WindowParticipation, WindowRegistry, takes_delivery};
use crate::workspace::{self, WorkspaceKey, WorkspaceKind};

use super::notice_queue;
use super::plan::PlanRuntime;
use super::report;
use super::store_gate::{StoreAccessError, StoreGate};
use super::{
    ApprovalEnd, Outbound, RequestWatch, approval_limits, await_approval, deny_msg,
    deny_no_approver, error_msg, now_ms, requester_for, window_info,
};

/// The one room every caller may reach, whatever workspace it belongs to.
/// Its row is created by the schema.
const MAIN_ROOM_ID: &str = "main";

/// The chat room's runtime state: the store gate, the effective `[chat]`
/// limits, the intro text and the participation exclusion list.
pub struct ChatRuntime {
    store: StoreGate<ChatStore>,
    limits: ChatLimits,
    intro: String,
    exclude_detections: Vec<String>,
    /// Whether this daemon was built with no chat at all, rather than with
    /// a chat that failed to come up.
    unwired: bool,
    /// The daemon's [`DaemonConfig::faults`].
    #[cfg(feature = "fault-injection")]
    faults: crate::fault_point::FaultPoints,
}

impl ChatRuntime {
    /// A disabled runtime: every chat tool answers `unsupported`, and no
    /// window's start-up door carries an intro.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            store: StoreGate::disabled(),
            limits: ChatLimits::default(),
            intro: String::new(),
            exclude_detections: Vec::new(),
            unwired: false,
            #[cfg(feature = "fault-injection")]
            faults: crate::fault_point::FaultPoints::default(),
        }
    }

    /// The runtime of a daemon built with no chat ([`super::IpcServer::new`]),
    /// so its construction never touches a database: every chat tool answers
    /// `unsupported`, and with no room to decide who takes part, every
    /// window's start-up door carries `config`'s intro.
    #[must_use]
    pub fn unwired(config: &DaemonConfig) -> Self {
        Self {
            intro: config.chat_intro.clone(),
            unwired: true,
            ..Self::disabled()
        }
    }

    /// [`Self::disabled`], logging why (once) first.
    fn disabled_with_reason(reason: &str) -> Self {
        tracing::warn!("chat disabled: {reason}");
        Self::disabled()
    }

    /// Builds the chat runtime from `config`'s already-resolved fields
    /// (`chat_limits` / `chat_intro` / `chat_exclude_detections` /
    /// `store_db_path_resolved` / `playbook` / `approval_policy`), opening the
    /// store synchronously.
    ///
    /// A caller in an async context must run this on a blocking thread
    /// (`tokio::task::spawn_blocking`): opening a database is blocking I/O.
    ///
    /// A playbook that was configured but failed to load (`playbook.is_some()`
    /// and `approval_policy.is_none()`) disables chat entirely rather than
    /// running it with built-in defaults: a broken policy file
    /// must not leave chat silently open on whatever the defaults happen to
    /// allow.
    ///
    /// The pending deliveries to the windows of `remaining` (the windows the
    /// start found still there, taken back or lost) stay pending; every other
    /// one is settled as never delivered.
    #[must_use]
    pub fn from_config(config: &DaemonConfig, remaining: &[String]) -> Self {
        if config.playbook.is_some() && config.approval_policy.is_none() {
            return Self::disabled_with_reason("the configured playbook failed to load");
        }
        let Some(db_path) = config.store_db_path_resolved.as_ref() else {
            return Self::disabled_with_reason("no store database path is resolved");
        };
        match ChatStore::open(db_path) {
            Ok(mut store) => {
                tracing::info!("chat store: {}", db_path.display());
                // An inbox ends with its daemon: what a previous one still held
                // for a window that is gone was never delivered.
                if let Err(err) = store.settle_pending_after_restart(remaining) {
                    tracing::warn!(error = %err, "could not settle the deliveries a previous daemon left pending");
                }
                Self {
                    store: StoreGate::opened(store),
                    limits: config.chat_limits,
                    intro: config.chat_intro.clone(),
                    exclude_detections: config.chat_exclude_detections.clone(),
                    unwired: false,
                    #[cfg(feature = "fault-injection")]
                    faults: config.faults.clone(),
                }
            }
            Err(err) => {
                Self::disabled_with_reason(&format!("failed to open the chat store: {err}"))
            }
        }
    }

    /// The intro `entry`'s start-up door carries: the configured `[chat]
    /// intro` for a chat participant, nothing for any other window or when
    /// chat failed to come up.
    pub(super) fn intro_for(&self, entry: &WindowEntry) -> &str {
        self.introduction(entry.delivers(), entry.detection_definition())
    }

    /// The intro a window is started with, decided before it is spawned from
    /// the same facts [`Self::intro_for`] decides from afterwards: `has_delivery`
    /// and the `hooks` route the launch settled on, and the detection
    /// definition's name.
    pub(crate) fn launch_intro(
        &self,
        has_delivery: bool,
        hooks: HooksRoute,
        detection: Option<&str>,
    ) -> &str {
        self.introduction(takes_delivery(has_delivery, hooks), detection)
    }

    /// The one rule for who is introduced to the room.
    fn introduction(&self, delivers: bool, detection: Option<&str>) -> &str {
        let introduced = self.unwired
            || (self.store.is_enabled()
                && is_participant(delivers, detection, &self.exclude_detections));
        if introduced { &self.intro } else { "" }
    }

    /// Records one destination's delivery outcome in the store, reason
    /// included when the outcome is the one arm able to carry one. Returns
    /// `false` when the store reported the message gone and wrote nothing.
    async fn record_delivery(
        &self,
        room_id: &str,
        seq: i64,
        window_id: &str,
        outcome: DeliveryOutcome,
    ) -> bool {
        let room_id = room_id.to_string();
        let window_id = window_id.to_string();
        let at_ms = i64::try_from(now_ms()).unwrap_or(i64::MAX);
        let recorded = self
            .store
            .with_store(move |store| store.set_delivery(&room_id, seq, &window_id, outcome, at_ms))
            .await;
        // A failed write still announces: the reader re-reads what the store holds.
        !matches!(recorded, Ok(Ok(false)))
    }

    /// [`Self::record_delivery`], then pushes a [`DaemonMessage::ChatChanged`]
    /// carrying `seq` and the room's `needs_user` exactly as the store now
    /// holds it: every place a destination's delivery outcome settles calls
    /// this, so a `pending` row written by `append_message` never goes
    /// unannounced. A message cleared in the meantime has nothing to announce.
    async fn record_delivery_and_notify(
        &self,
        broker: &Arc<ApprovalBroker>,
        room_id: &str,
        seq: i64,
        window_id: &str,
        outcome: DeliveryOutcome,
    ) {
        if !self.record_delivery(room_id, seq, window_id, outcome).await {
            return;
        }
        self.announce_change(broker, room_id, seq).await;
    }

    /// Settles a parcel's end ([`ChatStore::settle_parcel_end`]): its kept
    /// row is forgotten together with the delivery's settlement, then the
    /// change is announced as [`Self::record_delivery_and_notify`] does.
    async fn settle_parcel_end_and_notify(
        &self,
        broker: &Arc<ApprovalBroker>,
        room_id: &str,
        seq: i64,
        window_id: &str,
        outcome: DeliveryOutcome,
    ) {
        let room = room_id.to_string();
        let window = window_id.to_string();
        let parcel_key = chat_parcel_key(room_id, seq);
        let at_ms = i64::try_from(now_ms()).unwrap_or(i64::MAX);
        #[cfg(feature = "fault-injection")]
        let faults = self.faults.clone();
        let recorded = self
            .store
            .with_store(move |store| {
                let end = ParcelEndRecord {
                    room_id: &room,
                    seq,
                    window_id: &window,
                    parcel_key: &parcel_key,
                    outcome,
                    at_ms,
                };
                store.settle_parcel_end(&end, || {
                    #[cfg(feature = "fault-injection")]
                    faults.reach(crate::fault_point::FaultPoint::ParcelEndHalfSettled);
                })
            })
            .await;
        if let Ok(Err(error)) = &recorded {
            tracing::warn!(%error, "could not settle a chat parcel's end; its row stays kept");
        }
        // A failed write still announces: the reader re-reads what the store holds.
        if !matches!(recorded, Ok(Ok(false))) {
            self.announce_change(broker, room_id, seq).await;
        }
    }

    /// Pushes a [`DaemonMessage::ChatChanged`] carrying `seq` and the room's
    /// `needs_user` as the store holds it.
    async fn announce_change(&self, broker: &Arc<ApprovalBroker>, room_id: &str, seq: i64) {
        let room_id_owned = room_id.to_string();
        let needs_user = match self
            .store
            .with_store(move |store| store.room_status(&room_id_owned))
            .await
        {
            Ok(Ok(status)) => status.needs_user,
            // Fail-safe: a status this daemon could not read back is treated
            // as needing the user.
            _ => true,
        };
        broker.notify_approvers(&DaemonMessage::ChatChanged {
            room_id: room_id.to_string(),
            seq: u64::try_from(seq).unwrap_or(0),
            needs_user,
        });
    }

    /// Evaluates whether a delivered-and-unanswered post should be flagged
    /// for silence, called once, `silence_timeout_secs` after
    /// the post, by the task [`spawn_silence_watch`] spawns. A no-op if the
    /// room has since halted, nothing was actually placed, an addressed
    /// window has since replied, the message was already noted, or the
    /// message is gone.
    async fn evaluate_silence(
        &self,
        broker: &Arc<ApprovalBroker>,
        room_id: &str,
        seq: i64,
        targets: &[String],
    ) {
        if targets.is_empty() {
            return;
        }
        let room_id_for_precheck = room_id.to_string();
        let precheck = self
            .store
            .with_store(
                move |store| -> Result<
                    (chat_store::RoomStatus, Vec<chat_store::DeliveryRow>),
                    StoreError,
                > {
                    let status = store.room_status(&room_id_for_precheck)?;
                    let deliveries = store.deliveries_for_seq(&room_id_for_precheck, seq)?;
                    Ok((status, deliveries))
                },
            )
            .await;
        let Ok(Ok((status, deliveries))) = precheck else {
            return;
        };

        let room_id_owned = room_id.to_string();
        let targets_owned = targets.to_vec();
        let probe = self
            .store
            .with_store(move |store| -> Result<(bool, bool), StoreError> {
                let this_row = store.read_after(&room_id_owned, seq - 1, 1)?;
                let already_noted = this_row.messages.first().is_some_and(|m| m.silence_noted);
                let after = store.read_after(&room_id_owned, seq, 1000)?;
                let replied_since = after.messages.iter().any(|m| {
                    m.from_window_id
                        .as_deref()
                        .is_some_and(|id| targets_owned.iter().any(|t| t == id))
                });
                Ok((already_noted, replied_since))
            })
            .await;
        let Ok(Ok((already_noted, replied_since))) = probe else {
            return;
        };

        // The rule itself lives in `room`; this call site only gathers the
        // evidence. The store already dropped any row it could not
        // reconstruct, so every remaining outcome projects onto a state.
        let silence_timeout_ms = self.limits.silence_timeout_secs.saturating_mul(1000);
        let check = room::SilenceCheck {
            deliveries: deliveries
                .iter()
                .map(|(_, outcome)| outcome.state())
                .collect(),
            replied_since,
            // This runs once the watcher has already slept the timeout out.
            elapsed_ms: silence_timeout_ms,
            halted: status.halted.is_some(),
            silence_noted: already_noted,
        };
        if !room::is_silence_candidate(&check, silence_timeout_ms) {
            return;
        }

        let limits = self.limits;
        let room_id_for_store = room_id.to_string();
        let at_ms = i64::try_from(now_ms()).unwrap_or(i64::MAX);
        let result = self
            .store
            .with_store(move |store| note_silence(store, &room_id_for_store, seq, at_ms, &limits))
            .await;
        if matches!(result, Ok(Ok(true))) {
            broker.notify_approvers(&DaemonMessage::ChatChanged {
                room_id: room_id.to_string(),
                seq: u64::try_from(seq).unwrap_or(0),
                needs_user: true,
            });
        }
    }
}

/// Flags the post `(room_id, seq)` as unanswered: marks it noted, appends the
/// system notice and raises `needs_user`. Writes nothing and returns `false`
/// when the post is gone (its history was cleared or its room deleted) -- the
/// check and the writes run under one store access, so a clear cannot slip
/// between them.
fn note_silence(
    store: &mut ChatStore,
    room_id: &str,
    seq: i64,
    at_ms: i64,
    limits: &ChatLimits,
) -> Result<bool, StoreError> {
    if !store.message_exists(room_id, seq)? {
        return Ok(false);
    }
    store.mark_silence_noted(room_id, seq)?;
    store.append_message(
        NewChatMessage {
            room_id: room_id.to_string(),
            at_ms,
            from_kind: "system".to_string(),
            from_window_id: None,
            from_name: "system".to_string(),
            body: "no response was observed within the silence timeout".to_string(),
            mentions: Vec::new(),
            mentions_user: true,
            broadcast: false,
        },
        limits,
    )?;
    store.set_needs_user(room_id, true)?;
    Ok(true)
}

/// The room a request acts on, once [`resolve_room`] has decided it.
pub(super) struct ResolvedRoom {
    /// The room's identifier.
    room_id: String,
    /// The workspace the room stands for. `None` for `"main"`, which stands
    /// for no workspace and holds every participant.
    key: Option<WorkspaceKey>,
    /// The room's display name, resolved against every other room's name.
    /// `None` when the caller did not ask for one (`need_name == false`).
    name: Option<String>,
}

/// Why [`resolve_room`] refused to name a room. Carries no room contents, so
/// a refusal can never reveal that a room the caller may not reach exists.
#[derive(Debug)]
pub(super) enum RoomError {
    /// The request named a room this caller may not act on, or none exists.
    /// The text is the `invalid_argument` message to answer with.
    InvalidArgument(&'static str),
    /// Chat has no store on this daemon.
    Unsupported,
    /// The store could not be reached, or answered with a room whose key
    /// contradicts the one asked for. Fail-closed: the room is not named.
    Internal,
}

/// What a caller who may not have this room is told, kept identical for "you
/// are not in it", "it does not exist" and "that is not a room id at all" so
/// the answer never reveals which of the three it was. An approver, which is
/// allowed to see the whole listing anyway, is told plainly instead.
fn room_denial_message(origin: &RequestOrigin) -> &'static str {
    match origin {
        RequestOrigin::Approver => "unknown room",
        RequestOrigin::Window(_) | RequestOrigin::External => "not a member of that room",
    }
}

/// The row a room lookup has to make in the store, decided before it is
/// reached so the store work itself is one pass.
enum RoomLookup {
    /// `"main"`: its row is created by the schema, never by a caller.
    Main,
    /// The caller's own workspace room, created on first use.
    Own {
        room_id: String,
        kind: WorkspaceKind,
        key: String,
    },
    /// A room the caller named and is allowed to reach without owning it.
    Named { room_id: String },
}

/// What the store answered a [`RoomLookup`] with.
enum RoomLookupOutcome {
    Found {
        key: Option<WorkspaceKey>,
        listing: Option<Vec<chat_store::RoomRow>>,
    },
    NotFound,
}

/// A stored room row's workspace key, or `None` for a row that carries no
/// workspace binding (`"main"`).
fn row_workspace_key(row: &chat_store::RoomRow) -> Option<WorkspaceKey> {
    match (row.kind, row.key.as_ref()) {
        (Some(kind), Some(path)) => Some(WorkspaceKey {
            kind,
            path: path.clone(),
        }),
        _ => None,
    }
}

/// Decides which room a request acts on, and -- when `need_name` -- what that
/// room is called.
///
/// A window reaches its own workspace room (the default when `room_id` is
/// omitted) and `"main"`, and nothing else. An approver reaches every room
/// that exists, defaulting to `"main"`. An unidentified caller reaches
/// `"main"` only. A requested id that is not `"main"` and not a well-formed
/// room token is refused before the store is touched at all.
///
/// The row is in place by the time this returns: the caller's own room is
/// created here if it did not exist, inside the same store pass that reads
/// the listing `need_name` needs.
///
/// # Errors
///
/// [`RoomError::InvalidArgument`] when the caller may not reach the room it
/// named (or no such room exists), [`RoomError::Unsupported`] when chat has
/// no store, and [`RoomError::Internal`] when the store could not be read,
/// when the requesting window is no longer registered (its key is unknown),
/// or when the room id the caller's own key maps to is already bound to a
/// different workspace.
pub(super) async fn resolve_room(
    chat: &ChatRuntime,
    origin: &RequestOrigin,
    requested: Option<&str>,
    registry: &WindowRegistry,
    need_name: bool,
) -> Result<ResolvedRoom, RoomError> {
    let denial = room_denial_message(origin);
    if let Some(requested) = requested
        && requested != MAIN_ROOM_ID
        && !is_valid_token(requested)
    {
        return Err(RoomError::InvalidArgument(denial));
    }

    let own = match origin {
        RequestOrigin::Window(window_id) => {
            let Some(entry) = registry.get(window_id) else {
                // The window this connection speaks for is gone from the
                // registry, so its room cannot be known. Fail closed rather
                // than guess at one.
                return Err(RoomError::Internal);
            };
            let key = entry.workspace().clone();
            Some((workspace::room_id_for(&key), key))
        }
        RequestOrigin::Approver | RequestOrigin::External => None,
    };

    let lookup = match (requested, &own) {
        (None | Some(MAIN_ROOM_ID), None) | (Some(MAIN_ROOM_ID), Some(_)) => RoomLookup::Main,
        (None, Some((room_id, key))) => own_lookup(room_id, key),
        (Some(requested), Some((room_id, key))) => {
            if requested == room_id {
                own_lookup(room_id, key)
            } else {
                return Err(RoomError::InvalidArgument(denial));
            }
        }
        (Some(requested), None) => match origin {
            RequestOrigin::Approver => RoomLookup::Named {
                room_id: requested.to_string(),
            },
            // An unidentified caller belongs to no workspace, so every room
            // but "main" is somebody else's.
            RequestOrigin::Window(_) | RequestOrigin::External => {
                return Err(RoomError::InvalidArgument(denial));
            }
        },
    };

    let room_id = match &lookup {
        RoomLookup::Main => MAIN_ROOM_ID.to_string(),
        RoomLookup::Own { room_id, .. } | RoomLookup::Named { room_id } => room_id.clone(),
    };
    let now_ms = i64::try_from(now_ms()).unwrap_or(i64::MAX);
    let outcome = chat
        .store
        .with_store(move |store| -> Result<RoomLookupOutcome, StoreError> {
            let key = match &lookup {
                RoomLookup::Main => None,
                RoomLookup::Own { room_id, kind, key } => {
                    // Room creation belongs here, after the ownership check above.
                    #[allow(clippy::disallowed_methods)]
                    store.ensure_room(room_id, *kind, key, now_ms)?;
                    Some(WorkspaceKey {
                        kind: *kind,
                        path: key.clone(),
                    })
                }
                RoomLookup::Named { room_id } => {
                    let Some(row) = store.find_room(room_id)? else {
                        return Ok(RoomLookupOutcome::NotFound);
                    };
                    row_workspace_key(&row)
                }
            };
            // Read after the write, so a room created just above is already
            // part of the listing its own name is resolved against.
            let listing = if need_name {
                Some(store.list_rooms()?)
            } else {
                None
            };
            Ok(RoomLookupOutcome::Found { key, listing })
        })
        .await;

    let (key, listing) = match outcome {
        Ok(Ok(RoomLookupOutcome::Found { key, listing })) => (key, listing),
        Ok(Ok(RoomLookupOutcome::NotFound)) => {
            return Err(RoomError::InvalidArgument(denial));
        }
        Ok(Err(store_err)) => {
            tracing::warn!(error = %store_err, "chat room resolution failed");
            return Err(RoomError::Internal);
        }
        Err(StoreAccessError::Disabled) => return Err(RoomError::Unsupported),
        Err(StoreAccessError::Failed(join_err)) => {
            tracing::warn!(error = %join_err, "chat store access task failed");
            return Err(RoomError::Internal);
        }
    };

    let name = listing.map(|rows| display_name_in(&rows, &room_id, key.as_ref()));
    Ok(ResolvedRoom { room_id, key, name })
}

/// The [`RoomLookup`] for a caller's own workspace room.
fn own_lookup(room_id: &str, key: &WorkspaceKey) -> RoomLookup {
    RoomLookup::Own {
        room_id: room_id.to_string(),
        kind: key.kind,
        key: key.path.clone(),
    }
}

/// `room_id`'s display name within `rows`: the collision-resolved name every
/// room in the listing gets. A row that is somehow not in the
/// listing falls back to its own undecorated name, which is what it would
/// have been called had it been there alone.
fn display_name_in(
    rows: &[chat_store::RoomRow],
    room_id: &str,
    key: Option<&WorkspaceKey>,
) -> String {
    let listed: Vec<(String, Option<WorkspaceKey>)> = rows
        .iter()
        .map(|row| (row.room_id.clone(), row_workspace_key(row)))
        .collect();
    let names = workspace::disambiguate(&listed);
    listed
        .iter()
        .position(|(id, _)| id == room_id)
        .and_then(|index| names.get(index).cloned())
        .unwrap_or_else(|| key.map_or_else(|| room_id.to_string(), workspace::display_name_for))
}

/// Whether a window is a chat participant: what the daemon sends of its own
/// accord can reach its CLI (`delivers`), and its detection definition's name
/// is not in `exclude`.
fn is_participant(delivers: bool, detection_definition: Option<&str>, exclude: &[String]) -> bool {
    delivers
        && detection_definition.is_none_or(|name| !exclude.iter().any(|excluded| excluded == name))
}

/// Everyone in `room`: a current chat
/// participant -- not `Exited`, and [`is_participant`] against whether it
/// delivers and its resolved detection definition -- whose own workspace is
/// the room's. `"main"` stands for no workspace, so every participant is in
/// it.
// The one reader of the registry's participant list, so every roster applies this rule.
#[allow(clippy::disallowed_methods)]
pub(super) fn room_members(
    registry: &WindowRegistry,
    exclude: &[String],
    room: &ResolvedRoom,
) -> Vec<WindowParticipation> {
    registry
        .list_participation()
        .into_iter()
        .filter(|p| p.status != WindowStatus::Exited)
        .filter(|p| is_participant(p.delivers, p.detection_definition.as_deref(), exclude))
        .filter(|p| room.key.as_ref().is_none_or(|key| &p.workspace == key))
        .collect()
}

/// The wire spelling of a [`room::HaltReason`], the same vocabulary
/// [`ChatHalt::reason`] carries.
fn chat_halt_reason_str(reason: room::HaltReason) -> &'static str {
    match reason {
        room::HaltReason::Repetition => "repetition",
        room::HaltReason::Monologue => "monologue",
        room::HaltReason::TurnBudget => "turn_budget",
    }
}

/// `(from_kind, from_name, from_window_id, envelope_from)` for the poster
/// named by `origin`.
fn poster_identity(
    registry: &WindowRegistry,
    origin: &RequestOrigin,
) -> (String, String, Option<String>, room::ChatEnvelopeFrom) {
    match origin {
        RequestOrigin::Window(window_id) => {
            let name = window_info(registry, window_id)
                .map(|window| window.name)
                .unwrap_or_else(|| window_id.clone());
            (
                "window".to_string(),
                name.clone(),
                Some(window_id.clone()),
                room::ChatEnvelopeFrom::Window {
                    window_id: window_id.clone(),
                    name,
                },
            )
        }
        RequestOrigin::Approver => (
            "user".to_string(),
            "user".to_string(),
            None,
            room::ChatEnvelopeFrom::User,
        ),
        RequestOrigin::External => (
            "external".to_string(),
            "external".to_string(),
            None,
            room::ChatEnvelopeFrom::External,
        ),
    }
}

/// A validated, envelope-composed chat post, everything [`complete_post`]
/// needs once the gate has let it through (or an approver has).
struct PendingChatPost {
    room_id: String,
    /// The room's display name as of the moment it was resolved, for the
    /// approver's dialog.
    room_name: String,
    body: String,
    from_kind: String,
    from_name: String,
    from_window_id: Option<String>,
    targets: Vec<String>,
    mentions_user: bool,
    unresolved: Vec<String>,
    envelope: String,
    tool: DestructiveTool,
}

/// Settles a `@parent` mention against the poster's parent, as the report
/// path decides it ([`report::resolve_parent`]). The parent is addressed only
/// when it is a member of this room, like any other mention; otherwise
/// `parent` joins the unresolved mentions.
async fn resolve_parent_mention(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    from_window_id: Option<&str>,
    roster: &[room::RosterEntry],
    mentions: &mut room::Mentions,
) {
    if !mentions.parent {
        return;
    }
    let parent = match from_window_id {
        Some(window_id) => report::resolve_parent(ledger, registry, window_id).await,
        None => None,
    };
    // A plan owner may nominate its own task window; a mention of oneself is
    // dropped without becoming unresolved, as `parse_mentions` does, whether or
    // not the poster takes part in this room.
    if parent.is_some() && parent.as_deref() == from_window_id {
        return;
    }
    match parent.filter(|parent| roster.iter().any(|entry| &entry.window_id == parent)) {
        Some(parent) => {
            if !mentions.targets.contains(&parent) {
                mentions.targets.push(parent);
            }
        }
        None => {
            if !mentions
                .unresolved
                .iter()
                .any(|raw| raw == room::PARENT_MENTION)
            {
                mentions.unresolved.push(room::PARENT_MENTION.to_string());
            }
        }
    }
}

/// Installs the window-opened hook every chat participant gets: settles the
/// window's own room, so the room exists whether or not the window's CLI ever
/// asks for anything. The intro itself is not sent: the window's start-up door
/// carries it ([`ChatRuntime::intro_for`]). A no-op call when chat is
/// disabled.
pub fn install_intro_hook(registry: &Arc<WindowRegistry>, chat: Arc<ChatRuntime>) {
    let registry_weak = Arc::downgrade(registry);
    registry.set_window_opened_hook(Box::new(move |entry: &Arc<WindowEntry>| {
        if !chat.store.is_enabled() {
            return;
        }
        if !is_participant(
            entry.delivers(),
            entry.detection_definition(),
            &chat.exclude_detections,
        ) {
            return;
        }
        let origin = RequestOrigin::Window(entry.window_id().to_string());
        let chat = Arc::clone(&chat);
        let registry_weak = registry_weak.clone();
        tokio::spawn(async move {
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            match resolve_room(&chat, &origin, None, &registry, false).await {
                // The room's row may have been created just now, after the
                // registration the participation watch already reacted to.
                Ok(_) => registry.announce_participation_change(),
                Err(err) => {
                    tracing::warn!(error = ?err, "could not settle a new window's chat room");
                }
            }
        });
    }));
}

/// One room as the participation watch compares it: the room, its display
/// name and who is in it.
#[derive(PartialEq, Eq)]
struct RoomRoster {
    room_id: String,
    name: String,
    members: Vec<(String, String)>,
}

/// Every room with its members, built from the same [`room_members`] rule
/// every roster uses. `None` when the store could not list the rooms.
async fn room_rosters(chat: &ChatRuntime, registry: &WindowRegistry) -> Option<Vec<RoomRoster>> {
    let Ok(Ok(rows)) = chat.store.with_store(|store| store.list_rooms()).await else {
        return None;
    };
    Some(
        rows.iter()
            .map(|row| {
                let resolved = ResolvedRoom {
                    room_id: row.room_id.clone(),
                    key: row_workspace_key(row),
                    name: None,
                };
                RoomRoster {
                    name: display_name_in(&rows, &row.room_id, resolved.key.as_ref()),
                    members: room_members(registry, &chat.exclude_detections, &resolved)
                        .into_iter()
                        .map(|member| (member.window_id, member.name))
                        .collect(),
                    room_id: row.room_id.clone(),
                }
            })
            .collect(),
    )
}

/// Tells every approver, by [`DaemonMessage::ChatParticipantsChanged`], when
/// the rooms or their members differ from what it last saw. The comparison
/// runs after the change is already visible, so a `read_chat` made on
/// receiving the push reflects it. A no-op when chat has no store.
pub(super) fn spawn_participation_watch(
    chat: Arc<ChatRuntime>,
    registry: Arc<WindowRegistry>,
    broker: Arc<ApprovalBroker>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if !chat.store.is_enabled() {
        return;
    }
    // Subscribed before the first listing, so a change in between is not lost.
    let mut changes = registry.subscribe_participation();
    tokio::spawn(async move {
        let mut last = room_rosters(&chat, &registry).await;
        loop {
            if *shutdown.borrow() {
                return;
            }
            tokio::select! {
                woke = changes.changed() => {
                    if woke.is_err() {
                        return;
                    }
                }
                // Shutdown only ever turns true, and a dropped sender means the
                // server is gone: either way the watch ends.
                _ = shutdown.changed() => return,
            }
            let Some(now) = room_rosters(&chat, &registry).await else {
                continue;
            };
            if last.as_ref() != Some(&now) {
                last = Some(now);
                broker.notify_approvers(&DaemonMessage::ChatParticipantsChanged);
            }
        }
    });
}

/// Posts a message to the room, addressed to the window(s) its `@` mentions
/// resolve to.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_send_chat(
    chat: &Arc<ChatRuntime>,
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    policy: &PolicyCell,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    room_id: Option<String>,
    body: String,
) {
    post_chat(
        chat, ledger, registry, broker, config, policy, outbound, state, room_id, body, false,
    )
    .await;
}

/// Posts a message to every participant but the poster.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_broadcast_chat(
    chat: &Arc<ChatRuntime>,
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    policy: &PolicyCell,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    room_id: Option<String>,
    body: String,
) {
    post_chat(
        chat, ledger, registry, broker, config, policy, outbound, state, room_id, body, true,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn post_chat(
    chat: &Arc<ChatRuntime>,
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    policy: &PolicyCell,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    room_id: Option<String>,
    body: String,
    broadcast: bool,
) {
    if !chat.store.is_enabled() {
        outbound.reply(&error_msg(
            "unsupported",
            "chat is not available on this daemon",
        ));
        return;
    }
    if let Err(err) = validate_chat_body(&body) {
        outbound.reply(&error_msg("invalid_argument", &err.to_string()));
        return;
    }

    // The room is settled before anything is composed or gated, but after
    // the body is judged: a post whose body could never be accepted must not
    // create a room row on its way to being refused.
    let resolved = match resolve_room(chat, &state.origin, room_id.as_deref(), registry, true).await
    {
        Ok(resolved) => resolved,
        Err(err) => {
            outbound.reply(&room_error_msg(&err));
            return;
        }
    };
    let room_id = resolved.room_id.clone();
    let room_name = resolved.name.clone().unwrap_or_else(|| room_id.clone());

    let tool = if broadcast {
        DestructiveTool::BroadcastChat
    } else {
        DestructiveTool::SendChat
    };

    let all_participants = room_members(registry, &chat.exclude_detections, &resolved);
    let roster: Vec<room::RosterEntry> = all_participants
        .iter()
        .map(|p| room::RosterEntry {
            window_id: p.window_id.clone(),
            name: p.name.clone(),
        })
        .collect();

    let (from_kind, from_name, from_window_id, envelope_from) =
        poster_identity(registry, &state.origin);
    let self_window_id = from_window_id.clone().unwrap_or_default();
    let mut mentions = room::parse_mentions(&body, &roster, &self_window_id);
    resolve_parent_mention(
        ledger,
        registry,
        from_window_id.as_deref(),
        &roster,
        &mut mentions,
    )
    .await;

    let targets: Vec<String> = if broadcast {
        all_participants
            .iter()
            .map(|p| p.window_id.clone())
            .filter(|id| Some(id.as_str()) != from_window_id.as_deref())
            .collect()
    } else {
        mentions.targets.clone()
    };

    let mentions_display: Vec<String> = if broadcast {
        Vec::new()
    } else {
        targets
            .iter()
            .filter_map(|id| {
                roster
                    .iter()
                    .find(|entry| &entry.window_id == id)
                    .map(|entry| entry.name.clone())
            })
            .collect()
    };

    let envelope = match room::compose_chat_envelope(
        &envelope_from,
        &room_id,
        &mentions_display,
        broadcast,
        &body,
    ) {
        Ok(text) => text,
        Err(err) => {
            outbound.reply(&error_msg("invalid_argument", &err.to_string()));
            return;
        }
    };

    let post = PendingChatPost {
        room_id,
        room_name,
        body,
        from_kind,
        from_name,
        from_window_id,
        targets,
        mentions_user: mentions.user,
        unresolved: mentions.unresolved,
        envelope,
        tool,
    };

    // Every origin runs the same gate, with no target to compare the
    // requester against: chat addresses a room, not a window, so `auto_approve`
    // is the only thing that can let a post through without a human.
    let policy_snapshot = policy.snapshot();
    match control::policy_gate(
        live_policy::policy_source(&policy_snapshot),
        tool,
        &state.origin,
        None,
    ) {
        Ok(GateVerdict::Allow) => {
            complete_post(chat, registry, broker, outbound, post).await;
        }
        Ok(GateVerdict::NeedsApproval) => {
            submit_for_approval(chat, registry, broker, config, outbound, state, post).await;
        }
        Err(reason) => {
            // Unreachable in practice: `chat.store` is `None` whenever the
            // playbook failed to load (checked above), so
            // `PlaybookBroken` never applies here. Kept exhaustive so a
            // future `DenyReason` cannot silently fall through unanswered.
            outbound.reply(&deny_msg(tool, reason));
        }
    }
}

async fn submit_for_approval(
    chat: &Arc<ChatRuntime>,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    config: &DaemonConfig,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    post: PendingChatPost,
) {
    let requester = requester_for(registry, &state.origin);
    let draft = ApprovalDraft {
        tool: post.tool,
        requester,
        target: None,
        text: Some(post.envelope.clone()),
        room: Some(ApprovalRoomRef {
            room_id: post.room_id.clone(),
            name: post.room_name.clone(),
        }),
    };
    match broker.submit(draft, state.conn_id, approval_limits(config)) {
        Ok(mut ticket) => {
            let chat = Arc::clone(chat);
            let registry = Arc::clone(registry);
            let broker = Arc::clone(broker);
            let config = config.clone();
            let outbound = Arc::clone(outbound);
            let RequestWatch {
                left: mut closed, ..
            } = RequestWatch::connection_only(registry.as_ref(), &outbound, state);
            let deadline = Duration::from_secs(config.approval_timeout_secs);
            tokio::spawn(async move {
                match await_approval(&mut ticket, deadline, &mut closed).await {
                    ApprovalEnd::Approved => {
                        complete_post(&chat, &registry, &broker, &outbound, post).await;
                    }
                    ApprovalEnd::Refused(message) => outbound.reply(&message),
                    // A poster whose window was interrupted is still there to
                    // be told nothing was posted.
                    ApprovalEnd::RequesterGone(left) if left.can_answer() => {
                        outbound.reply(&super::withdrawn_msg(left, "nothing was posted"));
                    }
                    ApprovalEnd::RequesterGone(_) => {}
                }
            });
        }
        Err(SubmitError::NoApprover) => outbound.reply(&deny_no_approver(post.tool)),
        Err(SubmitError::TooManyPending | SubmitError::TooManyPendingForConn) => {
            outbound.reply(&error_msg(
                "resource_exhausted",
                "too many requests are already awaiting approval",
            ));
        }
    }
}

async fn complete_post(
    chat: &Arc<ChatRuntime>,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    outbound: &Arc<Outbound>,
    post: PendingChatPost,
) {
    let PendingChatPost {
        room_id,
        room_name: _,
        body,
        from_kind,
        from_name,
        from_window_id,
        targets,
        mentions_user,
        unresolved,
        envelope,
        tool,
    } = post;
    let is_user_post = from_kind == "user";
    let at_ms = now_ms();
    let new_message = NewChatMessage {
        room_id: room_id.clone(),
        at_ms: i64::try_from(at_ms).unwrap_or(i64::MAX),
        from_kind,
        from_window_id,
        from_name,
        body,
        mentions: targets.clone(),
        mentions_user,
        broadcast: matches!(tool, DestructiveTool::BroadcastChat),
    };
    let landing = Landing {
        registry: Arc::clone(registry),
        room_id: room_id.clone(),
        envelope,
        targets: targets.clone(),
        #[cfg(feature = "fault-injection")]
        faults: chat.faults.clone(),
    };
    let effects = PostEffects {
        user_post: is_user_post,
        mentions_user,
    };
    let limits = chat.limits;
    let posted = chat
        .store
        .with_store(move |store| {
            post_in_one_transaction(store, new_message, &limits, effects, landing)
        })
        .await;

    let Posting {
        posted,
        settled,
        ends,
    } = match posted {
        Ok(Ok(posting)) => posting,
        Ok(Err(StoreError::RoomGone { .. })) => {
            outbound.reply(&error_msg("invalid_argument", "unknown room"));
            return;
        }
        Ok(Err(store_err)) => {
            tracing::warn!(error = %store_err, "chat store append failed");
            outbound.reply(&error_msg("internal", "failed to record the chat message"));
            return;
        }
        Err(StoreAccessError::Disabled) => {
            outbound.reply(&error_msg(
                "unsupported",
                "chat is not available on this daemon",
            ));
            return;
        }
        Err(StoreAccessError::Failed(join_err)) => {
            tracing::warn!(error = %join_err, "chat store access task failed");
            outbound.reply(&error_msg("internal", "failed to record the chat message"));
            return;
        }
    };
    let seq = posted.seq;
    let halted_now = posted.halted;
    let needs_user_now = posted.needs_user;

    outbound.reply(&DaemonMessage::ChatPosted {
        room_id: room_id.clone(),
        seq: u64::try_from(seq).unwrap_or(0),
        targets: targets.clone(),
        user: mentions_user,
        unresolved,
        halted: halted_now,
    });
    // Every post is announced exactly once here unless it has destinations,
    // whose per-delivery pushes below announce it instead. An AI's `@user`
    // mention on a room that is not halted is announced even then.
    let announces_user = !is_user_post && !halted_now && mentions_user;
    if announces_user || targets.is_empty() {
        broker.notify_approvers(&DaemonMessage::ChatChanged {
            room_id: room_id.clone(),
            seq: u64::try_from(seq).unwrap_or(0),
            needs_user: announces_user || needs_user_now,
        });
    }
    // The deliveries the post settled as it was written still announce.
    for _ in &settled {
        broker.notify_approvers(&DaemonMessage::ChatChanged {
            room_id: room_id.clone(),
            seq: u64::try_from(seq).unwrap_or(0),
            needs_user: needs_user_now,
        });
    }
    for (target_id, end) in ends {
        let chat = Arc::clone(chat);
        let broker = Arc::clone(broker);
        let room_id = room_id.clone();
        tokio::spawn(async move {
            record_when_left(&chat, &broker, &room_id, seq, &target_id, end).await;
        });
    }
    if !halted_now && !targets.is_empty() {
        spawn_silence_watch(Arc::clone(chat), Arc::clone(broker), room_id, seq, targets);
    }
}

/// The inbox key a chat post travels under: the delivery puts it in under
/// this key, and a clear takes it back out by the same one.
fn chat_parcel_key(room_id: &str, seq: i64) -> String {
    format!("{CHAT_PARCEL_PREFIX}{room_id}:{seq}")
}

/// What every chat parcel's key starts with.
const CHAT_PARCEL_PREFIX: &str = "chat:";

/// What a post needs to reach its destinations' inboxes.
struct Landing {
    registry: Arc<WindowRegistry>,
    room_id: String,
    envelope: String,
    targets: Vec<String>,
    #[cfg(feature = "fault-injection")]
    faults: crate::fault_point::FaultPoints,
}

/// A committed post, and where it stands in its destinations' inboxes.
struct Posting {
    posted: chat_store::Posted,
    /// The destinations whose delivery the post settled as it was written.
    settled: Vec<String>,
    /// How each destination's inbox will tell the parcel left it.
    ends: Vec<(String, oneshot::Receiver<ParcelEnd>)>,
}

/// A destination whose inbox took the post in, its row not yet written.
struct Landed {
    target_id: String,
    entry: Arc<WindowEntry>,
    deposited: Deposited,
}

/// Stores the post, puts it in its destinations' inboxes and writes their
/// rows in one transaction: a daemon that ends after the post is stored has
/// every destination's parcel kept for its next daemon, and no inbox shows a
/// post that is not stored. The capacity of an inbox (`Refused::Full`) and the
/// room's halt are decided at the post's seq, and a destination the post does
/// not reach has its delivery settled in the same transaction. Once it has
/// committed the parcels show.
fn post_in_one_transaction(
    store: &mut ChatStore,
    message: NewChatMessage,
    limits: &ChatLimits,
    effects: PostEffects,
    landing: Landing,
) -> Result<Posting, StoreError> {
    let Landing {
        registry,
        room_id,
        envelope,
        targets,
        #[cfg(feature = "fault-injection")]
        faults,
    } = landing;
    let mut landed: Vec<Landed> = Vec::new();
    // Dropped unsettled, a parcel's hold counts as a write that failed.
    let mut unwritten: Vec<(String, Unwritten)> = Vec::new();
    let mut settled: Vec<String> = Vec::new();
    let posted = store.append_and_place(message, limits, effects, |seq, halted| {
        let mut shares = Vec::new();
        for target_id in &targets {
            let settled_as = |outcome| Share::Settled {
                window_id: target_id.clone(),
                outcome,
            };
            if halted {
                shares.push(settled_as(DeliveryOutcome::UndeliveredHalted));
                settled.push(target_id.clone());
                continue;
            }
            let Some(entry) = registry.get(target_id) else {
                shares.push(settled_as(DeliveryOutcome::UndeliveredUnknown(None)));
                settled.push(target_id.clone());
                continue;
            };
            // The post's id is its parcel's key, which its seq decides: the
            // kept parcel carries it, so a next daemon hands it out the same.
            let key = chat_parcel_key(&room_id, seq);
            let parcel = Parcel::new(key.clone(), with_envelope_id(&envelope, &key));
            let delivery = ChatDelivery {
                room_id: room_id.clone(),
                seq,
            };
            let (deposited, row) = entry.inbox().deposit_with_row(parcel, delivery);
            if deposited.end.is_none() {
                let outcome = match notice_queue::after_deposit(&registry, &entry, deposited) {
                    Err(notice_queue::Refused::Full) => DeliveryOutcome::UndeliveredExhausted,
                    Err(notice_queue::Refused::Ended) | Ok(_) => {
                        DeliveryOutcome::UndeliveredUnknown(None)
                    }
                };
                shares.push(settled_as(outcome));
                settled.push(target_id.clone());
                continue;
            }
            if let Some((parcel, hold)) = row {
                #[cfg(feature = "fault-injection")]
                if faults.write_fails() {
                    return Err(StoreError::Sqlite(
                        crate::fault_point::simulated_write_failure(),
                    ));
                }
                shares.push(Share::Row {
                    window_id: target_id.clone(),
                    parcel,
                });
                unwritten.push((target_id.clone(), hold));
            }
            landed.push(Landed {
                target_id: target_id.clone(),
                entry,
                deposited,
            });
        }
        Ok(shares)
    })?;

    #[cfg(feature = "fault-injection")]
    if !posted.halted && !targets.is_empty() {
        faults.reach(crate::fault_point::FaultPoint::ChatPostCommitted);
    }
    for (target_id, hold) in unwritten {
        let outcome = if posted.unkept.contains(&target_id) {
            SaveOutcome::NoRow
        } else {
            SaveOutcome::Saved
        };
        hold.settle(outcome);
    }
    settled.extend(posted.unkept.iter().cloned());
    let mut ends = Vec::new();
    for Landed {
        target_id,
        entry,
        deposited,
    } in landed
    {
        if posted.unkept.contains(&target_id) {
            continue;
        }
        if let Ok(end) = notice_queue::after_deposit(&registry, &entry, deposited) {
            ends.push((target_id, end));
        }
    }
    Ok(Posting {
        posted,
        settled,
        ends,
    })
}

/// Records how the post `seq` of `room_id` left `target_id`'s inbox, once
/// `end` says: the one place a delivery in an inbox is settled, whether the
/// post was put in by this daemon or kept from a previous one.
async fn record_when_left(
    chat: &ChatRuntime,
    broker: &Arc<ApprovalBroker>,
    room_id: &str,
    seq: i64,
    target_id: &str,
    end: oneshot::Receiver<ParcelEnd>,
) {
    let outcome = match end.await {
        // The door's confirmation recorded it with the row's removal.
        Ok(ParcelEnd::TakenRecorded) => {
            chat.announce_change(broker, room_id, seq).await;
            return;
        }
        Ok(ParcelEnd::Taken) => DeliveryOutcome::Taken,
        // The next daemon settles it.
        Ok(ParcelEnd::Stopped) => return,
        Ok(ParcelEnd::Closed) | Err(_) => DeliveryOutcome::UndeliveredUnknown(None),
    };
    // Taken is the end that leaves the parcel's kept row to the delivery's
    // settlement; a closed window takes its rows with it.
    if matches!(outcome, DeliveryOutcome::Taken) {
        chat.settle_parcel_end_and_notify(broker, room_id, seq, target_id, outcome)
            .await;
    } else {
        chat.record_delivery_and_notify(broker, room_id, seq, target_id, outcome)
            .await;
    }
}

/// The chat delivery a chat parcel's `key` names: the inverse of
/// [`chat_parcel_key`].
fn chat_parcel_of(key: &str) -> Option<ChatDelivery> {
    let (room_id, seq) = key.strip_prefix(CHAT_PARCEL_PREFIX)?.rsplit_once(':')?;
    Some(ChatDelivery {
        room_id: room_id.to_owned(),
        seq: seq.parse().ok()?,
    })
}

/// Settles, as [`settle_placement`] does, the delivery of every chat post
/// a window's inbox read back from the store: a previous daemon put it in, and
/// it is still `pending` in the room. Runs for every window taken back, before
/// its doors can be reached.
pub(super) fn settle_kept_deliveries(
    registry: &Arc<WindowRegistry>,
    chat: &Arc<ChatRuntime>,
    broker: &Arc<ApprovalBroker>,
) {
    let chat = Arc::clone(chat);
    let broker = Arc::clone(broker);
    registry.add_kept_parcels_hook(Arc::new(move |entry: &Arc<WindowEntry>| {
        if !chat.store.is_enabled() {
            return;
        }
        for (ChatDelivery { room_id, seq }, end) in entry.inbox().adopt_kept(chat_parcel_of) {
            let chat = Arc::clone(&chat);
            let broker = Arc::clone(&broker);
            let target_id = entry.window_id().to_owned();
            tokio::spawn(async move {
                record_when_left(&chat, &broker, &room_id, seq, &target_id, end).await;
            });
        }
    }));
}

fn spawn_silence_watch(
    chat: Arc<ChatRuntime>,
    broker: Arc<ApprovalBroker>,
    room_id: String,
    seq: i64,
    targets: Vec<String>,
) {
    let silence_timeout_ms = chat.limits.silence_timeout_secs.saturating_mul(1000);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(silence_timeout_ms)).await;
        chat.evaluate_silence(&broker, &room_id, seq, &targets)
            .await;
    });
}

/// Reads a page of the room's history, starting after the caller's `after_seq`.
pub(super) async fn handle_read_chat(
    chat: &Arc<ChatRuntime>,
    registry: &Arc<WindowRegistry>,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    room_id: Option<String>,
    after_seq: Option<u64>,
    limit: Option<u32>,
) {
    if !chat.store.is_enabled() {
        outbound.reply(&error_msg(
            "unsupported",
            "chat is not available on this daemon",
        ));
        return;
    }
    let limit = limit.unwrap_or(50);
    if limit == 0 || limit > CHAT_READ_LIMIT_MAX {
        outbound.reply(&error_msg(
            "invalid_argument",
            &format!("limit must be between 1 and {CHAT_READ_LIMIT_MAX}"),
        ));
        return;
    }
    if after_seq.is_none() && matches!(state.origin, RequestOrigin::External) {
        outbound.reply(&error_msg(
            "invalid_argument",
            "after_seq is required for an unidentified connection",
        ));
        return;
    }
    // A reader is never shown a name, so the listing is not read for it.
    let resolved =
        match resolve_room(chat, &state.origin, room_id.as_deref(), registry, false).await {
            Ok(resolved) => resolved,
            Err(err) => {
                outbound.reply(&room_error_msg(&err));
                return;
            }
        };
    let room_id = resolved.room_id.clone();

    let limit_i64 = i64::from(limit);
    let origin = state.origin.clone();
    let room_id_for_store = room_id.clone();
    let room_id_for_status = room_id.clone();
    type ReadAndStatus = (
        chat_store::ReadAfter,
        chat_store::RoomStatus,
        HashMap<i64, Vec<chat_store::DeliveryRow>>,
    );
    let fetched = chat
        .store
        .with_store(move |store| -> Result<ReadAndStatus, StoreError> {
            let read = match (after_seq, &origin) {
                (Some(seq), _) => store.read_after(
                    &room_id_for_store,
                    i64::try_from(seq).unwrap_or(i64::MAX),
                    limit_i64,
                ),
                (None, RequestOrigin::Window(window_id)) => {
                    let last = store
                        .last_window_seq(&room_id_for_store, window_id)?
                        .unwrap_or(0);
                    store.read_after(&room_id_for_store, last, limit_i64)
                }
                (None, RequestOrigin::Approver) => {
                    let messages = store.read_tail(&room_id_for_store, limit_i64)?;
                    let next_after_seq = messages.last().map_or(0, |message| message.seq);
                    Ok(chat_store::ReadAfter {
                        messages,
                        next_after_seq,
                    })
                }
                // Refused above: after_seq is required for an unidentified
                // connection.
                (None, RequestOrigin::External) => Ok(chat_store::ReadAfter {
                    messages: Vec::new(),
                    next_after_seq: 0,
                }),
            }?;
            let status = store.room_status(&room_id_for_status)?;
            let deliveries = store.deliveries_by_room(&room_id_for_status)?;
            Ok((read, status, deliveries))
        })
        .await;

    let (read, status, deliveries) = match fetched {
        Ok(Ok(fetched)) => fetched,
        Ok(Err(store_err)) => {
            tracing::warn!(error = %store_err, "chat store read failed");
            outbound.reply(&error_msg("internal", "failed to read the chat room"));
            return;
        }
        Err(StoreAccessError::Disabled) => {
            outbound.reply(&error_msg(
                "unsupported",
                "chat is not available on this daemon",
            ));
            return;
        }
        Err(StoreAccessError::Failed(join_err)) => {
            tracing::warn!(error = %join_err, "chat store access task failed");
            outbound.reply(&error_msg("internal", "failed to read the chat room"));
            return;
        }
    };

    let roster: Vec<ChatParticipant> = room_members(registry, &chat.exclude_detections, &resolved)
        .into_iter()
        .map(|p| ChatParticipant {
            window_id: p.window_id,
            name: p.name,
        })
        .collect();

    let messages: Vec<ChatMessage> = read
        .messages
        .into_iter()
        .map(|row| {
            let mut mentions = row.mentions.clone();
            if row.mentions_user {
                mentions.push("user".to_string());
            }
            let message_deliveries = deliveries
                .get(&row.seq)
                .map(|entries| {
                    entries
                        .iter()
                        .map(|(window_id, outcome)| ChatDeliveryInfo {
                            window_id: window_id.clone(),
                            state: outcome.state().wire_str().to_string(),
                            note: match outcome {
                                DeliveryOutcome::Pending => registry
                                    .get(window_id)
                                    .and_then(|entry| super::waiting_note_for(&entry, false, true)),
                                _ => None,
                            },
                            unknown_reason: outcome.unknown_reason(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            ChatMessage {
                seq: u64::try_from(row.seq).unwrap_or(0),
                at_ms: u64::try_from(row.at_ms).unwrap_or(0),
                from_kind: row.from_kind,
                from_window_id: row.from_window_id,
                from_name: row.from_name,
                body: row.body,
                mentions,
                broadcast: row.broadcast,
                deliveries: message_deliveries,
            }
        })
        .collect();

    let halted = status.halted.map(|(reason, since_ms)| ChatHalt {
        reason: chat_halt_reason_str(reason).to_string(),
        since_ms: u64::try_from(since_ms).unwrap_or(0),
    });
    let needs_user = status.needs_user;

    outbound.reply(&DaemonMessage::ChatMessages {
        room_id,
        messages,
        next_after_seq: u64::try_from(read.next_after_seq).unwrap_or(0),
        halted,
        needs_user,
        roster,
    });
}

/// The reply a [`RoomError`] turns into. The `invalid_argument` text is the
/// one the error already chose, so the answer to "not your room", "no such
/// room" and "not a room id" stays one string for callers who may not know
/// which rooms exist.
fn room_error_msg(err: &RoomError) -> DaemonMessage {
    match err {
        RoomError::InvalidArgument(message) => error_msg("invalid_argument", message),
        RoomError::Unsupported => error_msg("unsupported", "chat is not available on this daemon"),
        RoomError::Internal => error_msg("internal", "failed to resolve the chat room"),
    }
}

/// Lists every room the daemon knows of, newest activity
/// first behind `"main"`.
///
/// Approver-only: the listing carries the display names of the workspaces
/// windows are open in, so a window or an unidentified caller is refused the
/// same way it is refused approver registration, before chat's own
/// availability is even considered.
pub(super) async fn handle_list_chat_rooms(
    chat: &Arc<ChatRuntime>,
    registry: &Arc<WindowRegistry>,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&error_msg(
            "auth_failed",
            "this connection may not list chat rooms",
        ));
        return;
    }

    let listed = chat.store.with_store(|store| store.list_rooms()).await;
    let rows = match listed {
        Ok(Ok(rows)) => rows,
        Ok(Err(store_err)) => {
            tracing::warn!(error = %store_err, "chat room listing failed");
            outbound.reply(&error_msg("internal", "failed to list the chat rooms"));
            return;
        }
        Err(StoreAccessError::Disabled) => {
            outbound.reply(&error_msg(
                "unsupported",
                "chat is not available on this daemon",
            ));
            return;
        }
        Err(StoreAccessError::Failed(join_err)) => {
            tracing::warn!(error = %join_err, "chat store access task failed");
            outbound.reply(&error_msg("internal", "failed to list the chat rooms"));
            return;
        }
    };

    let rooms: Vec<ChatRoomInfo> = rows
        .iter()
        .map(|row| {
            let resolved = ResolvedRoom {
                room_id: row.room_id.clone(),
                key: row_workspace_key(row),
                name: None,
            };
            let members = room_members(registry, &chat.exclude_detections, &resolved).len();
            ChatRoomInfo {
                name: display_name_in(&rows, &row.room_id, resolved.key.as_ref()),
                room_id: row.room_id.clone(),
                participant_count: u32::try_from(members).unwrap_or(u32::MAX),
                needs_user: row.needs_user,
                halted: row.halted.map(|(reason, since_ms)| ChatHalt {
                    reason: chat_halt_reason_str(reason).to_string(),
                    since_ms: u64::try_from(since_ms).unwrap_or(0),
                }),
                last_at_ms: row
                    .last_at_ms
                    .map(|at_ms| u64::try_from(at_ms).unwrap_or(0)),
                has_open_windows: registry.has_window_in_room(&row.room_id),
            }
        })
        .collect();

    outbound.reply(&DaemonMessage::ChatRooms { rooms });
}

/// Which of the two room removals a request asks for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RoomRemoval {
    /// Empty the room; the room stays.
    ClearHistory,
    /// Remove the room itself.
    DeleteRoom,
}

/// How a removal ended inside the store pass.
enum RemovalEnd {
    NoSuchRoom,
    InUse,
    Done,
}

/// Empties a room's history on an approver's request. See
/// [`handle_room_removal`].
pub(super) async fn handle_clear_chat_room(
    chat: &Arc<ChatRuntime>,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    room_id: String,
) {
    handle_room_removal(
        chat,
        registry,
        broker,
        outbound,
        state,
        room_id,
        RoomRemoval::ClearHistory,
    )
    .await;
}

/// Deletes a room no window belongs to on an approver's request. See
/// [`handle_room_removal`].
pub(super) async fn handle_delete_chat_room(
    chat: &Arc<ChatRuntime>,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    room_id: String,
) {
    handle_room_removal(
        chat,
        registry,
        broker,
        outbound,
        state,
        room_id,
        RoomRemoval::DeleteRoom,
    )
    .await;
}

/// Clears or deletes a room. Approver-only, like the listing.
///
/// The store's removal and the taking back of every post still waiting in a
/// window's inbox happen in one store pass, the same pass that puts a post in
/// an inbox only while it is stored (`post_in_one_transaction`): whichever
/// runs first, a removed post never reaches a CLI. A post a CLI is already
/// taking is not stopped. On success every approver is pushed
/// [`DaemonMessage::ChatHistoryCleared`], and a deletion also raises the
/// participation signal so the room's disappearance is announced like any
/// other change of the rooms.
async fn handle_room_removal(
    chat: &Arc<ChatRuntime>,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    outbound: &Arc<Outbound>,
    state: &super::ConnectionState,
    room_id: String,
    removal: RoomRemoval,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&error_msg(
            "auth_failed",
            "this connection may not remove chat history or rooms",
        ));
        return;
    }
    if removal == RoomRemoval::DeleteRoom && room_id == MAIN_ROOM_ID {
        outbound.reply(&error_msg(
            "invalid_argument",
            "the main room cannot be deleted",
        ));
        return;
    }
    // Refused before the store is touched, the way `resolve_room` refuses
    // an id that cannot name a room.
    if room_id != MAIN_ROOM_ID && !is_valid_token(&room_id) {
        outbound.reply(&error_msg("invalid_argument", "unknown room"));
        return;
    }

    let store_registry = Arc::clone(registry);
    let store_room = room_id.clone();
    let ended = chat
        .store
        .with_store(move |store| -> Result<RemovalEnd, StoreError> {
            if store.find_room(&store_room)?.is_none() {
                return Ok(RemovalEnd::NoSuchRoom);
            }
            if removal == RoomRemoval::DeleteRoom && store_registry.has_window_in_room(&store_room)
            {
                return Ok(RemovalEnd::InUse);
            }
            let pending = match removal {
                RoomRemoval::ClearHistory => store.clear_history(&store_room)?,
                RoomRemoval::DeleteRoom => store.delete_room(&store_room)?,
            };
            for (seq, window_id) in pending {
                if let Some(entry) = store_registry.get(&window_id) {
                    // A post a door or a push already holds goes on to the CLI.
                    let _ = entry.inbox().withdraw(&chat_parcel_key(&store_room, seq));
                }
            }
            Ok(RemovalEnd::Done)
        })
        .await;

    match ended {
        Ok(Ok(RemovalEnd::Done)) => {}
        Ok(Ok(RemovalEnd::NoSuchRoom)) => {
            outbound.reply(&error_msg("invalid_argument", "unknown room"));
            return;
        }
        Ok(Ok(RemovalEnd::InUse)) => {
            outbound.reply(&error_msg(
                "room_in_use",
                "a window is open in that room, so it cannot be deleted",
            ));
            return;
        }
        Ok(Err(store_err)) => {
            tracing::warn!(error = %store_err, "chat room removal failed");
            outbound.reply(&error_msg("internal", "failed to remove the chat history"));
            return;
        }
        Err(StoreAccessError::Disabled) => {
            outbound.reply(&error_msg(
                "unsupported",
                "chat is not available on this daemon",
            ));
            return;
        }
        Err(StoreAccessError::Failed(join_err)) => {
            tracing::warn!(error = %join_err, "chat store access task failed");
            outbound.reply(&error_msg("internal", "failed to remove the chat history"));
            return;
        }
    }

    outbound.reply(&match removal {
        RoomRemoval::ClearHistory => DaemonMessage::ChatRoomCleared {
            room_id: room_id.clone(),
        },
        RoomRemoval::DeleteRoom => DaemonMessage::ChatRoomDeleted {
            room_id: room_id.clone(),
        },
    });
    broker.notify_approvers(&DaemonMessage::ChatHistoryCleared { room_id });
    if removal == RoomRemoval::DeleteRoom {
        registry.announce_participation_change();
    }
}

#[cfg(test)]
mod tests {

    use tokio::sync::watch;
    use tokio::time::Instant;
    use yagura_protocol::{DetectedStatus, UnknownReason};

    use super::*;
    use crate::inbox::{DeliverySpec, Inbox};
    use crate::waiter::StatusAt;

    const ROOM: &str = "r-alpha0000000001";

    fn limits() -> ChatLimits {
        ChatLimits {
            repetition_count: 1000,
            repetition_window: 1000,
            repetition_distance: 6,
            monologue_max: 1000,
            turn_message_max: 1000,
            silence_timeout_secs: 300,
            history_max: 5000,
        }
    }

    // Rooms are made through `resolve_room` in production; these cases need one without a request.
    #[allow(clippy::disallowed_methods)]
    fn store_with_room(tag: &str) -> ChatStore {
        let dir = yagura_test_support::paths::unique_temp_dir("yagura-daemon-chat-unit", tag);
        let mut store = ChatStore::open(&dir.join("chat.db")).expect("the store must open");
        store
            .ensure_room(ROOM, WorkspaceKind::Git, "/repo/alpha", 1_000)
            .expect("the room must be created");
        store
    }

    fn post(store: &mut ChatStore, mentions: &[&str]) -> i64 {
        store
            .append_message(
                NewChatMessage {
                    room_id: ROOM.to_string(),
                    at_ms: 2_000,
                    from_kind: "window".to_string(),
                    from_window_id: Some("w-a".to_string()),
                    from_name: "Alpha".to_string(),
                    body: "hello".to_string(),
                    mentions: mentions.iter().map(|id| (*id).to_string()).collect(),
                    mentions_user: false,
                    broadcast: false,
                },
                &limits(),
            )
            .expect("the post must be recorded")
            .seq
    }

    fn inbox() -> Inbox {
        let now = Instant::now();
        let (_tx, rx) = watch::channel(StatusAt {
            status: DetectedStatus::Unknown(UnknownReason::NotYetJudged),
            changed_at: now,
            evidence_since: now,
            generation: 0,
        });
        Inbox::new(8, DeliverySpec::default(), rx)
    }

    fn parcel_key(seq: i64) -> String {
        chat_parcel_key(ROOM, seq)
    }

    /// The post was deposited and the history cleared after: the clear pulls
    /// every parcel it reports pending back out of the inbox.
    #[test]
    fn a_post_cleared_after_it_is_deposited_is_pulled_back_out_of_the_inbox() {
        let mut store = store_with_room("clear-after-deposit");
        let seq = post(&mut store, &["w-b"]);
        let inbox = inbox();
        let (deposited, _saving) = inbox.deposit_saved(Parcel::new(parcel_key(seq), "ENVELOPE"));
        assert!(deposited.end.is_some());
        assert!(inbox.holds(&parcel_key(seq)));

        let pending = store.clear_history(ROOM).expect("the clear must succeed");
        for (seq, _window_id) in &pending {
            assert!(matches!(
                inbox.withdraw(&parcel_key(*seq)),
                crate::inbox::Withdrawal::Removed
            ));
        }

        assert_eq!(pending, vec![(seq, "w-b".to_string())]);
        assert_eq!(inbox.pending_count(), 0);
    }

    #[test]
    fn silence_is_noted_for_a_message_that_is_still_there() {
        let mut store = store_with_room("silence-present");
        let seq = post(&mut store, &["w-b"]);

        let wrote = note_silence(&mut store, ROOM, seq, 9_000, &limits())
            .expect("noting silence must succeed");

        assert!(wrote);
        let messages = store.read_tail(ROOM, 10).expect("the tail must read");
        assert_eq!(messages.len(), 2, "the system notice follows the post");
        assert!(messages[0].silence_noted);
        assert!(
            store
                .room_status(ROOM)
                .expect("the status must read")
                .needs_user
        );
    }

    #[test]
    fn silence_is_not_noted_for_a_message_that_was_cleared() {
        let mut store = store_with_room("silence-cleared");
        let seq = post(&mut store, &["w-b"]);
        store.clear_history(ROOM).expect("the clear must succeed");

        let wrote = note_silence(&mut store, ROOM, seq, 9_000, &limits())
            .expect("noting silence must succeed");

        assert!(!wrote, "the message is gone: nothing is written");
        assert!(
            store
                .read_tail(ROOM, 10)
                .expect("the tail must read")
                .is_empty()
        );
        assert!(
            !store
                .room_status(ROOM)
                .expect("the status must read")
                .needs_user
        );
    }

    #[test]
    fn silence_is_not_noted_for_a_room_that_was_deleted() {
        let mut store = store_with_room("silence-deleted");
        let seq = post(&mut store, &["w-b"]);
        store.delete_room(ROOM).expect("the delete must succeed");

        let wrote = note_silence(&mut store, ROOM, seq, 9_000, &limits())
            .expect("noting silence must succeed");

        assert!(!wrote);
        assert!(
            store
                .find_room(ROOM)
                .expect("the lookup must succeed")
                .is_none()
        );
    }
}
