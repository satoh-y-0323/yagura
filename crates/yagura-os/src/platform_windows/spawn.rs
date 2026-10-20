//! The one way this crate starts a process on Windows: with only the handles
//! it is given, and, except for the detached daemon, inside a job object from
//! before its first instruction.

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows::Win32::System::Console::HPCON;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_CREATION_FLAGS,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use super::child::ChildProcess;
use super::command_line::{self, ChildEnv, win32_error_to_io};
use super::job::JobObject;
use crate::capture::{CaptureEnv, CapturePipes, CapturedChild};

/// What the child's standard handles are connected to.
pub(crate) enum Io {
    /// Three inheritable handles, handed to the child as its standard input,
    /// output and error and to nothing else.
    Pipes(StdHandles),
    /// The pseudoconsole the child is attached to, with no other handle
    /// inherited. The caller keeps it open until the child has been resumed.
    PseudoConsole(HPCON),
}

/// The child's ends of its standard handles, each inheritable.
pub(crate) struct StdHandles {
    pub(crate) stdin: OwnedHandle,
    pub(crate) stdout: OwnedHandle,
    pub(crate) stderr: OwnedHandle,
}

/// What [`spawn_in_job`] starts.
pub(crate) struct SpawnSpec<'a> {
    /// `argv[0]` is resolved by [`command_line::resolve_application`].
    pub(crate) argv: &'a [String],
    /// `None` inherits the calling process's working directory.
    pub(crate) cwd: Option<&'a Path>,
    pub(crate) env: &'a ChildEnv,
    pub(crate) io: Io,
}

/// A child running inside the job that owns its process tree.
pub(crate) struct Spawned {
    pub(crate) child: ChildProcess,
    pub(crate) job: JobObject,
}

/// Starts `spec` suspended, assigns it to a fresh job object, then lets it
/// run, so every process it ever starts is born inside that job.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] / [`io::ErrorKind::NotFound`] from
/// resolving the program and building the command line and environment;
/// otherwise the operating system's error when the job cannot be created,
/// the process cannot be created, or it cannot be assigned to the job or
/// resumed. In the last two cases the suspended child is terminated before
/// this returns, so none of its code has run.
pub(crate) fn spawn_in_job(spec: SpawnSpec<'_>) -> io::Result<Spawned> {
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "argv must not be empty"))?;
    let block = spec.env.to_block()?;

    let job = JobObject::new().map_err(crate::capture::os_error_to_io)?;

    let info = create_process(Creation {
        program: OsStr::new(program),
        args,
        child_path: spec.env.get(OsStr::new("PATH")),
        env_block: Some(&block),
        cwd: spec.cwd,
        io: &spec.io,
        flags: CREATE_SUSPENDED,
    })?;
    // SAFETY: CreateProcessW succeeded, so both handles are fresh and owned
    // here alone.
    let suspended = unsafe { SuspendedChild::from_information(&info) };
    drop(spec.io);

    job.assign_process(suspended.process())
        .map_err(|error| win32_error_to_io(&error))?;
    let child = suspended.resume()?;
    Ok(Spawned { child, job })
}

/// Starts `program` with `args` outside any job, with the `NUL` device as its
/// three standard handles and no other handle inherited, in the caller's own
/// environment and working directory. Nothing waits on it.
///
/// # Errors
///
/// As for resolving the program and building the command line, plus the
/// operating system's error when `NUL` cannot be opened or the process
/// cannot be created with `flags`.
pub(super) fn spawn_detached(
    program: &OsStr,
    args: &[&OsStr],
    flags: PROCESS_CREATION_FLAGS,
) -> io::Result<()> {
    // One handle per slot: the handle-list attribute is not documented to
    // accept the same handle twice.
    let io = Io::Pipes(StdHandles {
        stdin: inheritable_null(false)?,
        stdout: inheritable_null(true)?,
        stderr: inheritable_null(true)?,
    });
    let path = std::env::var_os("PATH");
    let info = create_process(Creation {
        program,
        args,
        child_path: path.as_deref(),
        env_block: None,
        cwd: None,
        io: &io,
        flags,
    })?;
    // SAFETY: CreateProcessW succeeded, so both handles are fresh and owned
    // here alone; each is closed once as its owner drops.
    unsafe {
        drop(OwnedHandle::from_raw_handle(info.hProcess.0 as *mut c_void));
        drop(OwnedHandle::from_raw_handle(info.hThread.0 as *mut c_void));
    }
    Ok(())
}

/// What [`create_process`] starts.
struct Creation<'a, A> {
    /// Resolved by [`command_line::resolve_application`] against `child_path`.
    program: &'a OsStr,
    args: &'a [A],
    child_path: Option<&'a OsStr>,
    /// A `CREATE_UNICODE_ENVIRONMENT` block, or `None` to inherit the caller's.
    env_block: Option<&'a [u16]>,
    /// `None` inherits the calling process's working directory.
    cwd: Option<&'a Path>,
    io: &'a Io,
    /// Added to the flags `io` itself needs.
    flags: PROCESS_CREATION_FLAGS,
}

/// The one `CreateProcessW` of this crate: the child inherits exactly the
/// handles `io` names, through `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
// The OS boundary owns this `CreateProcessW`; `clippy.toml` bans it everywhere else.
#[allow(clippy::disallowed_methods)]
fn create_process<A: AsRef<OsStr>>(creation: Creation<'_, A>) -> io::Result<PROCESS_INFORMATION> {
    let application = command_line::resolve_application(creation.program, creation.child_path)?;
    let application_wide = command_line::to_wide_nul(application.as_os_str())?;
    let mut line = command_line::command_line(&application, creation.args)?;
    let cwd_wide = creation
        .cwd
        .map(|cwd| command_line::to_wide_nul(cwd.as_os_str()))
        .transpose()?;

    // A pseudoconsole child gets its standard handles from the console, so all
    // three are explicitly invalid and no handle is inherited.
    let (attributes, std_handles, inherit_handles, window_flag) = match creation.io {
        Io::Pipes(handles) => {
            let std_handles = [
                raw(&handles.stdin),
                raw(&handles.stdout),
                raw(&handles.stderr),
            ];
            (
                ProcThreadAttributes::handle_list(&std_handles)?,
                std_handles,
                true,
                CREATE_NO_WINDOW,
            )
        }
        Io::PseudoConsole(console) => (
            ProcThreadAttributes::pseudo_console(*console)?,
            [INVALID_HANDLE_VALUE; 3],
            false,
            PROCESS_CREATION_FLAGS(0),
        ),
    };

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = std_handles[0];
    startup.StartupInfo.hStdOutput = std_handles[1];
    startup.StartupInfo.hStdError = std_handles[2];
    startup.lpAttributeList = attributes.as_list();

    let env_flag = if creation.env_block.is_some() {
        CREATE_UNICODE_ENVIRONMENT
    } else {
        PROCESS_CREATION_FLAGS(0)
    };
    let flags = creation.flags | EXTENDED_STARTUPINFO_PRESENT | env_flag | window_flag;
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every pointer refers to a buffer that outlives the call: the
    // NUL-terminated application name, command line (writable, as
    // CreateProcessW requires) and working directory owned by this frame, the
    // caller's environment block, and `startup`, whose attribute list
    // `attributes` keeps alive together with the handle array it may point
    // at. Listed handles are inheritable and stay open until after the call
    // (`creation.io`); the pseudoconsole stays open because the caller
    // guarantees it.
    unsafe {
        CreateProcessW(
            PCWSTR(application_wide.as_ptr()),
            PWSTR(line.as_mut_ptr()),
            None,
            None,
            inherit_handles,
            flags,
            creation
                .env_block
                .map(|block| block.as_ptr().cast::<c_void>()),
            cwd_wide
                .as_ref()
                .map_or(PCWSTR::null(), |cwd| PCWSTR(cwd.as_ptr())),
            &startup.StartupInfo,
            &mut info,
        )
    }
    .map_err(|error| win32_error_to_io(&error))?;
    drop(attributes);
    Ok(info)
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle() as isize)
}

/// A process created suspended. Dropping it before [`SuspendedChild::resume`]
/// terminates the process, which has then run none of its own code.
struct SuspendedChild {
    process: Option<OwnedHandle>,
    thread: OwnedHandle,
    pid: u32,
}

impl SuspendedChild {
    /// # Safety
    ///
    /// `info` must come from a successful `CreateProcessW` whose handles are
    /// not owned anywhere else.
    unsafe fn from_information(info: &PROCESS_INFORMATION) -> Self {
        // SAFETY: per this function's contract, both handles are valid and
        // owned by nothing else, so each gets exactly one owner here.
        unsafe {
            Self {
                process: Some(OwnedHandle::from_raw_handle(info.hProcess.0 as *mut c_void)),
                thread: OwnedHandle::from_raw_handle(info.hThread.0 as *mut c_void),
                pid: info.dwProcessId,
            }
        }
    }

    fn process(&self) -> HANDLE {
        self.process.as_ref().map_or(HANDLE::default(), raw)
    }

    /// Lets the main thread run and hands the process over; the thread
    /// handle is closed here.
    fn resume(mut self) -> io::Result<ChildProcess> {
        // SAFETY: `self.thread` is the suspended main thread's valid handle.
        if unsafe { ResumeThread(raw(&self.thread)) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        let process = self.process.take().ok_or_else(|| {
            io::Error::other("the suspended child no longer holds its process handle")
        })?;
        Ok(ChildProcess::new(process, self.pid))
    }
}

impl Drop for SuspendedChild {
    fn drop(&mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        // SAFETY: `process` is a valid handle owned here; it is waited on only
        // when the termination was accepted, so the wait cannot block on a
        // process that keeps running.
        unsafe {
            if TerminateProcess(raw(&process), 1).is_ok() {
                let _ = WaitForSingleObject(raw(&process), INFINITE);
            }
        }
    }
}

/// An initialized attribute list holding one attribute.
struct ProcThreadAttributes {
    /// The list's storage, `u64`-aligned because the list holds pointers.
    buffer: Vec<u64>,
    /// The array a handle-list attribute points at; it must outlive every use
    /// of the list.
    _handles: Box<[HANDLE]>,
}

impl ProcThreadAttributes {
    /// `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`: `handles` are the only handles the
    /// child inherits.
    fn handle_list(handles: &[HANDLE]) -> io::Result<Self> {
        let attribute = Self::initialized(handles.into())?;
        // SAFETY: the value points at `_handles`, a heap array that lives as
        // long as `attribute`, and the size is that array's size in bytes.
        unsafe {
            attribute.update(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(attribute._handles.as_ptr().cast::<c_void>()),
                mem::size_of_val(&*attribute._handles),
            )
        }?;
        Ok(attribute)
    }

    /// `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`: the child is attached to
    /// `console`.
    fn pseudo_console(console: HPCON) -> io::Result<Self> {
        let attribute = Self::initialized(Box::default())?;
        // SAFETY: the attribute's value is the `HPCON` itself, not a pointer to
        // it, and its size is that of the handle type, as the API requires; the
        // caller keeps the pseudoconsole open until the child is resumed.
        unsafe {
            attribute.update(
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(console.0 as *const c_void),
                mem::size_of::<HPCON>(),
            )
        }?;
        Ok(attribute)
    }

    fn initialized(handles: Box<[HANDLE]>) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a null list with a size out-parameter only reports the size
        // needed; the expected "insufficient buffer" failure is ignored.
        let _ = unsafe {
            InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST::default(),
                1,
                0,
                &mut size,
            )
        };
        let mut buffer = vec![0u64; size.div_ceil(mem::size_of::<u64>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
        // SAFETY: `buffer` holds at least `size` bytes, as just reported.
        unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) }
            .map_err(|error| win32_error_to_io(&error))?;
        Ok(Self {
            buffer,
            _handles: handles,
        })
    }

    /// # Safety
    ///
    /// `value` must be what `attribute` expects and stay valid for `size`
    /// bytes until the list is dropped, as `UpdateProcThreadAttribute`
    /// requires.
    unsafe fn update(
        &self,
        attribute: usize,
        value: Option<*const c_void>,
        size: usize,
    ) -> io::Result<()> {
        // SAFETY: the list was initialized for one attribute in `initialized`;
        // the caller guarantees `value` and `size`.
        unsafe { UpdateProcThreadAttribute(self.as_list(), 0, attribute, value, size, None, None) }
            .map_err(|error| win32_error_to_io(&error))
    }

    fn as_list(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buffer.as_ptr().cast_mut().cast())
    }
}

impl Drop for ProcThreadAttributes {
    fn drop(&mut self) {
        // SAFETY: the list was initialized in `initialized` before this value
        // existed.
        unsafe { DeleteProcThreadAttributeList(self.as_list()) };
    }
}

/// An anonymous pipe as `(read end, write end)`, neither inheritable: no
/// security attributes are passed, so a process started meanwhile cannot take
/// either end by inheritance.
pub(super) fn anonymous_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: both out-parameters are live for the call; the handles it
    // returns are given exactly one owner each right after.
    unsafe {
        CreatePipe(&mut read, &mut write, None, 0).map_err(|error| win32_error_to_io(&error))?;
        Ok((
            OwnedHandle::from_raw_handle(read.0 as *mut c_void),
            OwnedHandle::from_raw_handle(write.0 as *mut c_void),
        ))
    }
}

/// An anonymous pipe as `(parent end, child end)`, with only the child end
/// inheritable. `child_writes` picks which end the child gets.
fn pipe_for_child(child_writes: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (read, write) = anonymous_pipe()?;
    let (parent, child) = if child_writes {
        (read, write)
    } else {
        (write, read)
    };
    make_inheritable(&child)?;
    Ok((parent, child))
}

fn make_inheritable(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: `handle` is valid for the call; only its inherit flag changes.
    unsafe { SetHandleInformation(raw(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
        .map_err(|error| win32_error_to_io(&error))
}

/// The `NUL` device, opened for writing or for reading, inheritable.
fn inheritable_null(write: bool) -> io::Result<OwnedHandle> {
    let file = std::fs::OpenOptions::new()
        .read(!write)
        .write(write)
        .open("NUL")?;
    let handle = OwnedHandle::from(file);
    make_inheritable(&handle)?;
    Ok(handle)
}

/// Windows branch of [`crate::capture::spawn_captured`]: the child runs with
/// no console window, inside a job object from before it starts
/// ([`spawn_in_job`]), and receives its three standard handles and no other
/// handle. Dropping that job object ends the whole tree.
///
/// # Errors
///
/// As for [`spawn_in_job`], plus the operating system's error when a pipe or
/// the `NUL` device cannot be opened.
pub(crate) fn spawn_captured(
    argv: &[String],
    cwd: Option<&Path>,
    env: CaptureEnv<'_>,
    stdin: Option<Vec<u8>>,
) -> io::Result<CapturedChild> {
    let (mut child_env, vars) = match env {
        CaptureEnv::Inherit(vars) => (ChildEnv::inherited(), vars),
        CaptureEnv::Replace(vars) => (ChildEnv::default(), vars),
    };
    for (name, value) in vars {
        child_env.set(name.clone(), value.clone());
    }

    let (stdout_parent, stdout_child) = pipe_for_child(true)?;
    let (stderr_parent, stderr_child) = pipe_for_child(true)?;
    let (stdin_parent, stdin_child) = if stdin.is_some() {
        let (parent, child) = pipe_for_child(false)?;
        (Some(parent), child)
    } else {
        (None, inheritable_null(false)?)
    };

    let spawned = spawn_in_job(SpawnSpec {
        argv,
        cwd,
        env: &child_env,
        io: Io::Pipes(StdHandles {
            stdin: stdin_child,
            stdout: stdout_child,
            stderr: stderr_child,
        }),
    })?;

    let pipes = CapturePipes {
        stdout: Some(File::from(stdout_parent)),
        stderr: Some(File::from(stderr_parent)),
        stdin: stdin_parent.map(File::from),
    };
    Ok(CapturedChild::new(spawned.child, spawned.job, pipes, stdin))
}
