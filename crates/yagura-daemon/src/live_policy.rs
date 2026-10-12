//! The daemon-wide cell a live approval policy lives in.
//!
//! [`DaemonConfig::approval_policy`] and [`DaemonConfig::plan_approval_required`]
//! are read exactly once, at boot ([`PolicyCell::from_config`]); every
//! decision made afterwards -- the destructive-operation gate, a handoff's
//! pre-judgement, a chat post's gate, a new plan's review flag -- reads a
//! [`PolicyCell`] snapshot instead, so a toggle from the operator can take
//! effect without a restart. Writes are serialized ([`PolicyCell::lock_writes`])
//! so a read-modify-write toggle can never race another one.

use std::sync::{Arc, RwLock};

use tokio::sync::{Mutex, MutexGuard};

use crate::config::DaemonConfig;
use crate::control::{ApprovalPolicy, ApprovalSection, DestructiveTool, PolicySource};

/// Where the cell's policy came from, mirroring [`PolicySource`] but owning
/// its [`ApprovalPolicy`] rather than borrowing one: a write installs a new,
/// modified policy, which needs somewhere of its own to live.
///
/// `Loaded` is boxed so the other, data-less variants do not each pay for
/// [`ApprovalPolicy`]'s size (`clippy::large_enum_variant`).
#[derive(Debug, Clone)]
pub enum LiveSource {
    /// No playbook was configured.
    NotConfigured,
    /// A playbook was configured and loaded (or has since been modified by a
    /// write through this cell).
    Loaded(Box<ApprovalPolicy>),
    /// A playbook was configured but failed to load.
    Failed,
}

/// One immutable snapshot of the daemon's live approval policy.
///
/// `plans_require_approval` is deliberately not named after
/// [`DaemonConfig::plan_approval_required`]: only [`PolicyCell::from_config`]
/// may read the latter, and giving the cell's own field a different spelling
/// keeps a future rename of either one from blurring that boundary.
#[derive(Debug, Clone)]
pub struct LivePolicy {
    /// The playbook state, as [`policy_source`] turns back into a
    /// [`PolicySource`] for the gate.
    pub source: LiveSource,
    /// Whether a newly created plan waits for the operator's review before
    /// any of its task windows can open.
    pub plans_require_approval: bool,
}

/// The daemon-wide cell every policy decision reads from.
///
/// Reads take a cheap `Arc` clone under a plain [`RwLock`]; writes are
/// serialized by [`lock_writes`](PolicyCell::lock_writes) and swap in a whole
/// new snapshot, so a reader that already holds one never observes a
/// partially applied change.
#[derive(Debug)]
pub struct PolicyCell {
    state: RwLock<Arc<LivePolicy>>,
    /// Serializes writers: a `spawn_blocking` file rewrite and the in-memory
    /// toggle it licenses must never interleave with another one.
    write_lock: Mutex<()>,
}

impl PolicyCell {
    /// Builds a cell from `config`'s startup fields.
    #[must_use]
    pub fn from_config(config: &DaemonConfig) -> Self {
        let source = match config.approval_policy.as_ref() {
            Some(policy) => LiveSource::Loaded(Box::new(policy.clone())),
            None if config.playbook.is_some() => LiveSource::Failed,
            None => LiveSource::NotConfigured,
        };
        let policy = LivePolicy {
            source,
            plans_require_approval: config.plan_approval_required,
        };
        Self {
            state: RwLock::new(Arc::new(policy)),
            write_lock: Mutex::new(()),
        }
    }

    /// The cell's current value.
    ///
    /// Poisoning is read through rather than propagated (there is no
    /// panicking writer to poison this lock with in practice, and a reader
    /// must never itself panic over a lock's history), so a poisoned lock is
    /// treated exactly like a healthy one.
    #[must_use]
    pub fn snapshot(&self) -> Arc<LivePolicy> {
        let guard = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&guard)
    }

    /// Serializes writers: only one [`PolicyWriteGuard`] can exist at a time,
    /// so a read-modify-write toggle is never lost to another one running
    /// concurrently.
    pub async fn lock_writes(&self) -> PolicyWriteGuard<'_> {
        let permit = self.write_lock.lock().await;
        PolicyWriteGuard {
            cell: self,
            _permit: permit,
        }
    }

    fn store(&self, policy: LivePolicy) {
        let new_value = Arc::new(policy);
        match self.state.write() {
            Ok(mut guard) => *guard = new_value,
            Err(poisoned) => *poisoned.into_inner() = new_value,
        }
    }
}

/// The write side of a [`PolicyCell`]: obtained from
/// [`PolicyCell::lock_writes`], which admits only one of these at a time.
pub struct PolicyWriteGuard<'a> {
    cell: &'a PolicyCell,
    _permit: MutexGuard<'a, ()>,
}

impl PolicyWriteGuard<'_> {
    /// Turns `tool`'s auto-approval on or off.
    ///
    /// Turning it off purges every entry for `tool`'s name, including
    /// duplicates. A cell with nothing [`LiveSource::Loaded`] never changes
    /// (fail-closed: a write through a broken or absent playbook must not
    /// conjure a policy into existence). Returns whether anything changed;
    /// a request that already matches the current value is a no-op.
    pub fn set_auto_approve(&mut self, tool: DestructiveTool, on: bool) -> bool {
        let current = self.cell.snapshot();
        let LiveSource::Loaded(policy) = &current.source else {
            return false;
        };
        if policy.allows(tool) == on {
            return false;
        }
        let mut updated_policy = policy.clone();
        let mut names = updated_policy
            .approval
            .take()
            .map(|section| section.auto_approve)
            .unwrap_or_default();
        let name = tool.as_tool_name();
        if on {
            names.push(name.to_string());
        } else {
            names.retain(|entry| entry != name);
        }
        updated_policy.approval = Some(ApprovalSection {
            auto_approve: names,
        });
        let mut updated = (*current).clone();
        updated.source = LiveSource::Loaded(updated_policy);
        self.cell.store(updated);
        true
    }

    /// Turns the plan-review requirement on or off. Returns whether anything
    /// changed; a request that already matches the current value is a no-op.
    pub fn set_plan_approval_required(&mut self, required: bool) -> bool {
        let current = self.cell.snapshot();
        if current.plans_require_approval == required {
            return false;
        }
        let mut updated = (*current).clone();
        updated.plans_require_approval = required;
        self.cell.store(updated);
        true
    }
}

/// The [`PolicySource`] the gate judges a request against, derived from a
/// snapshot's [`LiveSource`].
#[must_use]
pub fn policy_source(policy: &LivePolicy) -> PolicySource<'_> {
    match &policy.source {
        LiveSource::NotConfigured => PolicySource::NotConfigured,
        LiveSource::Loaded(policy) => PolicySource::Loaded(policy),
        LiveSource::Failed => PolicySource::Failed,
    }
}
