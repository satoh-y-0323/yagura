//! The lock that marks a pipe name as bound by a live listener.

use std::os::windows::ffi::OsStrExt;
use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Foundation::WAIT_ABANDONED;
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Foundation::WAIT_TIMEOUT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::System::Threading::ReleaseMutex;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::PCWSTR;

use super::security_failure;
use crate::OsError;

const PIPE_NAMESPACE: &str = r"\\.\pipe\";

/// Ownership of the named mutex `Local\yagura-endpoint-<pipe name>`, the
/// Windows counterpart of the Unix `<endpoint>.lock`.
///
/// A mutex belongs to the thread that took it, so a thread of its own holds it
/// until this value is dropped; when the process dies the system releases it as
/// abandoned.
pub(crate) struct EndpointLock {
    release: Option<mpsc::Sender<()>>,
    holder: Option<JoinHandle<()>>,
}

impl Drop for EndpointLock {
    fn drop(&mut self) {
        drop(self.release.take());
        // Joined so the mutex is released before the drop returns: the next
        // bind of the same name in this process must find it free.
        if let Some(holder) = self.holder.take() {
            let _ = holder.join();
        }
    }
}

/// Takes the lock of `endpoint`, a `\\.\pipe\` name.
///
/// # Errors
///
/// Returns [`OsError::EndpointHeld`] when a live process holds it,
/// [`OsError::Security`] when `endpoint` is not a plain pipe name, and
/// [`OsError::Io`] when the mutex cannot be opened (e.g. another user created
/// it first with a DACL that leaves this user out).
pub(crate) fn lock_endpoint(endpoint: &str) -> Result<EndpointLock, OsError> {
    let name = mutex_name(endpoint)?;
    let endpoint = endpoint.to_owned();
    let (taken_tx, taken_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::Builder::new()
        .name("endpoint-lock".to_owned())
        .spawn(move || hold(&name, &endpoint, &taken_tx, &release_rx))
        .map_err(OsError::Io)?;
    let taken = taken_rx
        .recv()
        .unwrap_or_else(|_| Err(security_failure("the endpoint lock thread ended early")));
    match taken {
        Ok(()) => Ok(EndpointLock {
            release: Some(release_tx),
            holder: Some(holder),
        }),
        Err(err) => {
            let _ = holder.join();
            Err(err)
        }
    }
}

fn mutex_name(endpoint: &str) -> Result<Vec<u16>, OsError> {
    let leaf = endpoint
        .strip_prefix(PIPE_NAMESPACE)
        .filter(|leaf| !leaf.is_empty() && !leaf.contains('\\'))
        .ok_or_else(|| OsError::Security(format!("{endpoint} is not a plain named-pipe name")))?;
    Ok(
        std::ffi::OsStr::new(&format!(r"Local\yagura-endpoint-{leaf}"))
            .encode_wide()
            .chain(Some(0))
            .collect(),
    )
}

/// Runs on the lock's own thread: takes the mutex, reports the outcome through
/// `taken`, and holds it until `release` is dropped.
fn hold(
    name: &[u16],
    endpoint: &str,
    taken: &mpsc::Sender<Result<(), OsError>>,
    release: &mpsc::Receiver<()>,
) {
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
    let handle = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
        Ok(handle) => handle,
        Err(err) => {
            let _ = taken.send(Err(OsError::Io(std::io::Error::other(err))));
            return;
        }
    };
    // SAFETY: `handle` is the mutex opened above, closed only at the end of
    // this function.
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
        let _ = taken.send(Ok(()));
        let _ = release.recv();
        // SAFETY: this thread owns the mutex, taken by the wait above.
        let _ = unsafe { ReleaseMutex(handle) };
    } else if wait == WAIT_TIMEOUT {
        let _ = taken.send(Err(OsError::EndpointHeld(format!(
            "{endpoint} is held by a live listener (its lock is taken)"
        ))));
    } else {
        let _ = taken.send(Err(OsError::Io(std::io::Error::last_os_error())));
    }
    // SAFETY: `handle` is still open and is not used after this call.
    let _ = unsafe { CloseHandle(handle) };
}
