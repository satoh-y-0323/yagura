//! The points a test of the daemon arms to stand the daemon at one moment of
//! its work: the daemon ends itself there, or its parcel writer, its window
//! table's writer, or a relay session's take of reports, fails or stops there
//! until the test releases it. Each point is named by the
//! property it stands for, not by the code it sits in, so a change that moves
//! the property moves the point with it.
//!
//! Only the `fault-injection` feature builds this module: a shipped daemon has
//! neither the points nor a way to arm them.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use thiserror::Error;
use tokio::sync::watch;

use crate::parcel_store::{ParcelStore, WaitedStore};

/// The variable a daemon process reads the name of its armed point from.
pub const POINT_ENV: &str = "YAGURA_FAULT_POINT";

/// The variable naming the file whose existence holds [`FaultPoint::WriteFails`]
/// in a daemon process: removing the file releases it.
pub const SIGNAL_ENV: &str = "YAGURA_FAULT_SIGNAL";

/// One point a test can arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// A handoff envelope for a window is in that window's inbox. The daemon
    /// ends there.
    HandoffDeposited,
    /// Every write of kept parcels, reports and delivery marks fails until the
    /// fault is released.
    WriteFails,
    /// A parcel's end has removed its kept row, and the chat delivery it
    /// carried is not settled yet, inside the one change that does both. The
    /// daemon ends there.
    ParcelEndHalfSettled,
    /// A task window's report is in the plan ledger, and its parent's report
    /// inbox does not hold it yet. The daemon ends there.
    TaskReportHalfWritten,
    /// The parcel writer stops before its first write until the fault is
    /// released. Released only in-process ([`FaultPoints::release`]).
    WriterPaused,
    /// A chat post is committed, and no destination's inbox shows it yet. The
    /// daemon ends there.
    ChatPostCommitted,
    /// A door has said it printed a read that took kept parcels, and the end
    /// of what it took is not committed yet. The daemon ends there.
    DoorBeforeSettle,
    /// A relay session's `wait_for_report` has taken its reports, and its
    /// answer is not handed to the session yet. Every such take stops there
    /// until the fault is released. Released only in-process.
    ReportTakenUnanswered,
    /// A window's holder has started its child and let the daemon in, and the
    /// window is not registered yet. The daemon ends there.
    WindowHolderStarted,
    /// The window table's writer stops before it writes a window's values to
    /// its row until the fault is released. Released only in-process.
    WindowRowUpdatePaused,
    /// Every write of a window's values to its row fails until the fault is
    /// released; the rows themselves are still reserved, marked and removed.
    WindowRowUpdateFails,
}

impl FaultPoint {
    /// Every point, in the order of [`FaultPoint::name`]'s table.
    pub const ALL: [Self; 11] = [
        Self::HandoffDeposited,
        Self::WriteFails,
        Self::ParcelEndHalfSettled,
        Self::TaskReportHalfWritten,
        Self::WriterPaused,
        Self::ChatPostCommitted,
        Self::DoorBeforeSettle,
        Self::ReportTakenUnanswered,
        Self::WindowHolderStarted,
        Self::WindowRowUpdatePaused,
        Self::WindowRowUpdateFails,
    ];

    /// The point's name, as [`POINT_ENV`] carries it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::HandoffDeposited => "handoff_deposited",
            Self::WriteFails => "write_fails",
            Self::ParcelEndHalfSettled => "parcel_end_half_settled",
            Self::TaskReportHalfWritten => "task_report_half_written",
            Self::WriterPaused => "writer_paused",
            Self::ChatPostCommitted => "chat_post_committed",
            Self::DoorBeforeSettle => "door_before_settle",
            Self::ReportTakenUnanswered => "report_taken_unanswered",
            Self::WindowHolderStarted => "window_holder_started",
            Self::WindowRowUpdatePaused => "window_row_update_paused",
            Self::WindowRowUpdateFails => "window_row_update_fails",
        }
    }

    /// The point named `name`; `None` for any other text.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|point| point.name() == name)
    }

    /// Whether the daemon ends itself at this point; the others stop or fail
    /// its work until released.
    #[must_use]
    pub const fn ends_daemon(self) -> bool {
        !matches!(
            self,
            Self::WriteFails
                | Self::WriterPaused
                | Self::ReportTakenUnanswered
                | Self::WindowRowUpdatePaused
                | Self::WindowRowUpdateFails
        )
    }

    /// Whether only the test's own process can release this point.
    const fn in_process_only(self) -> bool {
        matches!(
            self,
            Self::WriterPaused | Self::ReportTakenUnanswered | Self::WindowRowUpdatePaused
        )
    }
}

/// Why [`FaultPoints::from_env`] refused what the environment names.
#[derive(Debug, Error)]
pub enum FaultArmError {
    #[error("{POINT_ENV} names no fault point: {0:?}")]
    Unknown(String),
    #[error("the fault point {0} is released only in-process")]
    InProcessOnly(&'static str),
}

/// The point a daemon has armed, if any: part of its [`crate::DaemonConfig`],
/// and shared by every clone of it.
#[derive(Debug, Clone, Default)]
pub struct FaultPoints {
    armed: Option<Arc<Armed>>,
}

#[derive(Debug)]
struct Armed {
    point: FaultPoint,
    /// While this file exists the fault holds: how a test releases a daemon
    /// process.
    signal: Option<PathBuf>,
    released: Mutex<bool>,
    release: Condvar,
    /// [`Armed::released`] for a task that waits without blocking its thread.
    released_watch: watch::Sender<bool>,
    reached: watch::Sender<bool>,
    /// Waited on before the daemon ends at a point, so what the daemon handed
    /// its writer before the point is written and nothing after it. It does
    /// not keep the writer, which holds these points itself.
    store: Mutex<Option<WaitedStore>>,
}

impl FaultPoints {
    /// `point` armed, released by [`FaultPoints::release`].
    #[must_use]
    pub fn arm(point: FaultPoint) -> Self {
        Self::armed(point, None)
    }

    fn armed(point: FaultPoint, signal: Option<PathBuf>) -> Self {
        Self {
            armed: Some(Arc::new(Armed {
                point,
                signal,
                released: Mutex::new(false),
                release: Condvar::new(),
                released_watch: watch::channel(false).0,
                reached: watch::channel(false).0,
                store: Mutex::new(None),
            })),
        }
    }

    /// The point a daemon process was started with: [`POINT_ENV`] names it
    /// and [`SIGNAL_ENV`] the file that holds it. Nothing armed without
    /// [`POINT_ENV`].
    ///
    /// # Errors
    ///
    /// [`FaultArmError::Unknown`] when [`POINT_ENV`] names no point, and
    /// [`FaultArmError::InProcessOnly`] for [`FaultPoint::WriterPaused`],
    /// [`FaultPoint::ReportTakenUnanswered`] and
    /// [`FaultPoint::WindowRowUpdatePaused`], which nothing outside the
    /// process can release.
    pub fn from_env() -> Result<Self, FaultArmError> {
        let Some(name) = std::env::var_os(POINT_ENV) else {
            return Ok(Self::default());
        };
        let name = name.to_string_lossy();
        let point = FaultPoint::from_name(&name)
            .ok_or_else(|| FaultArmError::Unknown(name.into_owned()))?;
        if point.in_process_only() {
            return Err(FaultArmError::InProcessOnly(point.name()));
        }
        Ok(Self::armed(
            point,
            std::env::var_os(SIGNAL_ENV).map(PathBuf::from),
        ))
    }

    /// The armed point, if any.
    #[must_use]
    pub fn point(&self) -> Option<FaultPoint> {
        self.armed.as_ref().map(|armed| armed.point)
    }

    /// Releases the armed fault: the writer goes on, and writes stop failing.
    pub fn release(&self) {
        if let Some(armed) = &self.armed {
            *armed.lock_released() = true;
            armed.release.notify_all();
            armed.released_watch.send_replace(true);
        }
    }

    /// Returns once the armed point has been reached; never when nothing is
    /// armed.
    pub async fn reached(&self) {
        let Some(armed) = &self.armed else {
            return std::future::pending().await;
        };
        let mut reached = armed.reached.subscribe();
        // The sender lives in `armed`, which this borrow keeps.
        let _ = reached.wait_for(|reached| *reached).await;
    }

    /// Hands the parcel writer of `store` what it acts on, and keeps `store`
    /// to be waited on before the daemon ends at its point.
    pub(crate) fn attach(&self, store: &ParcelStore) {
        let Some(armed) = &self.armed else {
            return;
        };
        if armed.point.ends_daemon() {
            *armed.store.lock().unwrap_or_else(PoisonError::into_inner) = Some(store.waited());
        }
        store.arm_writer(self.clone());
    }

    /// Hands the writer of `table` what it acts on from its next command on.
    pub(crate) fn attach_window_table(&self, table: &crate::window_table::WindowTable) {
        if self.armed.is_some() {
            table.arm_writer(self.clone());
        }
    }

    /// Ends the daemon when `point` is the armed one that ends it, once its
    /// parcel writer has written what was handed to it before.
    pub(crate) fn reach(&self, point: FaultPoint) {
        let Some(armed) = self.ending_at(point) else {
            return;
        };
        armed.reached.send_replace(true);
        let store = armed
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(store) = store {
            store.kept_blocking();
        }
        end_daemon(point);
    }

    /// [`Self::reach`] on the parcel writer's own thread, which cannot wait
    /// for itself: it has done every command handed to it before the one at
    /// hand, whose transaction never commits.
    pub(crate) fn reach_on_writer(&self, point: FaultPoint) {
        let Some(armed) = self.ending_at(point) else {
            return;
        };
        armed.reached.send_replace(true);
        end_daemon(point);
    }

    fn ending_at(&self, point: FaultPoint) -> Option<&Armed> {
        self.armed
            .as_deref()
            .filter(|armed| armed.point == point && point.ends_daemon())
    }

    /// Whether the parcel writer's next write fails.
    pub(crate) fn write_fails(&self) -> bool {
        self.fails_at(FaultPoint::WriteFails)
    }

    /// Stops the parcel writer, on its own thread, until the fault is
    /// released.
    pub(crate) fn pause_writer(&self) {
        self.pause_at(FaultPoint::WriterPaused);
    }

    /// Whether the window table writer's next write of a window's values
    /// fails.
    pub(crate) fn row_update_fails(&self) -> bool {
        self.fails_at(FaultPoint::WindowRowUpdateFails)
    }

    /// Stops the window table's writer, on its own thread, before it writes
    /// a window's values, until the fault is released.
    pub(crate) fn pause_row_update(&self) {
        self.pause_at(FaultPoint::WindowRowUpdatePaused);
    }

    fn fails_at(&self, point: FaultPoint) -> bool {
        let Some(armed) = self.armed.as_ref().filter(|armed| armed.point == point) else {
            return false;
        };
        if armed.is_released() {
            return false;
        }
        armed.reached.send_replace(true);
        true
    }

    fn pause_at(&self, point: FaultPoint) {
        let Some(armed) = self.armed.as_ref().filter(|armed| armed.point == point) else {
            return;
        };
        let mut released = armed.lock_released();
        if *released {
            return;
        }
        armed.reached.send_replace(true);
        while !*released {
            released = armed
                .release
                .wait(released)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Stops a relay session's `wait_for_report` between taking its reports
    /// and handing its answer to the session, until the fault is released.
    pub(crate) async fn hold_report_taken(&self) {
        let Some(armed) = self
            .armed
            .as_ref()
            .filter(|armed| armed.point == FaultPoint::ReportTakenUnanswered)
        else {
            return;
        };
        let mut released = armed.released_watch.subscribe();
        if *released.borrow_and_update() {
            return;
        }
        armed.reached.send_replace(true);
        // The sender lives in `armed`, which this borrow keeps.
        let _ = released.wait_for(|released| *released).await;
    }
}

fn end_daemon(point: FaultPoint) -> ! {
    tracing::warn!(
        point = point.name(),
        "fault point reached; ending the daemon"
    );
    std::process::abort();
}

/// The error a write fails with while [`FaultPoint::WriteFails`] holds: the
/// one a disk that cannot be written returns.
pub(crate) fn simulated_write_failure() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
        Some("simulated write failure".to_owned()),
    )
}

impl Armed {
    fn lock_released(&self) -> MutexGuard<'_, bool> {
        self.released.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_released(&self) -> bool {
        *self.lock_released() || self.signal.as_ref().is_some_and(|path| !path.exists())
    }
}
