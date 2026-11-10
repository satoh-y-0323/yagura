//! The points a test of the daemon arms to stand the daemon at one moment of
//! its work: the daemon ends itself there, or its parcel writer fails or
//! stops there until the test releases it. Each point is named by the
//! property it stands for, not by the code it sits in, so a change that moves
//! the property moves the point with it.
//!
//! Only the `fault-injection` feature builds this module: a shipped daemon has
//! neither the points nor a way to arm them.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use thiserror::Error;
use tokio::sync::watch;

use crate::parcel_store::ParcelStore;

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
    /// carried is not settled yet. The daemon ends there.
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
    /// A typed push has written its text and confirmed it in the composer, and
    /// its first submit key is not pressed yet. The daemon ends there.
    BeforeSendCommit,
    /// A typed push has pressed the submit key, and the push has not ended
    /// yet. The daemon ends there.
    AfterSendKey,
    /// A door's answer has taken parcels, the door's connection has moved on
    /// (its next frame, or its end), and the settlement of a taken chat
    /// delivery is not committed yet. The daemon ends there.
    DoorBeforeSettle,
}

impl FaultPoint {
    /// Every point, in the order of [`FaultPoint::name`]'s table.
    pub const ALL: [Self; 9] = [
        Self::HandoffDeposited,
        Self::WriteFails,
        Self::ParcelEndHalfSettled,
        Self::TaskReportHalfWritten,
        Self::WriterPaused,
        Self::ChatPostCommitted,
        Self::BeforeSendCommit,
        Self::AfterSendKey,
        Self::DoorBeforeSettle,
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
            Self::BeforeSendCommit => "before_send_commit",
            Self::AfterSendKey => "after_send_key",
            Self::DoorBeforeSettle => "door_before_settle",
        }
    }

    /// The point named `name`; `None` for any other text.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|point| point.name() == name)
    }

    /// Whether the daemon ends itself at this point; the others act on the
    /// parcel writer.
    #[must_use]
    pub const fn ends_daemon(self) -> bool {
        !matches!(self, Self::WriteFails | Self::WriterPaused)
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
    reached: watch::Sender<bool>,
    /// Waited on before the daemon ends at a point, so what the daemon handed
    /// its writer before the point is written and nothing after it.
    store: Mutex<Option<ParcelStore>>,
    /// Where a door's answer stands, for [`FaultPoint::DoorBeforeSettle`].
    door: Mutex<Door>,
    door_changed: Condvar,
}

/// Where a door's answer stands: the settlement of what it took waits for the
/// door's connection to move on, so the door has read its answer by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Door {
    Idle,
    Answering,
    MovedOn,
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
                reached: watch::channel(false).0,
                store: Mutex::new(None),
                door: Mutex::new(Door::Idle),
                door_changed: Condvar::new(),
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
    /// which nothing outside the process can release.
    pub fn from_env() -> Result<Self, FaultArmError> {
        let Some(name) = std::env::var_os(POINT_ENV) else {
            return Ok(Self::default());
        };
        let name = name.to_string_lossy();
        let point = FaultPoint::from_name(&name)
            .ok_or_else(|| FaultArmError::Unknown(name.into_owned()))?;
        if point == FaultPoint::WriterPaused {
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

    /// Hands the parcel writer of `store` what it acts on, or keeps `store` to
    /// be waited on before the daemon ends at its point.
    pub(crate) fn attach(&self, store: &ParcelStore) {
        let Some(armed) = &self.armed else {
            return;
        };
        if armed.point.ends_daemon() {
            *armed.store.lock().unwrap_or_else(PoisonError::into_inner) = Some(store.clone());
        } else {
            store.arm_writer(self.clone());
        }
    }

    /// Marks a door's answer as under way, before it takes anything: a
    /// settlement it starts then waits for the door's connection to move on.
    pub(crate) fn door_answering(&self) {
        if let Some(armed) = self.door_armed() {
            let mut door = armed.lock_door();
            if *door == Door::Idle {
                *door = Door::Answering;
            }
        }
    }

    /// Ends the door's answer marked by [`Self::door_answering`]: when it took
    /// nothing, nothing waits for its connection.
    pub(crate) fn door_answered(&self, took: bool) {
        if took {
            return;
        }
        if let Some(armed) = self.door_armed() {
            let mut door = armed.lock_door();
            if *door == Door::Answering {
                *door = Door::Idle;
                armed.door_changed.notify_all();
            }
        }
    }

    /// The connection of a door whose answer took parcels has had its next
    /// event: a frame, or its end.
    pub(crate) fn door_moved_on(&self) {
        if let Some(armed) = self.door_armed() {
            let mut door = armed.lock_door();
            if *door == Door::Answering {
                *door = Door::MovedOn;
                armed.door_changed.notify_all();
            }
        }
    }

    fn door_armed(&self) -> Option<&Armed> {
        self.armed
            .as_deref()
            .filter(|armed| armed.point == FaultPoint::DoorBeforeSettle)
    }

    /// Ends the daemon when `point` is the armed one that ends it.
    /// [`FaultPoint::DoorBeforeSettle`] blocks the calling thread until the
    /// door's connection has moved on, and does nothing without a door's
    /// answer under way.
    pub(crate) fn reach(&self, point: FaultPoint) {
        let Some(armed) = self.armed.as_ref().filter(|armed| armed.point == point) else {
            return;
        };
        if !point.ends_daemon() {
            return;
        }
        if point == FaultPoint::DoorBeforeSettle && !armed.door_moved_on() {
            return;
        }
        armed.reached.send_replace(true);
        let store = armed
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(store) = store {
            store.kept_blocking();
        }
        tracing::warn!(
            point = point.name(),
            "fault point reached; ending the daemon"
        );
        std::process::abort();
    }

    /// Whether the parcel writer's next write fails.
    pub(crate) fn write_fails(&self) -> bool {
        let Some(armed) = self
            .armed
            .as_ref()
            .filter(|armed| armed.point == FaultPoint::WriteFails)
        else {
            return false;
        };
        if armed.is_released() {
            return false;
        }
        armed.reached.send_replace(true);
        true
    }

    /// Stops the parcel writer, on its own thread, until the fault is
    /// released.
    pub(crate) fn pause_writer(&self) {
        let Some(armed) = self
            .armed
            .as_ref()
            .filter(|armed| armed.point == FaultPoint::WriterPaused)
        else {
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
}

impl Armed {
    fn lock_released(&self) -> MutexGuard<'_, bool> {
        self.released.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_door(&self) -> MutexGuard<'_, Door> {
        self.door.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits out a door's answer under way; whether its connection moved on.
    fn door_moved_on(&self) -> bool {
        let mut door = self.lock_door();
        while *door == Door::Answering {
            door = self
                .door_changed
                .wait(door)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *door == Door::MovedOn
    }

    fn is_released(&self) -> bool {
        *self.lock_released() || self.signal.as_ref().is_some_and(|path| !path.exists())
    }
}
