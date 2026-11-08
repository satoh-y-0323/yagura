//! The process group that owns a child's whole process tree on Unix.

use std::sync::{Mutex, PoisonError};
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
    /// Set by the first `terminate`, which holds the lock until it has sent
    /// everything, so every later call waits for it.
    terminated: Mutex<bool>,
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
            terminated: Mutex::new(false),
            grace,
        })
    }

    /// Ends every process in the group and returns once the signals are sent:
    /// `SIGTERM`, then, if a live member received it, `SIGKILL` after the
    /// grace period. The group need not be empty on return -- the signals are
    /// delivered, the processes' exits are the kernel's. Idempotent: only the
    /// first call signals, and every call returns only after the first one
    /// has finished.
    ///
    /// Blocks for the grace period when a live member received `SIGTERM`;
    /// callers on an async runtime run it on a thread of their own.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Security`] when a live member cannot be signalled,
    /// or the group's members cannot be enumerated. Only the first call
    /// reports it.
    pub fn terminate(&self) -> Result<(), OsError> {
        let mut terminated = self
            .terminated
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if std::mem::replace(&mut *terminated, true) {
            return Ok(());
        }
        let pgid = self.pgid;

        if !send_signal(pgid, libc::SIGTERM)? {
            // Every process in the group had already exited before SIGTERM
            // was even sent; nothing is left to escalate.
            return Ok(());
        }
        if !self.grace.is_zero() {
            std::thread::sleep(self.grace);
        }
        send_signal(pgid, libc::SIGKILL)?;
        Ok(())
    }

    /// Sends `SIGKILL` to every process in the group now, without a grace
    /// period, and counts as [`Self::terminate`] having run, so nothing is
    /// sent when the group is dropped. Repeatable; waits for a `terminate`
    /// in progress. Returns whether a live member received it.
    ///
    /// Only for while the leader is unreaped: its zombie keeps the group id
    /// from being reused.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Security`] when a live member cannot be signalled,
    /// or the group's members cannot be enumerated.
    pub fn kill_now(&self) -> Result<bool, OsError> {
        let mut terminated = self
            .terminated
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *terminated = true;
        send_signal(self.pgid, libc::SIGKILL)
    }
}

impl Drop for UnixProcessGroup {
    fn drop(&mut self) {
        if *self
            .terminated
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
        {
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
pub(crate) fn send_signal(pgid: libc::pid_t, signal: i32) -> Result<bool, OsError> {
    // SAFETY: killpg signals every process in process group `pgid` with
    // `signal`; every caller rejects 0 and this process's own process group
    // first (`UnixProcessGroup::led_by`, the tree sweep's `leader_group`).
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

/// One reading of a process group's members.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum GroupListing {
    /// Every member the reading found; empty when it found none.
    Members(Vec<libc::pid_t>),
    /// The buffer filled up, so members may be missing from the reading.
    Full,
}

/// Turns a two-call size-query/list pattern into one reading of a group.
///
/// Both `needed_entries` and `list`'s return value count pids (entries); the
/// buffer `list` receives is `needed_entries` pids long. Bytes appear only
/// where the buffer is handed to the system ([`BufferBytes`]). What an empty
/// or [`GroupListing::Full`] reading means is the caller's to decide.
///
/// # Errors
///
/// Returns [`OsError::Security`] for a negative size or count, or a listed
/// pid that is not positive.
#[cfg(any(target_os = "macos", test))]
fn list_group_members(
    needed_entries: libc::c_int,
    list: impl FnOnce(&mut [libc::pid_t]) -> libc::c_int,
) -> Result<GroupListing, OsError> {
    list_pids("proc_listpgrppids", 1, needed_entries, list)
}

/// [`list_group_members`] for any listing of pids `call` names, whose pids
/// are all at least `lowest`.
#[cfg(any(target_os = "macos", test))]
fn list_pids(
    call: &str,
    lowest: libc::pid_t,
    needed_entries: libc::c_int,
    list: impl FnOnce(&mut [libc::pid_t]) -> libc::c_int,
) -> Result<GroupListing, OsError> {
    let capacity = usize::try_from(needed_entries)
        .map_err(|_| OsError::Security(format!("{call} size query failed: {needed_entries}")))?;
    if capacity == 0 {
        return Ok(GroupListing::Members(Vec::new()));
    }
    let mut buf: Vec<libc::pid_t> = vec![0; capacity];
    let count = list(&mut buf);
    let count = usize::try_from(count)
        .map_err(|_| OsError::Security(format!("{call} negative count: {count}")))?;
    if count >= capacity {
        return Ok(GroupListing::Full);
    }
    buf.truncate(count);
    if let Some(&pid) = buf.iter().find(|&&pid| pid < lowest) {
        return Err(OsError::Security(format!(
            "{call} out-of-contract pid: {pid}"
        )));
    }
    Ok(GroupListing::Members(buf))
}

/// The ids of every process, read again into a larger buffer for as long as
/// a reading fills its buffer, since a full one may have missed some.
///
/// # Errors
///
/// Returns the error the reading fails with.
#[cfg(target_os = "macos")]
pub(crate) fn all_pids() -> std::io::Result<Vec<u32>> {
    let mut floor: libc::c_int = 0;
    loop {
        // SAFETY: a NULL, zero-length query only asks for the pid count it
        // would need; it writes through neither pointer.
        let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let entries = needed.max(floor);
        // The kernel's own task is listed as pid 0.
        let listing = list_pids("proc_listallpids", 0, entries, |buf| {
            let Some(bytes) = BufferBytes::of(buf) else {
                return libc::c_int::MAX;
            };
            // SAFETY: `buf` is a live allocation of at least `bytes.0` bytes.
            unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes.0) }
        })
        .map_err(std::io::Error::other)?;
        match listing {
            GroupListing::Members(pids) => {
                return Ok(pids
                    .into_iter()
                    .filter_map(|pid| u32::try_from(pid).ok())
                    .filter(|&pid| pid != 0)
                    .collect());
            }
            GroupListing::Full => {
                floor = entries
                    .checked_mul(2)
                    .filter(|&next| BufferBytes::fits(next))
                    .ok_or_else(|| std::io::Error::other("the process list outgrew its buffer"))?;
            }
        }
    }
}

/// A buffer's length in bytes, the unit `proc_listpgrppids` takes its buffer
/// size in, while it counts what it returns in pids.
#[cfg(target_os = "macos")]
struct BufferBytes(libc::c_int);

#[cfg(target_os = "macos")]
impl BufferBytes {
    /// Whether a buffer of `entries` pids can be described in bytes.
    fn fits(entries: libc::c_int) -> bool {
        usize::try_from(entries)
            .ok()
            .and_then(|entries| entries.checked_mul(std::mem::size_of::<libc::pid_t>()))
            .is_some_and(|bytes| libc::c_int::try_from(bytes).is_ok())
    }

    fn of(buf: &[libc::pid_t]) -> Option<Self> {
        libc::c_int::try_from(std::mem::size_of_val(buf))
            .ok()
            .map(Self)
    }
}

/// Reads the members of group `pgid` from `proc_listpgrppids`.
#[cfg(target_os = "macos")]
fn read_group_listing(pgid: libc::pid_t) -> Result<GroupListing, OsError> {
    // SAFETY: a NULL, zero-length query only asks proc_listpgrppids for the
    // pid count it would need; it writes through neither pointer.
    let needed = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
    list_group_members(needed, |buf| {
        // A buffer too large to describe in bytes reads as full, never as a
        // group without members.
        let Some(bytes) = BufferBytes::of(buf) else {
            return libc::c_int::MAX;
        };
        // SAFETY: `buf` is a live allocation of at least `bytes.0` bytes.
        unsafe { libc::proc_listpgrppids(pgid, buf.as_mut_ptr().cast(), bytes.0) }
    })
}

/// Whether a group `killpg` answered with `EPERM` has no live member left,
/// judged from one `listing` of it. A reading that found no member or may
/// have missed some proves nothing, so it fails closed.
#[cfg(any(target_os = "macos", test))]
fn eperm_means_every_member_is_gone_from(
    pgid: libc::pid_t,
    listing: GroupListing,
    pgid_of: impl Fn(libc::pid_t) -> Option<libc::pid_t>,
) -> Result<bool, OsError> {
    match listing {
        GroupListing::Members(members) if members.is_empty() => Err(OsError::Security(format!(
            "proc_listpgrppids found no member of group {pgid}"
        ))),
        GroupListing::Members(members) => Ok(!any_member_alive(pgid, &members, pgid_of)),
        GroupListing::Full => Err(OsError::Security(format!(
            "proc_listpgrppids could not hold every member of group {pgid}"
        ))),
    }
}

/// XNU's `killpg1` returns `EPERM` (not `ESRCH`) precisely when the group
/// still exists but every member is a zombie (`nfound == 0` in a `posix`
/// build); this tells the two cases apart by enumerating the group itself.
#[cfg(target_os = "macos")]
fn eperm_means_every_member_is_gone(pgid: libc::pid_t) -> Result<bool, OsError> {
    eperm_means_every_member_is_gone_from(pgid, read_group_listing(pgid)?, member_pgid)
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

    // The member listing and the EPERM judgment take the listing as a value,
    // so each property is measured apart from the macOS calls.

    /// A listing that cannot be trusted must fail closed as `Security`.
    fn assert_fail_closed(err: OsError) {
        assert!(
            matches!(err, OsError::Security(_)),
            "an untrusted listing must fail closed as OsError::Security, got {err:?}"
        );
    }

    #[test]
    fn listing_fails_closed_on_a_negative_size_query() {
        let list = |_: &mut [libc::pid_t]| -> libc::c_int { 0 };
        assert_fail_closed(
            list_group_members(-1, list).expect_err("a negative size query must be rejected"),
        );
    }

    #[test]
    fn listing_fails_closed_on_a_negative_count() {
        let list = |_: &mut [libc::pid_t]| -> libc::c_int { -1 };
        assert_fail_closed(
            list_group_members(4, list).expect_err("a negative count must be rejected"),
        );
    }

    #[test]
    fn listing_fails_closed_on_an_out_of_contract_pid() {
        let list = |buf: &mut [libc::pid_t]| -> libc::c_int {
            buf[0] = 0;
            1
        };
        assert_fail_closed(
            list_group_members(4, list)
                .expect_err("a pid <= 0 in the listed slice must be rejected"),
        );
    }

    #[test]
    fn listing_reports_a_full_buffer_instead_of_a_list_that_may_be_cut_short() {
        let list = |buf: &mut [libc::pid_t]| -> libc::c_int { buf.len() as libc::c_int };
        let listing = list_group_members(4, list).expect("a full buffer is not a call failure");
        assert_eq!(listing, GroupListing::Full);
    }

    #[test]
    fn listing_of_a_group_with_no_members_is_an_empty_list_not_a_failure() {
        let list = |_: &mut [libc::pid_t]| -> libc::c_int { 0 };
        let listing = list_group_members(4, list).expect("no members is not a call failure");
        assert_eq!(listing, GroupListing::Members(Vec::new()));
        let listing = list_group_members(0, list).expect("a zero-capacity query is not a failure");
        assert_eq!(listing, GroupListing::Members(Vec::new()));
    }

    #[test]
    fn eperm_with_an_empty_listing_fails_closed_rather_than_proving_the_group_gone() {
        let pgid_of = |_: libc::pid_t| -> Option<libc::pid_t> { None };
        assert_fail_closed(
            eperm_means_every_member_is_gone_from(123, GroupListing::Members(Vec::new()), pgid_of)
                .expect_err("an empty listing must not prove the group gone"),
        );
    }

    #[test]
    fn eperm_with_a_full_listing_fails_closed() {
        let pgid_of = |_: libc::pid_t| -> Option<libc::pid_t> { None };
        assert_fail_closed(
            eperm_means_every_member_is_gone_from(123, GroupListing::Full, pgid_of)
                .expect_err("a full listing may be cut short and must not prove the group gone"),
        );
    }

    #[test]
    fn eperm_means_every_member_is_gone_once_no_listed_member_is_in_the_group() {
        let pgid_of = |_: libc::pid_t| -> Option<libc::pid_t> { None };
        let gone = eperm_means_every_member_is_gone_from(
            123,
            GroupListing::Members(vec![1, 2, 3]),
            pgid_of,
        )
        .expect("a readable listing is not a failure");
        assert!(
            gone,
            "no listed member is in the group, so every member is gone"
        );
    }

    #[test]
    fn eperm_does_not_mean_gone_while_a_listed_member_is_still_in_the_group() {
        let pgid_of =
            |pid: libc::pid_t| -> Option<libc::pid_t> { if pid == 2 { Some(123) } else { None } };
        let gone = eperm_means_every_member_is_gone_from(
            123,
            GroupListing::Members(vec![1, 2, 3]),
            pgid_of,
        )
        .expect("a readable listing is not a failure");
        assert!(!gone, "member 2 is still in the group, so it is not gone");
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
    fn listing_returns_every_member_the_call_reports_and_counts_in_entries() {
        // Five members in an eight-entry buffer: the buffer is handed over in
        // entries (pids), and only the reported count of them is the list.
        let list = |buf: &mut [libc::pid_t]| -> libc::c_int {
            assert_eq!(buf.len(), 8, "the buffer must be sized in entries");
            buf[..5].copy_from_slice(&[11, 22, 33, 44, 55]);
            5
        };
        let listing = list_group_members(8, list)
            .expect("a count below capacity with only positive pids must succeed");
        assert_eq!(listing, GroupListing::Members(vec![11, 22, 33, 44, 55]));
    }
}
