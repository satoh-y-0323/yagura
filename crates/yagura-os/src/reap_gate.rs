//! The gate one child's signals and its reaping pass through.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// One child's lock and its "reaped" flag.
///
/// A process id names the child only until the child is reaped; after that
/// the id may be handed to an unrelated process. Every signal to the child
/// and the reaping of the child run under this one lock, and nothing is sent
/// once the child has been reaped, so a signal never reaches a process that
/// took over the id.
///
/// Neither closure may block on the child's exit: a reap under the lock must
/// find the child already exited (known first without reaping it, e.g.
/// through [`crate::pty::PtyChild::wait_for_exit`]), or every signal waits
/// behind it.
#[derive(Debug, Default)]
pub struct ReapGate {
    reaped: Mutex<bool>,
}

impl ReapGate {
    /// A gate for a child that has not been reaped.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, bool> {
        self.reaped.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `send` under the gate's lock unless the child has been reaped.
    /// Returns `None`, without running it, once the child has been reaped.
    pub fn signal_unless_reaped<R>(&self, send: impl FnOnce() -> R) -> Option<R> {
        let reaped = self.lock();
        if *reaped {
            return None;
        }
        Some(send())
    }

    /// Runs `reap` under the gate's lock and marks the child reaped, whatever
    /// `reap` returns: an id whose reap failed is no longer known to name the
    /// child either.
    pub fn reap_with<R>(&self, reap: impl FnOnce() -> R) -> R {
        let mut reaped = self.lock();
        let result = reap();
        *reaped = true;
        result
    }

    /// Whether the child has been reaped.
    #[must_use]
    pub fn is_reaped(&self) -> bool {
        *self.lock()
    }

    /// Runs `body` under the gate's lock with whether the child has been
    /// reaped, so the answer holds for as long as `body` runs.
    #[cfg(target_os = "linux")]
    pub(crate) fn hold<R>(&self, body: impl FnOnce(bool) -> R) -> R {
        let reaped = self.lock();
        body(*reaped)
    }
}
