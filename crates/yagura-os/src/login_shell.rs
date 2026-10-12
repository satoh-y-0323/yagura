//! The user's login shell as the password database records it.

use std::path::PathBuf;

/// The login shell the password database records for the current user.
///
/// Used when a daemon started outside a terminal has no usable `$SHELL`: the
/// password database is where a terminal's own login shell comes from.
/// `None` on Windows, when the lookup fails, and when the entry's shell field
/// is empty (which conventionally means `/bin/sh`, a fallback the caller
/// applies itself).
#[must_use]
pub fn passwd_login_shell() -> Option<PathBuf> {
    passwd_login_shell_impl()
}

/// The buffer size tried first when `sysconf` gives no hint.
#[cfg(unix)]
const DEFAULT_PASSWD_BUFFER_BYTES: usize = 4096;

/// The largest buffer the lookup grows to on `ERANGE`, so a misbehaving name
/// service cannot make it allocate without bound.
#[cfg(unix)]
const MAX_PASSWD_BUFFER_BYTES: usize = 1024 * 1024;

#[cfg(unix)]
fn passwd_login_shell_impl() -> Option<PathBuf> {
    // SAFETY: getuid cannot fail and has no side effects.
    passwd_login_shell_of(unsafe { libc::getuid() })
}

/// The shell field of `uid`'s password entry.
#[cfg(unix)]
fn passwd_login_shell_of(uid: libc::uid_t) -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: sysconf only reads a system limit.
    let hint = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let first_size = usize::try_from(hint)
        .ok()
        .filter(|&size| size > 0)
        .unwrap_or(DEFAULT_PASSWD_BUFFER_BYTES);
    with_growing_buffer(first_size, |buffer| {
        // SAFETY: `passwd` is a plain C struct of integers and pointers, for
        // which all-zero bytes (null pointers) are a valid value.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to a live local, and `buffer.len()` is
        // exactly the size of the buffer getpwuid_r may write strings into.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE {
            return Attempt::TooSmall;
        }
        // `entry` alone is not trusted: glibc's files backend parses every
        // line into it while searching, so a miss leaves the last line there.
        if status != 0 || found.is_null() || entry.pw_shell.is_null() {
            return Attempt::Done(None);
        }
        // SAFETY: on success `pw_shell` points to a NUL-terminated string
        // inside `buffer`, which is still alive here.
        let shell = unsafe { CStr::from_ptr(entry.pw_shell) }.to_bytes();
        Attempt::Done((!shell.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(shell))))
    })
}

/// One call of a lookup that writes its result into a caller-supplied buffer.
#[cfg(unix)]
enum Attempt<T> {
    Done(Option<T>),
    TooSmall,
}

/// Runs `attempt` on a buffer of `first_size` bytes, doubling it while the
/// buffer is too small, up to [`MAX_PASSWD_BUFFER_BYTES`].
#[cfg(unix)]
fn with_growing_buffer<T>(
    first_size: usize,
    mut attempt: impl FnMut(&mut [libc::c_char]) -> Attempt<T>,
) -> Option<T> {
    let mut size = first_size.min(MAX_PASSWD_BUFFER_BYTES);
    loop {
        let mut buffer: Vec<libc::c_char> = vec![0; size];
        match attempt(&mut buffer) {
            Attempt::Done(value) => return value,
            Attempt::TooSmall => {
                if size >= MAX_PASSWD_BUFFER_BYTES {
                    return None;
                }
                size = size.saturating_mul(2).min(MAX_PASSWD_BUFFER_BYTES);
            }
        }
    }
}

#[cfg(not(unix))]
fn passwd_login_shell_impl() -> Option<PathBuf> {
    None
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Far above every system range, so no password database lists it.
    const UNLISTED_UID: libc::uid_t = 1_999_999_999;

    #[test]
    fn a_uid_the_password_database_does_not_list_has_no_login_shell() {
        assert!(
            passwd_login_shell_of(0).is_some(),
            "control: root is listed with a shell"
        );
        assert_eq!(passwd_login_shell_of(UNLISTED_UID), None);
    }

    // An entry larger than the system's hint (NSS / LDAP) needs the buffer to
    // grow well past it.
    #[test]
    fn the_buffer_grows_until_a_large_entry_fits() {
        let needed = 300 * 1024;
        let found = with_growing_buffer(1024, |buffer| {
            if buffer.len() < needed {
                Attempt::TooSmall
            } else {
                Attempt::Done(Some(buffer.len()))
            }
        });

        assert_eq!(found, Some(512 * 1024));
    }

    #[test]
    fn the_buffer_stops_growing_at_one_mebibyte() {
        let mut tried = Vec::new();
        let found: Option<()> = with_growing_buffer(1024, |buffer| {
            tried.push(buffer.len());
            Attempt::TooSmall
        });

        assert_eq!(found, None);
        assert_eq!(tried.last(), Some(&(1024 * 1024)));
        assert_eq!(tried.len(), 11, "1 KiB doubled up to 1 MiB: {tried:?}");
    }
}
