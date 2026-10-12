//! The job object that owns a child's whole process tree.

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::AssignProcessToJobObject;
use windows::Win32::System::JobObjects::CreateJobObjectW;
use windows::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
use windows::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
use windows::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
use windows::Win32::System::JobObjects::SetInformationJobObject;
use windows::Win32::System::JobObjects::TerminateJobObject;

use super::security_failure;
use crate::OsError;

/// A job object carrying `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: every process
/// in it ends when [`JobObject::terminate`] is called or when the value is
/// dropped, whichever comes first.
pub struct JobObject {
    handle: HANDLE,
    terminated: AtomicBool,
}

impl JobObject {
    pub(crate) fn new() -> Result<Self, OsError> {
        // SAFETY: CreateJobObjectW returns an owned handle or an error; the
        // handle is closed on the failure path below and otherwise owned by
        // the returned value.
        let handle = unsafe { CreateJobObjectW(None, None) }
            .map_err(|_| security_failure("failed to create job object"))?;

        // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE lives in BasicLimitInformation.LimitFlags,
        // but SetInformationJobObject rejects it (ERROR_INVALID_PARAMETER) unless it is
        // written through the *extended* information class.
        // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a valid value.
        let mut limit_info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        limit_info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        // SAFETY: `handle` is valid, and `limit_info` matches the information
        // class and the length passed alongside it.
        let set_result = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limit_info).cast(),
                mem::size_of_val(&limit_info) as u32,
            )
        };
        if set_result.is_err() {
            // SAFETY: `handle` is valid and owned here; nothing else refers to it.
            let _ = unsafe { CloseHandle(handle) };
            return Err(security_failure("failed to set job information"));
        }

        Ok(Self {
            handle,
            terminated: AtomicBool::new(false),
        })
    }

    /// Assigns the process `process` (a handle holding `PROCESS_SET_QUOTA` and
    /// `PROCESS_TERMINATE`) to this job.
    pub(crate) fn assign_process(&self, process: HANDLE) -> windows::core::Result<()> {
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { AssignProcessToJobObject(self.handle, process) }
    }

    /// Ends every process in the job. Idempotent: once a call has succeeded,
    /// later calls return `Ok(())` without doing anything.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Security`] when the operating system refuses to
    /// terminate the job; a later call tries again.
    pub fn terminate(&self) -> Result<(), OsError> {
        if self.terminated.load(Ordering::SeqCst) {
            return Ok(());
        }
        // SAFETY: `self.handle` is a valid job handle owned by this value.
        unsafe { TerminateJobObject(self.handle, 1) }
            .map_err(|_| security_failure("failed to terminate job object"))?;
        self.terminated.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is valid, obtained from CreateJobObjectW, and
        // closed only here; closing the last handle applies the kill-on-close
        // limit.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

impl std::fmt::Debug for JobObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobObject")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debug_form_names_the_type() {
        let job = JobObject::new().expect("a job object can be created");

        let text = format!("{job:?}");

        assert!(text.starts_with("JobObject"), "got {text:?}");
        assert!(text.contains("handle"), "got {text:?}");
    }
}
