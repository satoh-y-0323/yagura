//! The process group that owns a child's whole process tree on Unix.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::OsError;

/// Grace period between `SIGTERM` and the `SIGKILL` escalation for the groups
/// this crate creates itself.
pub(crate) const DEFAULT_TERM_GRACE: Duration = Duration::from_millis(150);

/// A Unix process group tracked for termination.
///
/// The group exists only together with its leader: its id is the leader's
/// pid, known from having spawned the leader, so there is no state in which
/// the group's id is still unknown.
///
/// The group id can be reused by an unrelated process once every member of
/// the original group has exited (same-uid, bounded to the grace window); that
/// residual risk is accepted rather than re-verified on every escalation.
#[derive(Debug)]
pub struct UnixProcessGroup {
    pgid: libc::pid_t,
    terminated: AtomicBool,
    grace: Duration,
}

impl UnixProcessGroup {
    /// The process group led by `pid`, ended with `SIGTERM` and then, once
    /// `grace` has passed, `SIGKILL`. `Duration::ZERO` skips straight to
    /// `SIGKILL`.
    ///
    /// Only valid for a child started with `setsid` (or `setpgid(0, 0)`)
    /// before it runs: such a child's process group id equals its own pid, so
    /// the id is known without reading it back. Reading it back with `getpgid`
    /// would race a child that exits and is reaped first -- macOS reports
    /// `ESRCH` for a zombie's process group, where Linux does not.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Security`] when `pid` is 0, does not fit a
    /// `pid_t`, or equals this process's own process group id, since
    /// signalling any of those would reach this process itself.
    pub fn led_by(pid: u32, grace: Duration) -> Result<Self, OsError> {
        let pgid = libc::pid_t::try_from(pid)
            .ok()
            .filter(|&pgid| pgid > 0)
            .ok_or_else(|| {
                OsError::Security(format!("{pid} is not a valid process group leader id"))
            })?;
        // SAFETY: getpgrp takes no arguments and only reads this process's own
        // process group id.
        let own_pgrp = unsafe { libc::getpgrp() };
        if pgid == own_pgrp {
            return Err(OsError::Security(format!(
                "process {pid} would lead this process's own process group ({own_pgrp}); \
                 refusing it, since terminating the group would then signal this process \
                 itself"
            )));
        }
        Ok(Self {
            pgid,
            terminated: AtomicBool::new(false),
            grace,
        })
    }

    /// Ends every process in the group: `SIGTERM`, then `SIGKILL` once the
    /// grace period has passed. Idempotent: only the first call signals.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Security`] when a live member cannot be signalled
    /// by the initial request (or by the forced kill when the grace period is
    /// zero), or the group's members cannot be enumerated. With a non-zero
    /// grace period the forced kill is sent by a detached thread and its
    /// outcome is not reported.
    pub fn terminate(&self) -> Result<(), OsError> {
        if self.terminated.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let pgid = self.pgid;

        if !send_signal(pgid, libc::SIGTERM)? {
            // Every process in the group had already exited before SIGTERM
            // was even sent; nothing is left to escalate.
            return Ok(());
        }

        if self.grace.is_zero() {
            send_signal(pgid, libc::SIGKILL)?;
            return Ok(());
        }

        let grace = self.grace;
        std::thread::spawn(move || {
            std::thread::sleep(grace);
            let _ = send_signal(pgid, libc::SIGKILL);
        });
        Ok(())
    }
}

impl Drop for UnixProcessGroup {
    fn drop(&mut self) {
        if self.terminated.load(Ordering::SeqCst) {
            return;
        }
        // No grace period here: a group dropped without an explicit
        // `terminate` call has no one left to wait out one.
        let _ = send_signal(self.pgid, libc::SIGKILL);
    }
}

/// Sends `signal` to every process in group `pgid`.
///
/// Returns `Ok(true)` when at least one process received it. Returns
/// `Ok(false)` when the group has no live member left (`ESRCH`; on macOS
/// also `EPERM` when every member is a zombie). Returns `Err` when a live
/// member could not be signaled, or the group could not be enumerated.
fn send_signal(pgid: libc::pid_t, signal: i32) -> Result<bool, OsError> {
    // SAFETY: killpg signals every process in process group `pgid` with
    // `signal`; `pgid` comes from `UnixProcessGroup::led_by`, which rejects 0
    // and this process's own process group.
    let result = unsafe { libc::killpg(pgid, signal) };
    if result == 0 {
        return Ok(true);
    }
    failed_signal_outcome(pgid, signal, std::io::Error::last_os_error())
}

/// What a failed `killpg(pgid, signal)` means, given the `errno` it left.
/// Separate from the call so an `errno` an unprivileged test cannot provoke
/// (`EPERM` from a group of another user's processes) can still be fed in.
fn failed_signal_outcome(
    pgid: libc::pid_t,
    signal: i32,
    errno: std::io::Error,
) -> Result<bool, OsError> {
    if errno.raw_os_error() == Some(libc::ESRCH) {
        return Ok(false);
    }
    if errno.raw_os_error() == Some(libc::EPERM) {
        return if eperm_means_every_member_is_gone(pgid)? {
            Ok(false)
        } else {
            Err(OsError::Security(format!(
                "killpg({pgid}, {signal}) failed: {errno}; a live member could not be signaled"
            )))
        };
    }
    Err(OsError::Security(format!(
        "killpg({pgid}, {signal}) failed: {errno}"
    )))
}

/// Whether any of `members` is still alive and in group `pgid` (callers
/// guarantee a member `pgid_of` maps to `None` has already exited); factored
/// out so this fold can be unit-tested apart from the macOS-only syscalls.
#[cfg(any(target_os = "macos", test))]
fn any_member_alive(
    pgid: libc::pid_t,
    members: &[libc::pid_t],
    pgid_of: impl Fn(libc::pid_t) -> Option<libc::pid_t>,
) -> bool {
    members.iter().any(|&member| pgid_of(member) == Some(pgid))
}

/// Turns a two-call size-query/list pattern into a validated pid list.
/// `needed_entries` and `list`'s return value are both pid *counts*; the
/// buffer length `list` receives is *bytes* (the syscall wrapper's unit).
///
/// Reachability: the wrapper folds a failed query into a count of zero
/// rather than a negative value, so only the zero-count guards below fire in
/// practice; the negative-count guards are unreachable in production and
/// exist only as a fail-closed contract, exercised by `mod tests`.
#[cfg(any(target_os = "macos", test))]
fn collect_group_members(
    needed_entries: libc::c_int,
    list: impl FnOnce(&mut [libc::pid_t]) -> libc::c_int,
) -> Result<Vec<libc::pid_t>, OsError> {
    if needed_entries < 0 {
        return Err(OsError::Security(format!(
            "proc_listpgrppids size query failed: {needed_entries}"
        )));
    }
    if needed_entries == 0 {
        return Err(OsError::Security(
            "proc_listpgrppids size query returned no capacity".to_string(),
        ));
    }
    let capacity = needed_entries as usize;
    let mut buf: Vec<libc::pid_t> = vec![0; capacity];
    let count = list(&mut buf);
    if count < 0 {
        return Err(OsError::Security(format!(
            "proc_listpgrppids negative count: {count}"
        )));
    }
    if count == 0 {
        return Err(OsError::Security(
            "proc_listpgrppids no members".to_string(),
        ));
    }
    let count = count as usize;
    if count >= capacity {
        return Err(OsError::Security(format!(
            "proc_listpgrppids returned {count} members for capacity {capacity}, which cannot \
             account for every member"
        )));
    }
    if let Some(&pid) = buf[..count].iter().find(|&&pid| pid <= 0) {
        return Err(OsError::Security(format!(
            "proc_listpgrppids out-of-contract pid: {pid}"
        )));
    }
    buf.truncate(count);
    Ok(buf)
}

/// XNU's `killpg1` returns `EPERM` (not `ESRCH`) precisely when the group
/// still exists but every member is a zombie (`nfound == 0` in a `posix`
/// build); this tells the two cases apart by enumerating the group itself.
#[cfg(target_os = "macos")]
fn eperm_means_every_member_is_gone(pgid: libc::pid_t) -> Result<bool, OsError> {
    // SAFETY: a NULL, zero-length query only asks proc_listpgrppids for the
    // pid count it would need; it writes through neither pointer.
    let needed = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
    let members = collect_group_members(needed, |buf| {
        // SAFETY: `buf` is a live allocation of `buf.len()` elements; the
        // size passed is the corresponding byte count, matching what
        // proc_listpgrppids documents as `buffersize`.
        unsafe {
            libc::proc_listpgrppids(
                pgid,
                buf.as_mut_ptr().cast(),
                std::mem::size_of_val(buf) as libc::c_int,
            )
        }
    })?;
    Ok(!any_member_alive(pgid, &members, member_pgid))
}

/// The process group `pid` is in now, or `None` once it has exited: getpgid's
/// only failure path is proc_find failing to locate the pid.
#[cfg(any(target_os = "macos", test))]
fn member_pgid(pid: libc::pid_t) -> Option<libc::pid_t> {
    // SAFETY: getpgid only reads the process group of the pid supplied; it
    // has no other effect.
    let candidate_pgid = unsafe { libc::getpgid(pid) };
    (candidate_pgid >= 0).then_some(candidate_pgid)
}

/// This OS does not distinguish `EPERM` from a genuine permission error, so
/// `send_signal` treats it as one instead of re-checking group membership.
#[cfg(not(target_os = "macos"))]
fn eperm_means_every_member_is_gone(_pgid: libc::pid_t) -> Result<bool, OsError> {
    Ok(false)
}

// =========================================================================
// Unit tests for the macOS EPERM-vs-zombie-group private helpers
// =========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // any_member_alive: a pure fold over `members`, so these four cases are
    // exhaustive over its branch structure (empty / all-None / one hit /
    // one miss).

    #[test]
    fn any_member_alive_is_false_for_an_empty_slice() {
        let pgid_of = |_: libc::pid_t| -> Option<libc::pid_t> { None };
        assert!(!any_member_alive(123, &[], pgid_of));
    }

    #[test]
    fn any_member_alive_is_false_when_every_member_maps_to_none() {
        let members: [libc::pid_t; 3] = [1, 2, 3];
        let pgid_of = |_: libc::pid_t| -> Option<libc::pid_t> { None };
        assert!(!any_member_alive(123, &members, pgid_of));
    }

    #[test]
    fn any_member_alive_is_true_when_one_member_maps_to_the_group_pgid() {
        let members: [libc::pid_t; 3] = [1, 2, 3];
        let pgid_of =
            |pid: libc::pid_t| -> Option<libc::pid_t> { if pid == 2 { Some(123) } else { None } };
        assert!(any_member_alive(123, &members, pgid_of));
    }

    #[test]
    fn any_member_alive_is_false_when_a_member_maps_to_a_different_pgid() {
        let members: [libc::pid_t; 3] = [1, 2, 3];
        let pgid_of =
            |pid: libc::pid_t| -> Option<libc::pid_t> { if pid == 2 { Some(456) } else { None } };
        assert!(!any_member_alive(123, &members, pgid_of));
    }

    // A pid that is known to have exited needs a child this test reaps itself.
    #[allow(clippy::disallowed_methods)]
    #[test]
    fn member_pgid_is_the_group_of_a_live_pid_and_none_once_it_is_reaped() {
        // SAFETY: getpgrp cannot fail and has no side effects.
        let own_group = unsafe { libc::getpgrp() };
        assert_eq!(
            member_pgid(std::process::id() as libc::pid_t),
            Some(own_group)
        );

        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        child.wait().unwrap();
        assert_eq!(member_pgid(pid), None);
    }

    // collect_group_members: each of the 6 fail-closed guards, plus the
    // truncate(count) success path, one variant per test so a mutation that
    // collapses two guards into one still shows up as a distinct failure.

    /// Every `Err` from `collect_group_members` must name `proc_listpgrppids`
    /// and the specific guard that rejected it, so no two guards can be
    /// satisfied by the same vacuous assertion.
    fn assert_names_the_guard(err: &OsError, guard_phrase: &str) {
        let OsError::Security(message) = err else {
            panic!("collect_group_members must report failures as OsError::Security, got {err:?}");
        };
        assert!(
            message.contains("proc_listpgrppids"),
            "every collect_group_members error must name proc_listpgrppids: {message}"
        );
        assert!(
            message.contains(guard_phrase),
            "expected the identifying phrase {guard_phrase:?} in the error message: {message}"
        );
    }

    #[test]
    fn collect_group_members_rejects_a_negative_size_query() {
        let list = |_: &mut [libc::pid_t]| -> libc::c_int { 0 };
        let err =
            collect_group_members(-1, list).expect_err("a negative size query must be rejected");
        assert_names_the_guard(&err, "size query failed");
    }

    #[test]
    fn collect_group_members_rejects_a_zero_size_query() {
        let list = |_: &mut [libc::pid_t]| -> libc::c_int { 0 };
        let err = collect_group_members(0, list)
            .expect_err("a zero-capacity size query must be rejected");
        assert_names_the_guard(&err, "size query returned no capacity");
    }

    #[test]
    fn collect_group_members_rejects_a_negative_result_count() {
        let list = |_: &mut [libc::pid_t]| -> libc::c_int { -1 };
        let err =
            collect_group_members(4, list).expect_err("a negative result count must be rejected");
        assert_names_the_guard(&err, "negative count");
    }

    #[test]
    fn collect_group_members_rejects_a_zero_result_count() {
        let list = |_: &mut [libc::pid_t]| -> libc::c_int { 0 };
        let err = collect_group_members(4, list).expect_err("a zero result count must be rejected");
        assert_names_the_guard(&err, "no members");
    }

    #[test]
    fn collect_group_members_rejects_a_count_at_or_over_capacity() {
        let list = |buf: &mut [libc::pid_t]| -> libc::c_int { buf.len() as libc::c_int };
        let err = collect_group_members(4, list)
            .expect_err("a count that fills the whole buffer must be rejected");
        assert_names_the_guard(&err, "for capacity");
    }

    #[test]
    fn collect_group_members_rejects_an_out_of_contract_pid() {
        let list = |buf: &mut [libc::pid_t]| -> libc::c_int {
            buf[0] = 0;
            1
        };
        let err = collect_group_members(4, list)
            .expect_err("a pid <= 0 in the filled slice must be rejected");
        assert_names_the_guard(&err, "out-of-contract pid");
    }

    // On Linux EPERM from killpg means no member could be signalled, so a
    // group of another user's processes must surface as an error rather than
    // as already gone.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn eperm_from_killpg_is_reported_as_a_live_member_that_could_not_be_signalled() {
        let outcome = failed_signal_outcome(
            123,
            libc::SIGTERM,
            std::io::Error::from_raw_os_error(libc::EPERM),
        );

        let Err(OsError::Security(message)) = outcome else {
            panic!("EPERM must be reported as OsError::Security, got {outcome:?}");
        };
        assert!(
            message.contains("could not be signaled"),
            "the error must say a live member could not be signalled: {message}"
        );
    }

    #[test]
    fn collect_group_members_truncates_to_the_reported_count() {
        let list = |buf: &mut [libc::pid_t]| -> libc::c_int {
            buf[0] = 111;
            buf[1] = 222;
            2
        };
        let members = collect_group_members(4, list)
            .expect("a count below capacity with only positive pids must succeed");
        assert_eq!(members, vec![111, 222]);
    }
}
