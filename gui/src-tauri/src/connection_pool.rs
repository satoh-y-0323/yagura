//! Multi-window connection pool.
//!
//! A daemon connection is attached to at most one window, so
//! watching N windows takes N connections. This module holds them, plus the
//! one attach-less *control* connection that carries the window-scoped
//! requests (`list_windows`, `list_detections`, `open_window`, `close_window`,
//! `interrupt_window`, `rename_window`), and it is the only place that knows
//! which connection a window id routes to.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::Mutex;
use yagura_protocol::{
    ClientMessage, DefaultPalettes, LaunchOptionAlias, McpRegistrationAction, OnNameTaken,
    PlanInfo, PlanSummary, RoleEntry, TaskAssignment, TerminalSize, TicketInfo,
    TicketWorkspaceInfo, WindowInfo, WindowStatus,
};

use crate::commands::{
    ApprovalSettingsReply, ChatMessagesReply, ChatPostedReply, ChatRoomListReply,
    DaemonHealthReply, DetectionListReply, McpRegistrationRequestedReply,
    McpRegistrationsListedReply, ModelAliasesSavedReply, ModelChoicesRequestedReply,
    OpenWindowReply, ProfileCommandSetReply, ProfileDetailReply, ProfileEnabledSetReply,
    ProfileListReply, RestoreCandidateListReply, RoleListReply, TicketListReply,
    UpdateWindowLayoutReply,
};
use crate::events::AttachResponse;
use crate::frame_mailbox::FrameMailbox;
use crate::host::GuiHost;
use crate::ipc_client::{IpcClient, IpcError};
use crate::pool_outcomes::{
    ApprovalSyncOutcome, CloseOutcome, DecideApprovalOutcome, map_close_result,
    map_decide_approval_result, map_sync_approvals_result,
};

/// How one request wants its control connection established.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ControlConnect {
    /// Start the daemon when none is running.
    Spawn,
    /// Use the daemon that is already running, or fail.
    Existing,
}

/// The connections the GUI holds to the daemon: one per attached window, plus
/// the control connection.
#[derive(Debug)]
pub struct ConnectionPool {
    /// Per-window connections, indexed by window ID.
    windows: Mutex<BTreeMap<String, Arc<IpcClient>>>,
    /// One guard per window ID, held for the whole "already connected? ->
    /// establish -> record" sequence so that the poll loop and an explicit
    /// attach cannot both establish a connection for the same window.
    ///
    /// Entries are reclaimed only while nobody holds the guard: dropping one
    /// out from under a holder would hand the next caller a fresh, uncontended
    /// mutex and bring the race back.
    connect_guards: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    /// The attach-less connection every window-scoped request goes through.
    /// Keeping it separate from the window connections is what stops a
    /// request from depending on any particular window being attached.
    control: Arc<IpcClient>,
    /// Frames the daemon sent that the WebView has not pulled yet.
    frames: Arc<FrameMailbox>,
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionPool {
    /// Creates a pool with no connections established yet. `control` is
    /// built with [`IpcClient::new`], not [`IpcClient::new_window`]: only
    /// this one connection may register as an approver.
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(BTreeMap::new()),
            connect_guards: Mutex::new(BTreeMap::new()),
            control: Arc::new(IpcClient::new()),
            frames: Arc::new(FrameMailbox::new()),
        }
    }

    /// The mailbox the WebView pulls the daemon's frames from.
    pub fn frames(&self) -> &Arc<FrameMailbox> {
        &self.frames
    }

    /// Establishes one connection per window id, strictly one at a time, and
    /// returns the ids that failed in input order. An id that already has a
    /// connection is skipped, so calling this twice does not open a second
    /// connection to the same window.
    ///
    /// Sequential rather than concurrent: the daemon offers a single pending
    /// endpoint instance at a time, so simultaneous attempts
    /// would race for that one instance.
    ///
    /// The window's guard is held across the check and the establishment, so a
    /// concurrent [`Self::attach_window`] cannot end up with two connections
    /// attached to one window.
    pub async fn connect_windows<F, Fut>(&self, window_ids: &[String], connect: F) -> Vec<String>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<Arc<IpcClient>, IpcError>>,
    {
        let mut failed = Vec::new();
        for window_id in window_ids {
            let guard = self.connect_guard(window_id).await;
            let _establishing = guard.lock().await;
            if self.get_window(window_id).await.is_some() {
                continue;
            }
            match connect(window_id.clone()).await {
                Ok(client) => {
                    self.windows.lock().await.insert(window_id.clone(), client);
                }
                Err(_) => failed.push(window_id.clone()),
            }
        }
        failed
    }

    /// The guard that serialises connection establishment for one window.
    ///
    /// Guards are per window so that establishing a connection, which waits on
    /// the daemon, never blocks input, resize or lookups for other windows.
    async fn connect_guard(&self, window_id: &str) -> Arc<Mutex<()>> {
        let mut guards = self.connect_guards.lock().await;
        Arc::clone(guards.entry(window_id.to_owned()).or_default())
    }

    /// Returns the connection attached to a window, or `None` if the pool
    /// tracks no such window.
    pub async fn get_window(&self, window_id: &str) -> Option<Arc<IpcClient>> {
        let windows = self.windows.lock().await;
        windows.get(window_id).map(Arc::clone)
    }

    /// Stops tracking a window and releases its connection. The connection is
    /// closed deliberately, so it neither reconnects nor reports a disconnect
    /// for a window that is gone on purpose.
    pub async fn remove_window(&self, window_id: &str) {
        let removed = self.windows.lock().await.remove(window_id);
        if let Some(client) = removed {
            client.disconnect().await;
        }
        self.prune_connect_guard(window_id).await;
        self.frames.discard_window(window_id);
    }

    /// Drops the window's guard entry once this map is its last holder, so
    /// that opening and closing windows does not grow the map without bound.
    ///
    /// The count is read under the map lock, which is also the only place a
    /// clone can be taken ([`Self::connect_guard`]), so a guard nobody holds
    /// cannot gain a holder before the entry is gone.
    async fn prune_connect_guard(&self, window_id: &str) {
        let mut guards = self.connect_guards.lock().await;
        if guards
            .get(window_id)
            .is_some_and(|guard| Arc::strong_count(guard) == 1)
        {
            guards.remove(window_id);
        }
    }

    /// Lists all tracked window IDs.
    pub async fn window_ids(&self) -> Vec<String> {
        let windows = self.windows.lock().await;
        windows.keys().cloned().collect()
    }

    /// Attaches to `window_id` on its own connection, establishing one on
    /// first use.
    ///
    /// Runs under the window's guard: the window either already has a
    /// connection here (re-attaching on it kicks nobody) or it has none and no
    /// other task can be establishing one meanwhile.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or the window
    /// does not exist.
    pub async fn attach_window(
        &self,
        app: &impl GuiHost,
        window_id: &str,
        size: Option<TerminalSize>,
    ) -> Result<AttachResponse, IpcError> {
        let attached = {
            let guard = self.connect_guard(window_id).await;
            let _establishing = guard.lock().await;
            let client = {
                let mut windows = self.windows.lock().await;
                Arc::clone(
                    windows
                        .entry(window_id.to_owned())
                        .or_insert_with(|| Arc::new(IpcClient::new_window())),
                )
            };
            match client.attach_window(app, window_id, size).await {
                Ok(response) => Ok(response),
                Err(error) => {
                    // A window whose connection could not be established must
                    // not stay in the pool: input and resize route through it,
                    // and the window-list poll retries on its next tick.
                    self.remove_window(window_id).await;
                    Err(error)
                }
            }
        };
        // Outside the block, and so past the drop of this call's own clone of
        // the guard, which is what stopped the removal above from reclaiming
        // the entry.
        if attached.is_err() {
            self.prune_connect_guard(window_id).await;
        }
        attached
    }

    /// Lists the windows the daemon currently manages.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached.
    pub async fn list_windows(&self, app: &impl GuiHost) -> Result<Vec<WindowInfo>, IpcError> {
        self.control(app).await?.list_windows().await
    }

    /// Lists the detection definition names the daemon can offer.
    ///
    /// Goes through the control connection, like every other request that is
    /// not bound to a particular window.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached.
    pub async fn list_detections(
        &self,
        app: &impl GuiHost,
    ) -> Result<DetectionListReply, IpcError> {
        self.control(app).await?.list_detections().await
    }

    /// Lists the launch profiles the daemon can offer.
    ///
    /// Goes through the control connection, like every other request that is
    /// not bound to a particular window.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached.
    pub async fn list_profiles(&self, app: &impl GuiHost) -> Result<ProfileListReply, IpcError> {
        self.control(app).await?.list_profiles().await
    }

    /// Posts a message to the chat room through the control connection: the
    /// GUI posts as the approver it registered as, not as an unidentified
    /// client.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// post.
    pub async fn send_chat(
        &self,
        app: &impl GuiHost,
        room_id: Option<String>,
        body: String,
    ) -> Result<ChatPostedReply, IpcError> {
        self.control(app).await?.send_chat(room_id, body).await
    }

    /// Posts a message to every participant in the chat room but the GUI.
    ///
    /// Goes through the control connection, like [`Self::send_chat`].
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// post.
    pub async fn broadcast_chat(
        &self,
        app: &impl GuiHost,
        room_id: Option<String>,
        body: String,
    ) -> Result<ChatPostedReply, IpcError> {
        self.control(app).await?.broadcast_chat(room_id, body).await
    }

    /// Reads a page of the chat room's history.
    ///
    /// Goes through the control connection, whose caller kind decides what an
    /// omitted `after_seq` means.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn read_chat(
        &self,
        app: &impl GuiHost,
        room_id: Option<String>,
        after_seq: Option<u64>,
        limit: Option<u32>,
    ) -> Result<ChatMessagesReply, IpcError> {
        self.control(app)
            .await?
            .read_chat(room_id, after_seq, limit)
            .await
    }

    /// Lists every chat room the daemon knows of, through the control
    /// connection: the daemon lists rooms only for the approver it registered
    /// this connection as.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_chat_rooms(&self, app: &impl GuiHost) -> Result<ChatRoomListReply, IpcError> {
        self.control(app).await?.list_chat_rooms().await
    }

    /// Clears one chat room's history, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn clear_chat_room(
        &self,
        app: &impl GuiHost,
        room_id: String,
    ) -> Result<String, IpcError> {
        self.control(app).await?.clear_chat_room(room_id).await
    }

    /// Deletes one chat room, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn delete_chat_room(
        &self,
        app: &impl GuiHost,
        room_id: String,
    ) -> Result<String, IpcError> {
        self.control(app).await?.delete_chat_room(room_id).await
    }

    /// Lists the plans the daemon lets this GUI see, through the control
    /// connection: the daemon answers it as the approver it registered, which
    /// is what every plan-ledger request from the GUI relies on.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_plans(&self, app: &impl GuiHost) -> Result<Vec<PlanSummary>, IpcError> {
        self.control(app).await?.list_plans().await
    }

    /// Reads one plan in full, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn get_plan(
        &self,
        app: &impl GuiHost,
        plan_id: String,
    ) -> Result<PlanInfo, IpcError> {
        self.control(app).await?.get_plan(plan_id).await
    }

    /// Deletes one plan, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn delete_plan(
        &self,
        app: &impl GuiHost,
        plan_id: String,
    ) -> Result<String, IpcError> {
        self.control(app).await?.delete_plan(plan_id).await
    }

    /// Sets the user's assignment override for one task, through the control
    /// connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_task_assignment(
        &self,
        app: &impl GuiHost,
        plan_id: String,
        task_id: String,
        assignment: TaskAssignment,
    ) -> Result<PlanInfo, IpcError> {
        self.control(app)
            .await?
            .set_task_assignment(plan_id, task_id, assignment)
            .await
    }

    /// Removes the user's assignment override from one task, through the
    /// control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn clear_task_assignment(
        &self,
        app: &impl GuiHost,
        plan_id: String,
        task_id: String,
    ) -> Result<PlanInfo, IpcError> {
        self.control(app)
            .await?
            .clear_task_assignment(plan_id, task_id)
            .await
    }

    /// Names a window as a plan's owner, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_plan_owner(
        &self,
        app: &impl GuiHost,
        plan_id: String,
        window_id: String,
    ) -> Result<PlanInfo, IpcError> {
        self.control(app)
            .await?
            .set_plan_owner(plan_id, window_id)
            .await
    }

    /// Approves a plan awaiting review, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn approve_plan(
        &self,
        app: &impl GuiHost,
        plan_id: String,
        version: u64,
    ) -> Result<PlanInfo, IpcError> {
        self.control(app)
            .await?
            .approve_plan(plan_id, version)
            .await
    }

    /// Clears the review marks of a plan, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn mark_plan_seen(
        &self,
        app: &impl GuiHost,
        plan_id: String,
        version: u64,
    ) -> Result<PlanInfo, IpcError> {
        self.control(app)
            .await?
            .mark_plan_seen(plan_id, version)
            .await
    }

    /// Lists every workspace that has tickets, through the control
    /// connection: the daemon answers it as the approver it registered.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_ticket_workspaces(
        &self,
        app: &impl GuiHost,
    ) -> Result<Vec<TicketWorkspaceInfo>, IpcError> {
        self.control(app).await?.list_ticket_workspaces().await
    }

    /// Reads one workspace's ticket list, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_tickets(
        &self,
        app: &impl GuiHost,
        workspace_id: i64,
        state: Option<String>,
        milestone: Option<String>,
        limit: Option<u32>,
    ) -> Result<TicketListReply, IpcError> {
        self.control(app)
            .await?
            .list_tickets(workspace_id, state, milestone, limit)
            .await
    }

    /// Reads one ticket in full, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn get_ticket(
        &self,
        app: &impl GuiHost,
        workspace_id: i64,
        ticket: u32,
    ) -> Result<TicketInfo, IpcError> {
        self.control(app)
            .await?
            .get_ticket(workspace_id, ticket)
            .await
    }

    /// Reads the role-to-assignment table, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_roles(&self, app: &impl GuiHost) -> Result<RoleListReply, IpcError> {
        self.control(app).await?.list_roles().await
    }

    /// Replaces the whole role-to-assignment table, through the control
    /// connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// table.
    pub async fn save_roles(
        &self,
        app: &impl GuiHost,
        roles: Vec<RoleEntry>,
        default_role: Option<String>,
    ) -> Result<RoleListReply, IpcError> {
        self.control(app)
            .await?
            .save_roles(roles, default_role)
            .await
    }

    /// Asks for the models a profile's `model` launch option can take,
    /// through the control connection. The list arrives later as a push on
    /// that connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_model_choices(
        &self,
        app: &impl GuiHost,
        profile: String,
        request_id: u64,
    ) -> Result<ModelChoicesRequestedReply, IpcError> {
        self.control(app)
            .await?
            .list_model_choices(profile, request_id)
            .await
    }

    /// Replaces the values of a profile's model aliases, through the control
    /// connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// aliases.
    pub async fn save_model_aliases(
        &self,
        app: &impl GuiHost,
        profile: String,
        aliases: Vec<LaunchOptionAlias>,
    ) -> Result<ModelAliasesSavedReply, IpcError> {
        self.control(app)
            .await?
            .save_model_aliases(profile, aliases)
            .await
    }

    /// Turns a profile's launching on or off, through the control
    /// connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_profile_enabled(
        &self,
        app: &impl GuiHost,
        profile: String,
        enabled: bool,
    ) -> Result<ProfileEnabledSetReply, IpcError> {
        self.control(app)
            .await?
            .set_profile_enabled(profile, enabled)
            .await
    }

    /// Reads every value a profile uses, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn get_profile_detail(
        &self,
        app: &impl GuiHost,
        profile: String,
    ) -> Result<ProfileDetailReply, IpcError> {
        self.control(app).await?.get_profile_detail(profile).await
    }

    /// Sets or puts back a profile's argv on this host, through the control
    /// connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_profile_command(
        &self,
        app: &impl GuiHost,
        profile: String,
        argv: Option<Vec<String>>,
    ) -> Result<ProfileCommandSetReply, IpcError> {
        self.control(app)
            .await?
            .set_profile_command(profile, argv)
            .await
    }

    /// Gives the daemon the palettes a window it opens from now on answers
    /// color queries with, through the control connection. Neither connects
    /// nor starts a daemon: the control connection sends them when it is
    /// next made.
    ///
    /// # Errors
    /// Returns [`IpcError`] when a connected daemon could not be written to.
    pub async fn set_default_palettes(&self, palettes: DefaultPalettes) -> Result<(), IpcError> {
        self.control.set_default_palettes(palettes).await
    }

    /// Reads the daemon's current approval settings, through the control
    /// connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn get_approval_settings(
        &self,
        app: &impl GuiHost,
    ) -> Result<ApprovalSettingsReply, IpcError> {
        self.control(app).await?.get_approval_settings().await
    }

    /// Turns automatic approval of one destructive tool on or off, through
    /// the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_auto_approve(
        &self,
        app: &impl GuiHost,
        tool: String,
        auto_approve: bool,
    ) -> Result<ApprovalSettingsReply, IpcError> {
        self.control(app)
            .await?
            .set_auto_approve(tool, auto_approve)
            .await
    }

    /// Turns the plan review gate on or off, through the control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_plan_approval_required(
        &self,
        app: &impl GuiHost,
        required: bool,
    ) -> Result<ApprovalSettingsReply, IpcError> {
        self.control(app)
            .await?
            .set_plan_approval_required(required)
            .await
    }

    /// Asks which profiles describe how to register the MCP bridge with their
    /// CLI, through the control connection. Each row's state arrives later as
    /// a push on that connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_mcp_registrations(
        &self,
        app: &impl GuiHost,
        request_id: u64,
    ) -> Result<McpRegistrationsListedReply, IpcError> {
        self.control(app)
            .await?
            .list_mcp_registrations(request_id)
            .await
    }

    /// Asks the daemon to change one profile's MCP registration, through the
    /// control connection. The outcome arrives later as a push on that
    /// connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn run_mcp_registration(
        &self,
        app: &impl GuiHost,
        profile: String,
        action: McpRegistrationAction,
        request_id: u64,
    ) -> Result<McpRegistrationRequestedReply, IpcError> {
        self.control(app)
            .await?
            .run_mcp_registration(profile, action, request_id)
            .await
    }

    /// Opens a new window and returns the id the daemon assigned to it,
    /// together with the detection definition's load warning, if any.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or refuses to
    /// open the window.
    // One parameter per `open_window` wire argument, plus the host.
    #[allow(clippy::too_many_arguments)]
    pub async fn open_window(
        &self,
        app: &impl GuiHost,
        command: Vec<String>,
        cwd: Option<String>,
        name: Option<String>,
        on_name_taken: OnNameTaken,
        detection: Option<String>,
        vendor: Option<String>,
    ) -> Result<OpenWindowReply, IpcError> {
        self.control(app)
            .await?
            .open_window(command, cwd, name, on_name_taken, detection, vendor)
            .await
    }

    /// Opens a new window from a launch profile, which supplies the command,
    /// the detection definition and the vendor badge.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, does not know
    /// the profile, or refuses to open the window.
    // One parameter per `open_window` wire argument, plus the host.
    #[allow(clippy::too_many_arguments)]
    pub async fn open_window_from_profile(
        &self,
        app: &impl GuiHost,
        profile: String,
        cwd: Option<String>,
        name: Option<String>,
        on_name_taken: OnNameTaken,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<OpenWindowReply, IpcError> {
        self.control(app)
            .await?
            .open_window_from_profile(profile, cwd, name, on_name_taken, model, effort)
            .await
    }

    /// Lists the previous boot's window configuration for the restore UI.
    /// Goes through the control connection, like every other request that is
    /// not bound to a particular window.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached.
    pub async fn list_restore_candidates(
        &self,
        app: &impl GuiHost,
    ) -> Result<RestoreCandidateListReply, IpcError> {
        self.control(app).await?.list_restore_candidates().await
    }

    /// Asks the daemon what keeps it from opening windows, through the
    /// control connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached.
    pub async fn get_daemon_health(
        &self,
        app: &impl GuiHost,
    ) -> Result<DaemonHealthReply, IpcError> {
        self.control(app).await?.get_daemon_health().await
    }

    /// Reports a window's presentation layout to the daemon, verbatim.
    ///
    /// Connects without starting a daemon: this is a report about what the
    /// view shows, and there is nothing to tell a daemon that is not running.
    ///
    /// # Errors
    /// Returns [`IpcError`] when no daemon is running or it cannot be
    /// reached, the window does not exist, or the daemon rejects the layout
    /// as too large.
    pub async fn update_window_layout(
        &self,
        app: &impl GuiHost,
        window_id: &str,
        layout_json: String,
    ) -> Result<UpdateWindowLayoutReply, IpcError> {
        self.control_connected(app, ControlConnect::Existing)
            .await?
            .update_window_layout(window_id, layout_json)
            .await
    }

    /// The whole close path as one result: connecting to a running daemon,
    /// confirming the approver registration, and the round trip itself.
    /// Folding them together is what lets [`ConnectionPool::close_window`]
    /// map an unreachable daemon, which fails before the round trip.
    ///
    /// # Errors
    /// Returns [`IpcError`] when no daemon is running or it cannot be
    /// reached, this connection has not registered as an approver yet
    /// ([`IpcError::ApproverNotRegistered`]), or the daemon rejects the
    /// request.
    async fn close_round_trip(
        &self,
        app: &impl GuiHost,
        window_id: &str,
        force: Option<bool>,
    ) -> Result<(), IpcError> {
        self.require_approver_registered(app, ControlConnect::Existing)
            .await?
            .close_window(window_id, force)
            .await
    }

    /// Closes a window, terminating its process tree, and stops tracking it.
    ///
    /// GUI-originated closes bypass the daemon's human approval gate
    /// entirely once this connection has registered as an approver, so the
    /// request is refused here rather than sent
    /// unregistered.
    ///
    /// Every outcome drops the window's connection: it is unusable whether
    /// the daemon closed the window, holds no such window, or cannot be
    /// reached -- and a window that is in fact alive is attached again by the
    /// next reconcile.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon rejects the request for any
    /// reason other than the window not existing, answers off-contract, or
    /// this connection has not registered as an approver yet
    /// ([`IpcError::ApproverNotRegistered`]).
    pub async fn close_window(
        &self,
        app: &impl GuiHost,
        window_id: &str,
        force: Option<bool>,
    ) -> Result<CloseOutcome, IpcError> {
        let outcome = map_close_result(self.close_round_trip(app, window_id, force).await)?;
        self.remove_window(window_id).await;
        Ok(outcome)
    }

    /// Sends an interrupt signal to a window's child process.
    ///
    /// GUI-originated interrupts bypass the daemon's human approval gate
    /// entirely once this connection has registered as an approver, so the
    /// request is refused here rather than sent
    /// unregistered.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, the window
    /// does not exist, or this connection has not registered as an approver
    /// yet ([`IpcError::ApproverNotRegistered`]).
    pub async fn interrupt_window(
        &self,
        app: &impl GuiHost,
        window_id: &str,
    ) -> Result<(), IpcError> {
        self.require_approver_registered(app, ControlConnect::Spawn)
            .await?
            .interrupt_window(window_id)
            .await
    }

    /// Settles one pending approval request.
    ///
    /// A request the daemon no longer has pending (`approval_not_pending` --
    /// already decided elsewhere, or expired) is reported as
    /// [`DecideApprovalOutcome::NotPending`] rather than an error: the
    /// approver's decision itself was not refused, it simply arrived too
    /// late to matter.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request for any other reason.
    pub async fn decide_approval(
        &self,
        app: &impl GuiHost,
        request_id: &str,
        approved: bool,
    ) -> Result<DecideApprovalOutcome, IpcError> {
        map_decide_approval_result(
            self.control(app)
                .await?
                .decide_approval(request_id, approved)
                .await,
        )
    }

    /// Pulls the requests pending for this GUI's approver registration, so the
    /// WebView starts from the daemon's list instead of from whatever events
    /// happened to arrive after its subscription opened.
    ///
    /// [`ControlConnect::Existing`], not [`ControlConnect::Spawn`]: a pull is
    /// a resynchronisation of a running daemon, so a missing daemon is
    /// reported rather than started.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// registration for a reason other than refusing this approver (which is
    /// reported as [`ApprovalSyncOutcome::Refused`]).
    pub async fn sync_approvals(
        &self,
        app: &impl GuiHost,
    ) -> Result<ApprovalSyncOutcome, IpcError> {
        self.control_connected(app, ControlConnect::Existing)
            .await?;
        map_sync_approvals_result(self.control.sync_approvals(app).await)
    }

    /// The control connection, with its approver registration confirmed
    /// current: [`ConnectionPool::control_connected()`]'s single hook-free
    /// connect is deliberately not where this lives (the approver gate is
    /// specific to the destructive operations, not every control-connection
    /// request).
    ///
    /// `connect` follows the caller's intent: an operation on a live window
    /// starts a daemon when none runs, whereas tidying the view up does not.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or the
    /// connection's registration attempt has not succeeded yet.
    async fn require_approver_registered(
        &self,
        app: &impl GuiHost,
        connect: ControlConnect,
    ) -> Result<&Arc<IpcClient>, IpcError> {
        let control = self.control_connected(app, connect).await?;
        control.ensure_approver_registered(app).await;
        control.ensure_protocol_revision().await;
        // Not `approver_registration_due().is_some()`: a refused generation
        // makes `due` `None` too (the refusal latch), which would
        // otherwise let a destructive operation through unregistered.
        if !control.is_approver_registered() {
            return Err(IpcError::ApproverNotRegistered);
        }
        Ok(control)
    }

    /// Renames a window and returns the name as the daemon stored it.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, the window
    /// does not exist, or the name is invalid.
    pub async fn rename_window(
        &self,
        app: &impl GuiHost,
        window_id: &str,
        name: &str,
    ) -> Result<String, IpcError> {
        self.control(app)
            .await?
            .rename_window(window_id, name)
            .await
    }

    /// Sends an already validated message to the connection attached to
    /// `window_id`; the daemon applies it to that connection's window.
    ///
    /// # Errors
    /// Returns [`IpcError::WindowNotAttached`] when no connection serves that
    /// window, and [`IpcError`] when the write fails.
    pub async fn send_to_window(
        &self,
        window_id: &str,
        message: &ClientMessage,
    ) -> Result<(), IpcError> {
        self.window_or_error(window_id)
            .await?
            .send_attached(message)
            .await
    }

    /// Reads the text selected in `window_id`; `None` when nothing is
    /// selected.
    ///
    /// # Errors
    /// Returns [`IpcError::WindowNotAttached`] when no connection serves that
    /// window, and [`IpcError`] when the round trip fails.
    pub async fn copy_selection(&self, window_id: &str) -> Result<Option<String>, IpcError> {
        self.window_or_error(window_id)
            .await?
            .copy_selection()
            .await
    }

    /// Resizes the PTY of the connection attached to `window_id`.
    ///
    /// # Errors
    /// Returns [`IpcError::WindowNotAttached`] when no connection serves that
    /// window, and [`IpcError`] when the write fails.
    pub async fn resize(&self, window_id: &str, cols: u16, rows: u16) -> Result<(), IpcError> {
        self.window_or_error(window_id)
            .await?
            .resize(cols, rows)
            .await
    }

    /// The control connection, established the way `connect` asks for before
    /// it is handed out. Starting a daemon is a side effect a caller opts
    /// into: making a window, or acting on one that is live, is worth
    /// starting one for; tidying the view up after a close is not.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the connection cannot be established --
    /// which, for [`ControlConnect::Existing`], includes no daemon running.
    pub(crate) async fn control_connected(
        &self,
        app: &impl GuiHost,
        connect: ControlConnect,
    ) -> Result<&Arc<IpcClient>, IpcError> {
        match connect {
            ControlConnect::Existing => {
                self.control.connect_existing(app).await?;
                Ok(&self.control)
            }
            ControlConnect::Spawn => self.control(app).await,
        }
    }

    /// The control connection, connected (starting the daemon when none is
    /// running) before it is handed out.
    async fn control(&self, app: &impl GuiHost) -> Result<&Arc<IpcClient>, IpcError> {
        self.control.connect(app).await?;
        Ok(&self.control)
    }

    /// Brings a daemon up when none is running, by establishing the control
    /// connection the spawning way.
    ///
    /// This is the entry point the application takes on its own initiative;
    /// every other start follows a user action. A caller that only wants to
    /// know whether a daemon is there uses
    /// [`Self::has_live_control_connection`] instead.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon can neither be reached nor
    /// started.
    pub(crate) async fn ensure_daemon_running(&self, app: &impl GuiHost) -> Result<(), IpcError> {
        self.control_connected(app, ControlConnect::Spawn).await?;
        Ok(())
    }

    /// Whether the control connection is established right now.
    ///
    /// Side-effect free, unlike [`Self::control_connected`], which
    /// establishes the connection before handing it out: an observation must
    /// not be what repairs the thing it observes.
    pub(crate) async fn has_live_control_connection(&self) -> bool {
        self.control.is_connected().await
    }

    /// The control connection as it stands, without connecting it or
    /// starting a daemon. A read-only accessor over the private field, kept
    /// for [`crate::window_list_poll::run_window_list_poll`], which reports on
    /// a daemon that is already running rather than bringing one up.
    pub(crate) fn control_connection(&self) -> &Arc<IpcClient> {
        &self.control
    }

    async fn window_or_error(&self, window_id: &str) -> Result<Arc<IpcClient>, IpcError> {
        self.get_window(window_id)
            .await
            .ok_or_else(|| IpcError::WindowNotAttached {
                window_id: window_id.to_owned(),
            })
    }

    /// Gives every live window the daemon reports a connection of its own and
    /// tears down the connection of every window that is gone or has exited.
    /// Ids that fail to connect are left out and retried on the next poll.
    ///
    /// Tearing an exited window's connection down is what keeps the daemon
    /// able to evict its entry: only entries that are exited *and* unattached
    /// can be evicted.
    pub(crate) async fn reconcile(&self, app: &impl GuiHost, windows: &[WindowInfo]) {
        let live = live_window_ids(windows);
        self.retire_windows_except(&live).await;

        let ids: Vec<String> = live.iter().map(|id| (*id).to_owned()).collect();
        let _failed = self
            .connect_windows(&ids, |window_id| {
                let app = app.clone();
                async move {
                    let client = Arc::new(IpcClient::new_window());
                    client.attach_window(&app, &window_id, None).await?;
                    Ok(client)
                }
            })
            .await;
    }

    /// Tears down the connection of every tracked window not in `live`.
    async fn retire_windows_except(&self, live: &[&str]) {
        for tracked in self.window_ids().await {
            if !live.contains(&tracked.as_str()) {
                self.remove_window(&tracked).await;
            }
        }
    }
}

/// The ids of the windows that still want a connection: every one the daemon
/// reports except those that have exited.
fn live_window_ids(windows: &[WindowInfo]) -> Vec<&str> {
    windows
        .iter()
        .filter(|window| window.status != WindowStatus::Exited)
        .map(|window| window.window_id.as_str())
        .collect()
}

#[cfg(test)]
mod host_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc as StdArc;

    use tokio::sync::{Barrier, Mutex as TokioMutex};

    use super::*;

    /// Records connection-establishment events in the order they occur so
    /// tests can assert on interleaving (or lack thereof).
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Start(String),
        End(String),
    }

    #[tokio::test]
    async fn connect_windows_establishes_connections_strictly_one_at_a_time() {
        // Only a strictly sequential implementation can produce this exact
        // interleaving of Start/End events.
        let log = StdArc::new(TokioMutex::new(Vec::new()));
        let pool = ConnectionPool::new();
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let log_for_connect = StdArc::clone(&log);
        let failed = pool
            .connect_windows(&ids, move |id: String| {
                let log = StdArc::clone(&log_for_connect);
                async move {
                    log.lock().await.push(Event::Start(id.clone()));
                    tokio::task::yield_now().await;
                    log.lock().await.push(Event::End(id.clone()));
                    Ok(StdArc::new(IpcClient::new()))
                }
            })
            .await;

        assert!(failed.is_empty());
        let log = log.lock().await.clone();
        assert_eq!(
            log,
            vec![
                Event::Start("a".to_string()),
                Event::End("a".to_string()),
                Event::Start("b".to_string()),
                Event::End("b".to_string()),
                Event::Start("c".to_string()),
                Event::End("c".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn concurrent_execution_of_the_same_recording_logic_interleaves() {
        // Keeps the sequential assertion above from being a truism: the same
        // Start-yield-End logic run concurrently produces a log it would reject.
        let log = StdArc::new(TokioMutex::new(Vec::new()));
        let barrier = StdArc::new(Barrier::new(3));
        let mut tasks = Vec::new();

        for id in ["a", "b", "c"] {
            let log = StdArc::clone(&log);
            let barrier = StdArc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                log.lock().await.push(Event::Start(id.to_string()));
                barrier.wait().await;
                log.lock().await.push(Event::End(id.to_string()));
            }));
        }
        for task in tasks {
            task.await.expect("spawned task panicked");
        }

        let log = log.lock().await.clone();
        assert_eq!(log.len(), 6);
        let starts_before_ends = log[0..3].iter().all(|e| matches!(e, Event::Start(_)));
        let ends_after = log[3..6].iter().all(|e| matches!(e, Event::End(_)));
        assert!(
            starts_before_ends && ends_after,
            "expected all Starts to precede all Ends under concurrent execution, got {log:?}"
        );

        let strict_sequential = vec![
            Event::Start("a".to_string()),
            Event::End("a".to_string()),
            Event::Start("b".to_string()),
            Event::End("b".to_string()),
            Event::Start("c".to_string()),
            Event::End("c".to_string()),
        ];
        assert_ne!(log, strict_sequential);
    }

    #[tokio::test]
    async fn connect_windows_returns_failed_ids_in_input_order() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let failed = ConnectionPool::new()
            .connect_windows(&ids, |id: String| async move {
                if id == "b" {
                    Err(IpcError::ConnectTimeout)
                } else {
                    Ok(StdArc::new(IpcClient::new()))
                }
            })
            .await;

        assert_eq!(failed, vec!["b".to_string()]);
    }

    #[tokio::test]
    async fn connect_windows_reports_multiple_failures_in_input_order() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let failed = ConnectionPool::new()
            .connect_windows(&ids, |id: String| async move {
                if id == "a" || id == "c" {
                    Err(IpcError::ConnectTimeout)
                } else {
                    Ok(StdArc::new(IpcClient::new()))
                }
            })
            .await;

        assert_eq!(failed, vec!["a".to_string(), "c".to_string()]);
    }

    #[tokio::test]
    async fn has_live_control_connection_is_false_for_a_freshly_created_pool() {
        let pool = ConnectionPool::new();
        assert!(!pool.has_live_control_connection().await);
    }

    #[tokio::test]
    async fn get_remove_window_ids_are_independent_per_window_and_connecting_twice_does_not_duplicate()
     {
        let pool = ConnectionPool::new();
        let ids = vec!["a".to_string(), "b".to_string()];
        let failed = pool
            .connect_windows(&ids, |_id: String| async move {
                Ok(StdArc::new(IpcClient::new()))
            })
            .await;
        assert!(failed.is_empty());

        // Re-connecting the same id must not duplicate the entry.
        let again = vec!["a".to_string()];
        let failed_again = pool
            .connect_windows(&again, |_id: String| async move {
                Ok(StdArc::new(IpcClient::new()))
            })
            .await;
        assert!(failed_again.is_empty());

        let mut ids_now = pool.window_ids().await;
        ids_now.sort();
        assert_eq!(ids_now, vec!["a".to_string(), "b".to_string()]);

        assert!(pool.get_window("a").await.is_some());
        assert!(pool.get_window("b").await.is_some());
        assert!(pool.get_window("nonexistent").await.is_none());

        pool.remove_window("a").await;
        assert!(pool.get_window("a").await.is_none());
        assert!(pool.get_window("b").await.is_some());
    }

    async fn pool_with_windows(ids: &[&str]) -> ConnectionPool {
        let pool = ConnectionPool::new();
        let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
        let failed = pool
            .connect_windows(&ids, |_id: String| async move {
                Ok(StdArc::new(IpcClient::new()))
            })
            .await;
        assert!(failed.is_empty());
        pool
    }

    #[tokio::test]
    async fn concurrent_connects_for_one_window_establish_a_single_connection() {
        let pool = ConnectionPool::new();
        let calls = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
        let ids = vec!["a".to_owned()];
        let connect = |_id: String| {
            let calls = StdArc::clone(&calls);
            async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Suspends mid-establishment so the other connect runs meanwhile.
                tokio::task::yield_now().await;
                Ok::<_, IpcError>(StdArc::new(IpcClient::new()))
            }
        };

        let (first, second) = tokio::join!(
            pool.connect_windows(&ids, &connect),
            pool.connect_windows(&ids, &connect)
        );

        assert!(first.is_empty() && second.is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn removing_a_window_reclaims_its_connect_guard() {
        let pool = pool_with_windows(&["a"]).await;

        pool.remove_window("a").await;

        assert!(!pool.connect_guards.lock().await.contains_key("a"));
    }

    #[tokio::test]
    async fn a_connect_guard_someone_holds_survives_the_window_removal() {
        let pool = pool_with_windows(&["a"]).await;
        let held = pool.connect_guard("a").await;

        pool.remove_window("a").await;

        assert!(StdArc::ptr_eq(&held, &pool.connect_guard("a").await));
    }

    #[tokio::test]
    async fn requests_to_a_window_without_a_connection_report_it_as_not_attached() {
        let pool = ConnectionPool::new();

        let sent = pool
            .send_to_window("gone", &ClientMessage::ClearSelection)
            .await;
        let copied = pool.copy_selection("gone").await;
        let resized = pool.resize("gone", 80, 24).await;

        assert!(names_gone_as_not_attached(
            &sent.expect_err("send must fail")
        ));
        assert!(names_gone_as_not_attached(
            &copied.expect_err("copy must fail")
        ));
        assert!(names_gone_as_not_attached(
            &resized.expect_err("resize must fail")
        ));
    }

    fn names_gone_as_not_attached(error: &IpcError) -> bool {
        matches!(error, IpcError::WindowNotAttached { window_id } if window_id == "gone")
    }

    #[tokio::test]
    async fn the_frames_of_a_removed_window_never_reach_the_webview() {
        let pool = ConnectionPool::new();
        pool.frames().push("a", sample_frame(1));
        pool.frames().push("b", sample_frame(2));

        pool.remove_window("a").await;
        let delivered = tokio::time::timeout(HANG_GUARD, pool.frames().take())
            .await
            .expect("the remaining frame is delivered");

        assert_eq!(delivered, vec![("b".to_owned(), sample_frame(2))]);
    }

    #[tokio::test]
    async fn only_windows_the_daemon_still_reports_alive_keep_their_connection() {
        let pool = pool_with_windows(&["running", "exited", "gone"]).await;
        let reported = vec![
            window_info("running", WindowStatus::Running),
            window_info("exited", WindowStatus::Exited),
        ];

        let live = live_window_ids(&reported);
        pool.retire_windows_except(&live).await;

        assert_eq!(live, vec!["running"]);
        assert_eq!(pool.window_ids().await, vec!["running".to_owned()]);
    }

    /// Only keeps a broken mailbox from hanging the suite.
    const HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(10);

    fn window_info(window_id: &str, status: WindowStatus) -> WindowInfo {
        WindowInfo {
            window_id: window_id.to_owned(),
            name: window_id.to_owned(),
            vendor: None,
            cwd: "/work".to_owned(),
            status,
            exit_code: None,
            layout_json: None,
            room: None,
            unknown_reason: None,
            detection_warning: None,
            status_rev: None,
            mcp_connected: None,
            status_reports_missing: None,
        }
    }

    fn sample_frame(seq: u64) -> yagura_protocol::Frame {
        yagura_protocol::Frame {
            seq,
            cols: 2,
            rows: 1,
            reset: false,
            last: true,
            attrs: Vec::new(),
            lines: Vec::new(),
            cursor: yagura_protocol::CursorState {
                row: 0,
                col: 0,
                visible: true,
                shape: yagura_protocol::CursorShape::Block,
            },
            view: yagura_protocol::ViewState {
                display_offset: 0,
                history_rows: 0,
                mouse_reporting: false,
                alt_screen: false,
            },
            overrides: Vec::new(),
        }
    }
}
