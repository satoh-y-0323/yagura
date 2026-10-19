//! Windows implementation of the OS abstraction boundary.
//!
//! Named pipes carry the IPC, a DACL with a single access-allowed ACE for the
//! current user protects them, and a job object owns each child's process
//! tree from before the child runs (`spawn`).

// This module *is* the transport: `IpcTransport` for Windows is implemented in
// terms of the raw named-pipe types that `clippy.toml` bans everywhere else.
#![allow(clippy::disallowed_types)]

mod child;
mod command_line;
pub(crate) mod conpty;
mod endpoint_lock;
mod job;
mod spawn;

pub use child::ChildProcess;
pub use job::JobObject;
pub(crate) use spawn::spawn_captured;

use std::ffi::OsStr;
use std::ffi::OsString;
use std::ffi::c_void;
use std::mem;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use tokio::net::windows::named_pipe::ClientOptions;
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::net::windows::named_pipe::ServerOptions;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Foundation::ERROR_PIPE_BUSY;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Foundation::HLOCAL;
use windows::Win32::Foundation::LocalFree;
use windows::Win32::Foundation::PSID;
use windows::Win32::Security::ACCESS_ALLOWED_ACE;
use windows::Win32::Security::ACE_HEADER;
use windows::Win32::Security::ACL;
use windows::Win32::Security::ACL_REVISION;
use windows::Win32::Security::AddAccessAllowedAce;
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::Authorization::GetSecurityInfo;
use windows::Win32::Security::Authorization::SE_KERNEL_OBJECT;
use windows::Win32::Security::DACL_SECURITY_INFORMATION;
use windows::Win32::Security::GetAce;
use windows::Win32::Security::GetLengthSid;
use windows::Win32::Security::GetTokenInformation;
use windows::Win32::Security::InitializeAcl;
use windows::Win32::Security::InitializeSecurityDescriptor;
use windows::Win32::Security::PSECURITY_DESCRIPTOR;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Security::SECURITY_DESCRIPTOR;
use windows::Win32::Security::SetSecurityDescriptorDacl;
use windows::Win32::Security::TOKEN_USER;
use windows::Win32::Security::TokenUser;
use windows::Win32::Storage::FileSystem::CreateFileW;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
use windows::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows::Win32::System::Console::CONSOLE_MODE;
use windows::Win32::System::Console::ENABLE_ECHO_INPUT;
use windows::Win32::System::Console::ENABLE_LINE_INPUT;
use windows::Win32::System::Console::ENABLE_PROCESSED_INPUT;
use windows::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_INPUT;
use windows::Win32::System::Console::GetConsoleMode;
use windows::Win32::System::Console::GetStdHandle;
use windows::Win32::System::Console::STD_INPUT_HANDLE;
use windows::Win32::System::Console::SetConsoleMode;
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
use windows::Win32::System::Threading::DETACHED_PROCESS;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::Threading::OpenProcess;
use windows::Win32::System::Threading::OpenProcessToken;
use windows::Win32::System::Threading::PROCESS_NAME_WIN32;
use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
use windows::Win32::System::Threading::QueryFullProcessImageNameW;
use windows::core::PCWSTR;
use windows::core::PWSTR;

use crate::DetachedSpawn;
use crate::IpcTransport;
use crate::OsError;
use crate::PeerAuth;

fn security_failure(detail: &'static str) -> OsError {
    OsError::Security(detail.to_owned())
}

/// Access mask granted to the current user on the daemon pipe.
///
/// The bits are declared here as plain `u32` (winnt.h values) because
/// `AddAccessAllowedAce` takes an untyped mask while the `windows` crate
/// spreads the equivalent constants over several feature-gated modules with
/// incompatible newtypes.
///
/// Specific pipe rights rather than the generic ones, which do not map
/// directly onto named pipes: data, attribute and extended-attribute access,
/// `SYNCHRONIZE`, `FILE_CREATE_PIPE_INSTANCE` for the extra instances the
/// accept loop needs, and `READ_CONTROL` so
/// [`PeerAuth::endpoint_allows_only`] can read the DACL back.
const PIPE_ACCESS_MASK: u32 = FILE_READ_DATA_MASK
    | FILE_WRITE_DATA_MASK
    | FILE_READ_ATTRIBUTES_MASK
    | FILE_WRITE_ATTRIBUTES_MASK
    | FILE_READ_EA_MASK
    | FILE_WRITE_EA_MASK
    | SYNCHRONIZE_MASK
    | FILE_CREATE_PIPE_INSTANCE_MASK
    | READ_CONTROL_MASK;

const FILE_READ_DATA_MASK: u32 = 0x0000_0001;
const FILE_WRITE_DATA_MASK: u32 = 0x0000_0002;
const FILE_READ_ATTRIBUTES_MASK: u32 = 0x0000_0080;
const FILE_WRITE_ATTRIBUTES_MASK: u32 = 0x0000_0100;
const FILE_READ_EA_MASK: u32 = 0x0000_0008;
const FILE_WRITE_EA_MASK: u32 = 0x0000_0010;
const SYNCHRONIZE_MASK: u32 = 0x0010_0000;
const READ_CONTROL_MASK: u32 = 0x0002_0000;
const FILE_CREATE_PIPE_INSTANCE_MASK: u32 = 0x0000_0004;

/// Wide characters in the longest path Win32 can name (`UNICODE_STRING`'s
/// 32767-character limit plus its terminator).
const MAX_EXTENDED_PATH_CHARS: usize = 32_768;

/// An owned DACL for a named pipe.
///
/// The struct owns the ACL buffer so the pointers handed to Win32 stay valid
/// for as long as the value lives; `SECURITY_ATTRIBUTES` is never returned by
/// value (that would dangle). The single access-allowed ACE embeds its own
/// copy of the SID, so no separate SID buffer has to be kept alive.
///
/// Not `Clone`: [`IpcTransport::bind`] consumes it, so every
/// endpoint is protected by a descriptor built for that endpoint.
#[derive(Debug)]
pub struct PipeSecurity {
    /// ACL bytes, over-aligned via `u64` (Win32 requires the ACL to be
    /// `DWORD`-aligned, which `Vec<u8>` does not guarantee).
    acl: Vec<u64>,
}

impl PipeSecurity {
    /// Builds a DACL that grants [`PIPE_ACCESS_MASK`] to the current user's
    /// SID and nobody else (exactly one ACE, no inherited entries).
    fn for_current_user() -> Result<Self, OsError> {
        // SAFETY: GetCurrentProcess returns a pseudo-handle that is always
        // valid for the calling process and must not be closed.
        let process = unsafe { GetCurrentProcess() };
        let token_user = token_user_buffer(process, "failed to get current process token")?;
        let sid = token_user_sid(&token_user);

        // SAFETY: `sid` points into `token_user`, which outlives this block.
        // The ACL buffer is sized from GetLengthSid exactly as documented for
        // one ACCESS_ALLOWED_ACE (the ACE struct already carries the first
        // DWORD of the SID in `SidStart`), and is passed to InitializeAcl
        // before any ACE is added.
        unsafe {
            let sid_len = GetLengthSid(sid) as usize;
            if sid_len == 0 {
                return Err(security_failure("failed to create DACL"));
            }
            let acl_len = mem::size_of::<ACL>() + mem::size_of::<ACCESS_ALLOWED_ACE>()
                - mem::size_of::<u32>()
                + sid_len;
            let mut acl = vec![0u64; acl_len.div_ceil(mem::size_of::<u64>())];
            let acl_ptr = acl.as_mut_ptr().cast::<ACL>();

            InitializeAcl(acl_ptr, acl_len as u32, ACL_REVISION)
                .map_err(|_| security_failure("failed to create DACL"))?;
            AddAccessAllowedAce(acl_ptr, ACL_REVISION, PIPE_ACCESS_MASK, sid)
                .map_err(|_| security_failure("failed to create DACL"))?;

            Ok(Self { acl })
        }
    }

    fn acl_ptr(&self) -> *const ACL {
        self.acl.as_ptr().cast()
    }
}

/// A bound pipe name together with the instance waiting for the next client,
/// and the endpoint lock that marks its daemon as alive.
pub struct PipeListener {
    endpoint: String,
    security: PipeSecurity,
    /// `None` when refilling the pending instance failed; `accept` retries the
    /// refill at its next call before waiting for a client.
    pending: Option<NamedPipeServer>,
    // Declared last so the pipe instance closes before the lock is released:
    // a free lock never leaves a still-listening pipe behind.
    _lock: endpoint_lock::EndpointLock,
}

impl std::fmt::Debug for PipeListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeListener")
            .field("endpoint", &self.endpoint)
            .field("has_pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

/// Creates a named pipe instance protected by `security`.
///
/// The DACL owned by `security` is applied at creation time through
/// `SECURITY_ATTRIBUTES`, and `reject_remote_clients(true)` is always set.
/// This is the only pipe-creation path behind the boundary, so the daemon can
/// never serve an unsecured pipe.
///
/// `first_instance` sets `FILE_FLAG_FIRST_PIPE_INSTANCE`: creation then fails
/// when another process already owns the name (name-squatting defence).
async fn create_pipe_instance(
    name: &str,
    first_instance: bool,
    security: &PipeSecurity,
) -> std::io::Result<NamedPipeServer> {
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor_ptr = PSECURITY_DESCRIPTOR(ptr::addr_of_mut!(descriptor).cast());

    // SAFETY: `descriptor` and `attributes` are stack locals that outlive the
    // pipe-creation call, and the kernel copies the security descriptor into
    // the pipe object during creation, so nothing escapes this function. The
    // DACL pointer stays valid because `security` owns its ACL buffer and is
    // borrowed for the whole call.
    unsafe {
        InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION)
            .map_err(std::io::Error::other)?;
        SetSecurityDescriptorDacl(descriptor_ptr, true, Some(security.acl_ptr()), false)
            .map_err(std::io::Error::other)?;

        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_ptr.0,
            bInheritHandle: false.into(),
        };

        ServerOptions::new()
            .first_pipe_instance(first_instance)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, ptr::addr_of_mut!(attributes).cast())
    }
}

/// The named-pipe transport and peer check of the Windows platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsIpc;

impl PeerAuth for WindowsIpc {
    type Security = PipeSecurity;
    type Connection = NamedPipeServer;

    fn endpoint_security(&self) -> Result<Self::Security, OsError> {
        PipeSecurity::for_current_user()
    }

    fn current_user_id(&self) -> Result<String, OsError> {
        // SAFETY: GetCurrentProcess returns a pseudo-handle that is always
        // valid for the calling process and must not be closed.
        let process = unsafe { GetCurrentProcess() };
        let buffer = token_user_buffer(process, "failed to get current process token")?;
        sid_to_string(token_user_sid(&buffer))
    }

    fn peer_id(&self, conn: &Self::Connection) -> Option<String> {
        process_user_sid(client_process_id(conn)?).ok()
    }

    fn peer_executable(&self, conn: &Self::Connection) -> Option<PathBuf> {
        process_image_path(client_process_id(conn)?)
    }

    fn endpoint_allows_only(&self, endpoint: &str, user_id: &str) -> Result<bool, OsError> {
        pipe_dacl_allows_only(endpoint, user_id)
    }
}

impl IpcTransport for WindowsIpc {
    type Listener = PipeListener;
    type ClientConnection = NamedPipeClient;

    async fn bind(
        &self,
        endpoint: &str,
        security: Self::Security,
    ) -> Result<Self::Listener, OsError> {
        let lock = endpoint_lock::lock_endpoint(endpoint)?;
        let pending = create_pipe_instance(endpoint, true, &security).await?;
        Ok(PipeListener {
            endpoint: endpoint.to_owned(),
            security,
            pending: Some(pending),
            _lock: lock,
        })
    }

    /// Failure handling of `listener.pending`: instance
    /// **creation** failure is fatal, instance **connect** failure is not (the
    /// endpoint retries with a fresh instance), and a **refill** failure right
    /// after accepting a client is not either (the accepted connection is
    /// still returned).
    ///
    /// No new client can connect while `pending` is `None` after a refill
    /// failure, but `IpcServer::run_with_listener` hands the accepted
    /// connection to its own task and calls this again, so that window is at
    /// most one `accept` call wide.
    async fn accept(&self, listener: &mut Self::Listener) -> Result<Self::Connection, OsError> {
        loop {
            // Refill whenever `pending` is missing. Creation failure is fatal:
            // there is nothing left to offer, and an endpoint that stays
            // permanently broken (e.g. name-squatted) must not be retried
            // forever.
            if listener.pending.is_none() {
                let refilled =
                    create_pipe_instance(&listener.endpoint, false, &listener.security).await?;
                listener.pending = Some(refilled);
            }

            // Borrowing (not taking) `pending` keeps `accept` cancel-safe:
            // a dropped future leaves `listener.pending` for the next call.
            let Some(pending) = listener.pending.as_ref() else {
                return Err(security_failure(
                    "the pipe listener has no pending instance after refilling it",
                ));
            };

            if let Err(err) = pending.connect().await {
                // A connect failure (a transient I/O error; a client that hung
                // up first counts as connected and is handed out closed) is not
                // fatal: the dead instance is dropped and the refill above recreates it,
                // which is where a permanently broken endpoint surfaces as
                // fatal instead of spinning this loop forever.
                tracing::warn!(error = %err, "named pipe connect failed; recreating instance");
                listener.pending = None;
                continue;
            }

            // Refill the slot for the next client. A refill failure leaves
            // `pending` as `None` for the next call to retry, but the
            // connection just accepted must still be returned -- discarding it
            // would look like an immediate disconnect to the client.
            let next = create_pipe_instance(&listener.endpoint, false, &listener.security)
                .await
                .ok();

            match mem::replace(&mut listener.pending, next) {
                Some(accepted) => return Ok(accepted),
                None => {
                    return Err(security_failure(
                        "the pipe listener has no pending instance after connecting",
                    ));
                }
            }
        }
    }

    /// A pipe whose every instance is taken (`ERROR_PIPE_BUSY`) has a server that has not yet
    /// offered the next one, as a daemon does until its accept loop runs. The client waits for
    /// that instance, as a Unix client waits in the listen backlog, up to [`CONNECT_BUSY_WAIT`].
    async fn connect(&self, endpoint: &str) -> Result<Self::ClientConnection, OsError> {
        let deadline = tokio::time::Instant::now() + CONNECT_BUSY_WAIT;
        loop {
            match ClientOptions::new().open(endpoint) {
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if left.is_zero() {
                        return Err(err.into());
                    }
                    // Another client may take the instance first; the next open decides.
                    let name = endpoint.to_owned();
                    let _ = tokio::task::spawn_blocking(move || wait_named_pipe(&name, left)).await;
                }
                opened => {
                    let client = opened?;
                    let server =
                        server_process_id(&client).and_then(|pid| process_user_sid(pid).ok());
                    crate::require_own_server(
                        endpoint,
                        server.as_deref(),
                        &self.current_user_id()?,
                    )?;
                    return Ok(client);
                }
            }
        }
    }
}

/// How long [`WindowsIpc::connect`] waits for an instance of a pipe whose instances are all
/// taken before it gives up with the busy error.
const CONNECT_BUSY_WAIT: Duration = Duration::from_secs(30);

/// Blocks until an instance of `pipe_name` is offered or `limit` passes.
fn wait_named_pipe(pipe_name: &str, limit: Duration) {
    let wide: Vec<u16> = pipe_name.encode_utf16().chain(Some(0)).collect();
    let millis = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX).max(1);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call.
    let _ = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), millis) };
}

/// Reports whether the DACL of the *existing* pipe `pipe_name` grants access
/// to `sid` and to nobody else (verification API).
///
/// The pipe is opened by name with `READ_CONTROL` and its DACL is read from
/// the kernel object with `GetSecurityInfo`, so the answer reflects the real
/// object security rather than any in-process bookkeeping. A NULL DACL (which
/// grants everyone access) and any DENY entry are both reported as `false`;
/// access masks are not inspected.
///
/// Note that opening the pipe consumes a listening instance: verify each pipe
/// once, on a name created for that purpose.
fn pipe_dacl_allows_only(pipe_name: &str, sid: &str) -> Result<bool, OsError> {
    let mut wide: Vec<u16> = pipe_name.encode_utf16().collect();
    wide.push(0);

    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
    // CreateFileW call. The returned handle is closed on every path, and the
    // security descriptor allocated by GetSecurityInfo is released with
    // LocalFree before returning. `dacl` points into that descriptor and is
    // only read while it is alive.
    unsafe {
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            READ_CONTROL_MASK,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .map_err(|_| security_failure("failed to open the named pipe for security inspection"))?;

        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let status = GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            Some(&mut descriptor),
        );
        let _ = CloseHandle(handle);

        if status.is_err() {
            return Err(security_failure("failed to read the named pipe DACL"));
        }

        let verdict = acl_grants_only_sid(dacl, sid);
        let _ = LocalFree(HLOCAL(descriptor.0));
        verdict
    }
}

/// Walks `acl` and reports whether it consists solely of access-allowed
/// entries for `sid` (and holds at least one such entry).
///
/// # Safety
///
/// `acl` must be null or point to a valid ACL that stays alive for the call.
unsafe fn acl_grants_only_sid(acl: *const ACL, sid: &str) -> Result<bool, OsError> {
    // A NULL DACL grants full access to everyone: never "limited to `sid`".
    if acl.is_null() {
        return Ok(false);
    }

    // SAFETY: `acl` is non-null and valid per this function's contract; each
    // ACE pointer comes from GetAce, which validates the index, and is read
    // only through the header/allowed-ACE layout Win32 guarantees for it.
    unsafe {
        let ace_count = u32::from((*acl).AceCount);
        // An empty DACL denies everyone, so it does not grant `sid` either.
        if ace_count == 0 {
            return Ok(false);
        }

        for index in 0..ace_count {
            let mut ace: *mut c_void = ptr::null_mut();
            GetAce(acl, index, &mut ace)
                .map_err(|_| security_failure("failed to read the named pipe DACL"))?;

            let header = ace.cast::<ACE_HEADER>();
            if u32::from((*header).AceType) != ACCESS_ALLOWED_ACE_TYPE {
                // DENY (and every other) entry: fail closed.
                return Ok(false);
            }

            let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
            let ace_sid = PSID(ptr::addr_of!((*allowed).SidStart) as *mut c_void);
            if sid_to_string(ace_sid)? != sid {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

/// Converts a SID to its string form (e.g. `S-1-5-21-...`).
fn sid_to_string(sid: PSID) -> Result<String, OsError> {
    if sid.is_invalid() {
        return Err(security_failure("failed to get token SID"));
    }

    // SAFETY: `sid` is a valid SID pointer (checked above). The string
    // ConvertSidToStringSidW allocates is copied into an owned `String` and
    // then released with LocalFree exactly once.
    unsafe {
        let mut raw = PWSTR::null();
        ConvertSidToStringSidW(sid, &mut raw)
            .map_err(|_| security_failure("failed to convert SID to string"))?;
        let text = raw
            .to_string()
            .map_err(|_| security_failure("failed to convert SID to string"));
        let _ = LocalFree(HLOCAL(raw.0.cast()));
        text
    }
}

/// Reads the `TOKEN_USER` information of `process` into a `u64`-aligned
/// buffer (the embedded SID is read through a pointer into it, and Win32 SID
/// routines require alignment `Vec<u8>` does not guarantee).
///
/// This is the single SID acquisition path of the workspace: both the current
/// process (`current_user_id`, `PipeSecurity`) and a connected peer
/// (`peer_id`) go through it, which is why it takes the process handle as an
/// argument instead of assuming the current one.
fn token_user_buffer(process: HANDLE, token_error: &'static str) -> Result<Vec<u64>, OsError> {
    // SAFETY: `process` is a valid process handle owned by the caller. The
    // token handle opened here is closed before returning on every path, and
    // the two GetTokenInformation calls follow the documented
    // probe-size-then-fill protocol with a buffer of the reported size.
    unsafe {
        let mut token_handle = HANDLE::default();
        OpenProcessToken(
            process,
            windows::Win32::Security::TOKEN_QUERY,
            &mut token_handle,
        )
        .map_err(|_| security_failure(token_error))?;

        let mut size = 0u32;
        let _ = GetTokenInformation(token_handle, TokenUser, None, 0, &mut size);

        let mut buffer = vec![0u64; (size as usize).div_ceil(mem::size_of::<u64>()).max(1)];
        let result = GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        );

        let _ = CloseHandle(token_handle);
        result.map_err(|_| security_failure("failed to get token SID"))?;

        Ok(buffer)
    }
}

/// Returns the SID embedded in a buffer filled by [`token_user_buffer`].
fn token_user_sid(buffer: &[u64]) -> PSID {
    // SAFETY: the buffer was filled by GetTokenInformation(TokenUser), so it
    // starts with a TOKEN_USER whose `User.Sid` points inside the same buffer.
    unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
}

/// Process id of the client at the other end of an accepted pipe connection.
fn client_process_id(conn: &NamedPipeServer) -> Option<u32> {
    let pipe_handle = HANDLE(conn.as_raw_handle() as isize);

    // SAFETY: GetNamedPipeClientProcessId only reads the client process ID
    // from a pipe handle owned by the caller.
    unsafe {
        let mut client_pid = 0u32;
        GetNamedPipeClientProcessId(pipe_handle, &mut client_pid).ok()?;
        Some(client_pid)
    }
}

/// Process id of the server at the other end of a client pipe connection.
fn server_process_id(conn: &NamedPipeClient) -> Option<u32> {
    let pipe_handle = HANDLE(conn.as_raw_handle() as isize);

    // SAFETY: GetNamedPipeServerProcessId only reads the server process ID
    // from a pipe handle owned by the caller.
    unsafe {
        let mut server_pid = 0u32;
        GetNamedPipeServerProcessId(pipe_handle, &mut server_pid).ok()?;
        Some(server_pid)
    }
}

/// Full image path of `process_id` in Win32 form, or `None` when the process
/// cannot be opened or queried.
///
/// `PROCESS_QUERY_LIMITED_INFORMATION` is the narrowest right
/// `QueryFullProcessImageNameW` accepts, so no handle able to read the peer's
/// memory or token is ever held here.
fn process_image_path(process_id: u32) -> Option<PathBuf> {
    // SAFETY: OpenProcess returns an owned handle for an existing process or
    // an error; it is closed on both paths below and never used afterwards.
    let process_handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()? };

    // Sized for the longest path the file system can name, so the query never
    // needs a second call with a grown buffer.
    let mut buffer = vec![0u16; MAX_EXTENDED_PATH_CHARS];
    let mut written = buffer.len() as u32;

    // SAFETY: `process_handle` is valid and owned by us, and `buffer` is a
    // live allocation of `written` wide characters, which is the in-out
    // protocol QueryFullProcessImageNameW documents (capacity in, length
    // written out).
    let result = unsafe {
        QueryFullProcessImageNameW(
            process_handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut written,
        )
    };

    // SAFETY: `process_handle` is valid and owned by us.
    unsafe {
        let _ = CloseHandle(process_handle);
    }

    result.ok()?;
    let name = buffer.get(..written as usize)?;
    Some(PathBuf::from(OsString::from_wide(name)))
}

/// SID of the user `process_id` runs as.
///
/// `PROCESS_QUERY_LIMITED_INFORMATION` is all `OpenProcessToken` needs, and
/// unlike `PROCESS_QUERY_INFORMATION` it is granted on a process of a higher
/// integrity level, so an elevated peer of the same user is still identified.
fn process_user_sid(process_id: u32) -> Result<String, OsError> {
    // SAFETY: OpenProcess returns an owned handle for an existing process or
    // an error; it is closed on both paths below and never used afterwards.
    let process_handle = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id)
            .map_err(|_| security_failure("failed to open process"))?
    };

    let buffer = token_user_buffer(process_handle, "failed to get client process token");

    // SAFETY: `process_handle` is valid and owned by us.
    unsafe {
        let _ = CloseHandle(process_handle);
    }

    sid_to_string(token_user_sid(&buffer?))
}

/// Creation flags for the preferred detached daemon start:
/// no inherited console and no membership in the caller's job object, so that
/// the daemon survives the caller being closed.
const DETACHED_SPAWN_FLAGS: u32 = DETACHED_PROCESS.0 | CREATE_BREAKAWAY_FROM_JOB.0;

/// Fallback creation flags used when the environment refuses
/// `CREATE_BREAKAWAY_FROM_JOB`.
const DETACHED_SPAWN_FLAGS_NO_BREAKAWAY: u32 = DETACHED_PROCESS.0;

/// Windows branch of [`crate::schedule_force_kill`]: a no-op.
///
/// There is no weaker first attempt to escalate from here. What a caller's own
/// kill sends on this platform is `TerminateProcess`, which the target cannot
/// catch, ignore or delay, so a follow-up signal after a grace period would
/// have nothing left to do. The parameters are kept so the boundary has one
/// signature on every platform and its callers need no `cfg` of their own.
pub fn schedule_force_kill(_pid: u32, _grace: Duration, _exited: Arc<AtomicBool>) {}

/// Windows branch of [`crate::enter_raw_console_input`].
///
/// Clears `ENABLE_LINE_INPUT`, `ENABLE_ECHO_INPUT` and
/// `ENABLE_PROCESSED_INPUT`, and sets `ENABLE_VIRTUAL_TERMINAL_INPUT`, on the
/// console attached to standard input, leaving every other mode bit as it
/// was. A failing `GetConsoleMode` is how a non-console standard input (a
/// pipe, a file) is recognised, and is reported as success: there is no line
/// discipline in front of those to turn off.
///
/// # Errors
///
/// Returns [`OsError::Security`] when the standard input handle cannot be
/// obtained or when the new mode is rejected.
pub fn enter_raw_console_input() -> Result<(), OsError> {
    // SAFETY: GetStdHandle returns a handle the process already owns, which is
    // borrowed rather than closed here; the two mode calls only read and write
    // that handle's flags, and `mode` is a stack local written by
    // GetConsoleMode before it is read.
    unsafe {
        let stdin = GetStdHandle(STD_INPUT_HANDLE)
            .map_err(|_| security_failure("failed to get the standard input handle"))?;
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(stdin, &mut mode).is_err() {
            return Ok(());
        }
        let raw = (mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        SetConsoleMode(stdin, raw)
            .map_err(|_| security_failure("failed to set the console input mode"))
    }
}

/// Windows branch of [`crate::program::default_is_executable`]: an existing
/// file is startable, since what decides *how* it starts is its extension,
/// which the resolution rule has already chosen. Follows symlinks, so a shim
/// installed as one is seen as the file it points at.
pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Windows branch of [`crate::spawn_daemon_detached`].
pub fn spawn_daemon_detached(path: &Path, args: &[&OsStr]) -> Result<DetachedSpawn, OsError> {
    if spawn_detached(path, args, DETACHED_SPAWN_FLAGS).is_ok() {
        return Ok(DetachedSpawn {
            used_fallback: false,
        });
    }
    spawn_detached(path, args, DETACHED_SPAWN_FLAGS_NO_BREAKAWAY)?;
    Ok(DetachedSpawn {
        used_fallback: true,
    })
}

// The OS boundary owns the one detached spawn outside the PTY path; the ban in
// `clippy.toml` keeps every other call site from creating a child of its own.
#[allow(clippy::disallowed_methods)]
fn spawn_detached(path: &Path, args: &[&OsStr], creation_flags: u32) -> std::io::Result<()> {
    // Argument vector, never a shell command line (CWE-78).
    Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(creation_flags)
        .spawn()
        .map(|_detached| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::{ACL_SIZE_INFORMATION, AclSizeInformation, GetAclInformation};

    // The buffer is sized for the one ACE that is added: a spare byte would mean
    // the sizing sum is wrong, and a short buffer would have failed the ACE add.
    #[test]
    fn the_pipe_acl_holds_one_entry_and_no_spare_bytes() {
        let security = PipeSecurity::for_current_user().expect("the DACL can be built");
        let mut info = ACL_SIZE_INFORMATION::default();

        // SAFETY: `acl_ptr` points into `security`'s live buffer and `info` is a live
        // out-parameter whose size is passed alongside it.
        unsafe {
            GetAclInformation(
                security.acl_ptr(),
                ptr::addr_of_mut!(info).cast(),
                mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        }
        .expect("the ACL size can be read");

        assert_eq!(info.AceCount, 1);
        assert_eq!(info.AclBytesFree, 0);
    }
}
