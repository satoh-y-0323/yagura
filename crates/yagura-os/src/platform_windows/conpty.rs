//! A window's terminal on Windows: the bundled pseudoconsole, loaded from beside
//! the executable, and the child attached to it.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::System::Console::{COORD, HPCON};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{HRESULT, PCSTR, PCWSTR};

use super::child::ChildProcess;
use super::command_line::{self, ChildEnv, win32_error_to_io};
use super::spawn::{Io, SpawnSpec, anonymous_pipe, spawn_in_job};
use crate::bundled_conpty;
use crate::pty::{EnvOp, PtyCommand, PtySize, PtySpawn, PtySpawnError};

/// File name of the bundled pseudoconsole library.
const CONPTY_DLL: &str = "conpty.dll";

/// No flags: the pinned `conpty.dll` builds its host command line from the
/// inherit-cursor flag only (the daemon answers the cursor query itself), and
/// the resize and Win32-input behaviours are always on there.
const PSEUDOCONSOLE_FLAGS: u32 = 0;

/// An export as `GetProcAddress` reports it, before it is given its real type.
type RawEntry = unsafe extern "system" fn() -> isize;

type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

/// The three entry points of the bundled `conpty.dll`.
struct Api {
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

/// The bundled library's entry points, loaded once and kept for the life of
/// the process (the library is never unloaded).
fn api() -> Result<&'static Api, PtySpawnError> {
    static API: OnceLock<Api> = OnceLock::new();
    if let Some(api) = API.get() {
        return Ok(api);
    }
    let api = load_api().map_err(PtySpawnError::BundledConptyUnavailable)?;
    Ok(API.get_or_init(|| api))
}

/// Loads `conpty.dll` by its full path beside the running executable, searching
/// nowhere else for it or for what it needs but that directory and System32.
/// Never calls the same-named functions of the Windows built-in console.
fn load_api() -> Result<Api, String> {
    let dll = bundled_conpty::executable_dir()
        .map_err(|error| error.to_string())?
        .join(CONPTY_DLL);
    let mut wide: Vec<u16> = dll.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: `wide` is NUL-terminated and outlives the call; no file handle is
    // passed, as the API requires.
    let module = unsafe {
        LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            HANDLE::default(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }
    .map_err(|error| format!("{CONPTY_DLL} cannot be loaded: {error}"))?;
    // SAFETY: each name is the entry point the library exports with exactly the
    // signature of its function-pointer type here.
    unsafe {
        Ok(Api {
            create: std::mem::transmute::<RawEntry, CreatePseudoConsoleFn>(entry_point(
                module,
                "CreatePseudoConsole\0",
            )?),
            resize: std::mem::transmute::<RawEntry, ResizePseudoConsoleFn>(entry_point(
                module,
                "ResizePseudoConsole\0",
            )?),
            close: std::mem::transmute::<RawEntry, ClosePseudoConsoleFn>(entry_point(
                module,
                "ClosePseudoConsole\0",
            )?),
        })
    }
}

/// The address of the export `name` (NUL-terminated) of `module`.
///
/// # Safety
///
/// `module` must be a loaded module handle.
unsafe fn entry_point(module: HMODULE, name: &'static str) -> Result<RawEntry, String> {
    // SAFETY: `name` is a NUL-terminated static string; the caller guarantees
    // `module`.
    unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }.ok_or_else(|| {
        format!(
            "{CONPTY_DLL} does not export {}",
            name.trim_end_matches('\0')
        )
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn coord(size: PtySize) -> io::Result<COORD> {
    let cell = |count: u16| {
        i16::try_from(count)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "terminal size too large"))
    };
    Ok(COORD {
        X: cell(size.cols)?,
        Y: cell(size.rows)?,
    })
}

/// Resizes and closes a window's pseudoconsole.
///
/// Both take `&self` under one lock, so a resize never runs against a console
/// that is being closed. Closing is idempotent, and dropping closes a console
/// nobody closed.
pub struct PtyControl {
    console: Mutex<Option<HPCON>>,
    api: &'static Api,
}

impl PtyControl {
    /// Changes the terminal's size to `cols` x `rows`.
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal has been closed or the size is
    /// refused.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let size = coord(PtySize { cols, rows })?;
        let console = lock(&self.console);
        let Some(console) = *console else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the pseudoconsole is closed",
            ));
        };
        // SAFETY: `console` is open: it is only taken out of the slot, under
        // this lock, by `close`.
        unsafe { (self.api.resize)(console, size) }
            .ok()
            .map_err(|error| win32_error_to_io(&error))
    }

    /// Closes the pseudoconsole, which ends its host once the output has been
    /// read to the end or its reading end is dropped. Does nothing when it is
    /// already closed.
    ///
    /// Closing a console whose child still runs ends that child with a console
    /// exit code, so call it after the child has exited.
    pub fn close(&self) {
        let Some(console) = lock(&self.console).take() else {
            return;
        };
        // SAFETY: `console` was taken out of the slot, so this is the only close.
        unsafe { (self.api.close)(console) };
    }
}

impl Drop for PtyControl {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for PtyControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyControl")
            .field("open", &lock(&self.console).is_some())
            .finish_non_exhaustive()
    }
}

/// The started child, to wait on.
#[derive(Debug)]
pub struct PtyChild(ChildProcess);

impl PtyChild {
    /// The child's process id.
    #[must_use]
    pub fn process_id(&self) -> u32 {
        self.0.id()
    }

    /// Blocks until the child exits and returns its exit code.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when waiting or reading the exit
    /// code fails.
    pub fn wait(self) -> io::Result<i32> {
        self.0.wait().map(|code| code as i32)
    }
}

/// Kills the child. Clones act on the same process and work while another
/// thread waits for it.
#[derive(Debug, Clone)]
pub struct PtyKiller(ChildProcess);

impl PtyKiller {
    /// Terminates the child with exit code `1`. A child that has already
    /// exited is not an error.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the child still runs and
    /// cannot be terminated.
    pub fn kill(&self) -> io::Result<()> {
        self.0.kill()
    }
}

/// Starts `command` on a new pseudoconsole: attached to it and inside a job
/// object before it runs.
pub(crate) fn spawn(command: &PtyCommand, size: PtySize) -> Result<PtySpawn, PtySpawnError> {
    bundled_conpty::verify_beside_executable()
        .map_err(|error| PtySpawnError::BundledConptyUnavailable(error.to_string()))?;
    let api = api()?;
    let spawn_error = PtySpawnError::Spawn;

    let mut env = ChildEnv::window_base();
    for op in &command.env {
        match op {
            EnvOp::Set(name, value) => env.set(name.clone(), value.clone()),
            EnvOp::Remove(name) => env.remove(name),
        }
    }
    let cwd = command_line::window_cwd(command.cwd.as_deref(), &env);
    let size = coord(size).map_err(spawn_error)?;

    // Neither pipe is inheritable, so a child started elsewhere meanwhile
    // cannot hold a copy of an end.
    let (input_read, input_write) = anonymous_pipe().map_err(PtySpawnError::Spawn)?;
    let (output_read, output_write) = anonymous_pipe().map_err(PtySpawnError::Spawn)?;

    let mut console = HPCON::default();
    // SAFETY: both pipe ends are valid for the call and the out-parameter is
    // live; the library duplicates the ends it keeps.
    unsafe {
        (api.create)(
            size,
            HANDLE(input_read.as_raw_handle() as isize),
            HANDLE(output_write.as_raw_handle() as isize),
            PSEUDOCONSOLE_FLAGS,
            &mut console,
        )
    }
    .ok()
    .map_err(|error| PtySpawnError::Spawn(win32_error_to_io(&error)))?;
    let control = PtyControl {
        console: Mutex::new(Some(console)),
        api,
    };
    // The console holds what it needs of these two.
    drop(input_read);
    drop(output_write);

    // The console stays open until after the child is resumed, which a child
    // would not survive otherwise. On failure `control` closes it, with no
    // child left to affect.
    let spawned = spawn_in_job(SpawnSpec {
        argv: &command.argv,
        cwd: cwd.as_deref(),
        env: &env,
        io: Io::PseudoConsole(console),
    })
    .map_err(PtySpawnError::Spawn)?;

    Ok(PtySpawn {
        child: PtyChild(spawned.child.clone()),
        killer: PtyKiller(spawned.child),
        group: spawned.job,
        control,
        reader: Box::new(File::from(output_read)),
        writer: Box::new(File::from(input_write)),
    })
}

/// The value `name` has in a window child's base environment.
pub(crate) fn default_env_var(name: &OsStr) -> Option<OsString> {
    ChildEnv::window_base().get(name).map(OsStr::to_os_string)
}
