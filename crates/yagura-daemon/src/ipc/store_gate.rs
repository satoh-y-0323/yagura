//! The one door a store-holding mutex is reached through:
//! [`StoreGate::with_store`] runs a closure on a blocking thread, holding the
//! store's own async mutex only for the closure's duration.
//!
//! `blocking_lock` on a `tokio::sync::Mutex` panics when called from an
//! async context, so the door must be the sole call site: [`StoreGate`] never
//! hands its mutex out, and the type is generic over which store it holds --
//! a second store reaching its lock through a door of its own would defeat
//! the guarantee just as surely as a second call site sharing this one would.

use std::sync::Arc;

use tokio::task::JoinError;

/// Why an access through [`StoreGate::with_store`] could not run its
/// closure.
#[derive(Debug)]
pub(super) enum StoreAccessError {
    /// No store was opened at all.
    Disabled,
    /// The blocking task itself could not be joined (a panic inside the
    /// closure, or the runtime shutting down).
    Failed(JoinError),
}

/// An opened store of type `T`, or the absence of one.
pub(super) struct StoreGate<T> {
    store: Option<Arc<tokio::sync::Mutex<T>>>,
}

impl<T> StoreGate<T> {
    /// No store at all: every access answers [`StoreAccessError::Disabled`].
    pub(super) fn disabled() -> Self {
        Self { store: None }
    }

    /// Takes ownership of an already-opened store.
    pub(super) fn opened(store: T) -> Self {
        Self {
            store: Some(Arc::new(tokio::sync::Mutex::new(store))),
        }
    }

    /// `true` when a store was opened.
    pub(super) fn is_enabled(&self) -> bool {
        self.store.is_some()
    }
}

impl<T: Send + 'static> StoreGate<T> {
    /// The one door to `T` (module doc): runs `f` on a blocking thread,
    /// holding the store's own async mutex only for `f`'s duration.
    ///
    /// # Errors
    ///
    /// Returns [`StoreAccessError::Disabled`] when no store was opened, and
    /// [`StoreAccessError::Failed`] when the blocking task itself could not
    /// be joined (a panic inside `f`, or the runtime shutting down).
    pub(super) async fn with_store<R, F>(&self, f: F) -> Result<R, StoreAccessError>
    where
        F: FnOnce(&mut T) -> R + Send + 'static,
        R: Send + 'static,
    {
        let Some(store) = self.store.clone() else {
            return Err(StoreAccessError::Disabled);
        };
        tokio::task::spawn_blocking(move || {
            // Safe here only: this closure runs on the blocking pool, never on a runtime thread.
            #[allow(clippy::disallowed_methods)]
            let mut guard = store.blocking_lock();
            f(&mut guard)
        })
        .await
        .map_err(StoreAccessError::Failed)
    }
}
