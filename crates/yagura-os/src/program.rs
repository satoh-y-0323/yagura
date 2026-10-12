//! Resolution of a window's `argv[0]` to something the operating system can
//! actually start.
//!
//! The rules are selected by the [`Platform`] *value* a caller passes rather
//! than by a `#[cfg]` of this module, so both rule sets compile and run on
//! every host. Only two things here depend on the host: which
//! variables [`ProgramLookup::from_vars`] reads from the environment it is
//! given, and the body of [`default_is_executable`].
//!
//! Resolution never rewrites the caller's `argv`: it reports what it found,
//! and [`into_spawn_argv`] builds the spawn argv from that. A program that is
//! not found is reported as [`Resolution::NotFound`], which leaves the argv
//! untouched and lets the spawn fail on its own.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Characters cmd.exe reinterprets while parsing the command line it is
/// given: a `.cmd` / `.bat` shim can only be started through
/// cmd.exe, so an argument containing one of these would change what runs.
const CMD_METACHARACTERS: [char; 8] = ['%', '&', '|', '<', '>', '^', '"', '\n'];

/// Extensions appended to an extensionless `argv[0]` on Windows, in probe
/// order.
const WINDOWS_EXTENSIONS: [&str; 3] = ["exe", "cmd", "bat"];

/// Extensions `CreateProcess` cannot start by itself: these are scripts read
/// by the command interpreter, not executable images.
const WINDOWS_SHIM_EXTENSIONS: [&str; 2] = ["cmd", "bat"];

/// System directory holding `cmd.exe`, relative to `%SystemRoot%`.
const WINDOWS_SYSTEM_DIR: &str = "System32";

/// File name of the Windows command interpreter.
const WINDOWS_COMSPEC_FILE: &str = "cmd.exe";

/// npm's global shim directory, relative to `%APPDATA%`.
const WINDOWS_NPM_BIN_DIR: &str = "npm";

/// Flag that suppresses cmd.exe's AutoRun registry hook.
const CMD_NO_AUTORUN: &str = "/d";

/// Flag that makes cmd.exe run the following command and exit.
const CMD_RUN_AND_EXIT: &str = "/c";

/// Bare word inserted right before a `.cmd` / `.bat` shim to keep cmd.exe's
/// `/C` preprocessing from mangling the shim path's own quoting (see
/// [`needs_call_prefix`]).
const CMD_CALL: &str = "call";

/// Which set of resolution rules [`resolve_program`] applies.
///
/// Passed as a value so a single test binary can exercise both rule sets on
/// any host operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Extension probing, the extra bin directories and the cmd.exe shim
    /// wrapping apply.
    Windows,
    /// `PATH` is searched for the exact name, and the argv is never rewritten.
    Unix,
}

impl Platform {
    /// The operating system this binary is running on, collapsing every
    /// non-Windows target onto [`Platform::Unix`].
    ///
    /// Delegates to [`HostOs::host`], the single place in all of yagura that
    /// reads `cfg!(windows)` / `cfg!(target_os = ...)` to decide the host, so
    /// the two can never disagree. A caller outside `yagura-os` (the GUI's
    /// own daemon resolution) never needs an OS-specific `cfg`
    /// of its own.
    #[must_use]
    pub fn host() -> Self {
        HostOs::host().platform()
    }
}

/// The operating system a caller distinguishes when [`Platform`]'s two-way
/// split (Windows vs. everything else) is too coarse: a launch profile's
/// argv selection (the daemon's `ProfileOs`, and the GUI's own
/// default-profile placement) tells macOS and Linux apart, since they are
/// different ecosystems for a CLI's install location and invocation, even
/// though both resolve executables under [`Platform::Unix`]'s rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    /// Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Any other Unix (Linux, and everything `cfg(unix)` covers besides
    /// macOS).
    Linux,
}

impl HostOs {
    /// The operating system this binary is running on.
    ///
    /// The single place in all of yagura that reads `cfg!(windows)` /
    /// `cfg!(target_os = ...)` to decide the host operating system: every
    /// other caller that needs the host, including [`Platform::host`],
    /// derives the answer from this function instead of reading a `cfg!` of
    /// its own.
    #[must_use]
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }

    /// Maps onto the coarser [`Platform`] distinction executable resolution
    /// draws: Windows stays Windows, while both `MacOs` and `Linux` collapse
    /// onto [`Platform::Unix`].
    #[must_use]
    pub fn platform(self) -> Platform {
        match self {
            Self::Windows => Platform::Windows,
            Self::MacOs | Self::Linux => Platform::Unix,
        }
    }
}

/// Selects the value that belongs to `os` among four candidates keyed by
/// operating system -- the pattern a launch profile's `[command]` table (and
/// any future per-OS table) uses: `os`'s own key wins when present,
/// `default` is used when it is not, and the result is `None` when neither
/// is present. Never returns a key belonging to a different operating system
/// than `os`.
#[must_use]
pub fn select_for_os<'a, T: ?Sized>(
    os: HostOs,
    windows: Option<&'a T>,
    macos: Option<&'a T>,
    linux: Option<&'a T>,
    default: Option<&'a T>,
) -> Option<&'a T> {
    let own = match os {
        HostOs::Windows => windows,
        HostOs::MacOs => macos,
        HostOs::Linux => linux,
    };
    own.or(default)
}

/// The directories and interpreter a program name is resolved against.
///
/// Every field is data, so a caller (a test, or the daemon serving a request
/// against an injected directory) can resolve against an environment other
/// than the current process's.
#[derive(Debug, Clone)]
pub struct ProgramLookup {
    /// Directories from `PATH`, searched in order.
    pub path_dirs: Vec<PathBuf>,
    /// Directories searched after `path_dirs`, for shims that are commonly
    /// installed outside `PATH`. Windows only: the Unix rules ignore this.
    pub extra_bin_dirs: Vec<PathBuf>,
    /// Interpreter used to start a `.cmd` / `.bat` shim. `None` means no
    /// interpreter is available, which makes such a shim unresolvable.
    /// Windows only.
    pub windows_comspec: Option<PathBuf>,
}

impl ProgramLookup {
    /// Reads the current process environment: [`ProgramLookup::from_vars`]
    /// fed `std::env::var_os`, and the only function in this module that
    /// reads it.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_vars(std::env::var_os)
    }

    /// Builds the lookup from the environment `read` reports, a variable
    /// being absent when `read` returns `None`: the single place the rules
    /// below live, so a caller holding an environment other than the
    /// process's own resolves exactly as [`ProgramLookup::from_env`] would.
    ///
    /// Windows: `PATH`, `%APPDATA%\npm` as an extra bin directory, and
    /// `%SystemRoot%\System32\cmd.exe` as the interpreter. `COMSPEC` is
    /// deliberately not consulted, since it is a user-writable pointer to an
    /// arbitrary program; the interpreter's existence is verified
    /// here, so an absent cmd.exe becomes `None` rather than a spawn failure
    /// later.
    ///
    /// Unix: `PATH` only. The two Windows-only fields stay empty / `None`.
    ///
    /// The names asked for are fixed literals, so `read` only has to accept
    /// `&'static str`; that also admits a function item such as
    /// `std::env::var_os::<&str>`, which is not general over lifetimes.
    #[must_use]
    pub fn from_vars(read: impl Fn(&'static str) -> Option<OsString>) -> Self {
        let path_dirs = read("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        #[cfg(windows)]
        {
            Self {
                path_dirs,
                extra_bin_dirs: windows_extra_bin_dirs_from(read("APPDATA").as_deref()),
                windows_comspec: windows_comspec_from(read("SystemRoot").as_deref())
                    .filter(|comspec| comspec.is_file()),
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                path_dirs,
                extra_bin_dirs: Vec::new(),
                windows_comspec: None,
            }
        }
    }
}

/// Maps a `%SystemRoot%` value to the command interpreter under it:
/// `{system_root}\System32\cmd.exe`. `None` when the value is absent.
///
/// Pure: the path is not checked against the filesystem here, so this stays
/// testable on every host. [`ProgramLookup::from_vars`] does the existence
/// check.
#[must_use]
pub fn windows_comspec_from(system_root: Option<&OsStr>) -> Option<PathBuf> {
    system_root.map(|system_root| {
        PathBuf::from(system_root)
            .join(WINDOWS_SYSTEM_DIR)
            .join(WINDOWS_COMSPEC_FILE)
    })
}

/// Maps an `%APPDATA%` value to the extra bin directories searched after
/// `PATH`: npm installs its global shims into `%APPDATA%\npm`, which its
/// installer does not always add to `PATH`. Empty when the value is absent.
///
/// Pure, for the same reason as [`windows_comspec_from`].
#[must_use]
pub fn windows_extra_bin_dirs_from(app_data: Option<&OsStr>) -> Vec<PathBuf> {
    app_data
        .map(|app_data| vec![PathBuf::from(app_data).join(WINDOWS_NPM_BIN_DIR)])
        .unwrap_or_default()
}

/// What [`resolve_program`] found for `argv[0]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// An executable image that can be started as it is.
    Direct(PathBuf),
    /// A script shim that only the command interpreter can start.
    CmdShim {
        /// The interpreter to start.
        comspec: PathBuf,
        /// The shim path (as found by the search, or as written for a
        /// path-form `argv[0]`), handed to the interpreter as the command to
        /// run.
        shim: PathBuf,
    },
    /// Nothing matched, so the argv is left exactly as the caller wrote it
    /// and the spawn is allowed to fail on its own.
    NotFound,
}

/// Refusal to build a command interpreter command line, because the
/// interpreter would reinterpret it into something other than what the
/// caller asked for.
///
/// The message names the kind of input that was refused and deliberately
/// contains no path, so a caller may surface it to a user as-is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Rejected(pub String);

/// Resolves `argv[0]` against `lookup` under `platform`'s rules.
///
/// `argv` itself is never modified; use [`into_spawn_argv`] to build the argv
/// to spawn from the returned resolution.
///
/// `is_executable` decides whether a candidate path can be started. It is a
/// parameter rather than a direct filesystem call so that a caller can
/// resolve against an environment it describes itself;
/// [`default_is_executable`] is the filesystem-backed implementation.
///
/// A `argv[0]` containing a path separator (`/` on both platforms, plus `\`
/// on [`Platform::Windows`]) is already a path rather than a bare name, so it
/// is never searched for. Under [`Platform::Windows`], such a path that ends
/// in `.cmd` / `.bat` (any case) and that `is_executable` accepts is still
/// wrapped as a [`Resolution::CmdShim`] with the same checks as a bare name,
/// because `CreateProcess` would otherwise run it through cmd.exe unchecked.
/// Every other path-form `argv[0]` is reported as [`Resolution::NotFound`].
/// [`is_available`] still judges a path-form `argv[0]` directly, independent
/// of this function.
///
/// Otherwise, Windows: `path_dirs` then `extra_bin_dirs` are searched in
/// order. Within each directory, a name that already carries an extension is
/// tried exactly as written and nothing else, while an extensionless name is
/// tried as `.exe`, then `.cmd`, then `.bat` -- a same-name extensionless
/// file never matches, because `CreateProcess` cannot start one.
/// A weaker match in an earlier directory wins over a stronger one in a later
/// directory, which is the order the operating system itself would search.
///
/// Unix: `extra_bin_dirs` and the extension list do not apply; `path_dirs` is
/// searched for the exact name.
///
/// # Errors
///
/// Returns [`Rejected`] when a `.cmd` / `.bat` shim would have to be started
/// through the command interpreter, but the shim path, one of the trailing
/// arguments, or the interpreter's own resolved path (`comspec`) contains a
/// character the interpreter reinterprets. Rejecting here, before
/// the interpreter ever sees the argument, is what keeps the shim wrapper
/// (`Command::new("cmd")`) from re-opening the command-injection risk
/// argv-based process spawning is meant to close. Whitespace is not such a
/// character and is accepted. This can only happen under
/// [`Platform::Windows`]: nothing on the Unix path goes through a shell, so
/// nothing there is rejected.
pub fn resolve_program(
    argv: &[String],
    platform: Platform,
    lookup: &ProgramLookup,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Result<Resolution, Rejected> {
    let Some(program) = argv.first() else {
        return Ok(Resolution::NotFound);
    };
    if has_path_separator(program, platform) {
        return match platform {
            Platform::Windows => {
                resolve_windows_path_form(program, &argv[1..], lookup, is_executable)
            }
            Platform::Unix => Ok(Resolution::NotFound),
        };
    }
    match platform {
        Platform::Windows => resolve_windows(program, &argv[1..], lookup, is_executable),
        Platform::Unix => Ok(resolve_unix(program, lookup, is_executable)),
    }
}

/// Reports whether `argv` names a program that can be started.
///
/// For a bare name, this defers to [`resolve_program`]: both the listing
/// path and the spawn path ask this, so what a listing reports and what a
/// spawn attempts can never disagree. A rejection counts as unavailable,
/// exactly like a program that was not found.
///
/// For an `argv[0]` containing a path separator, [`resolve_program`] never
/// searches (see its docs), so availability instead asks `is_executable`
/// about that literal path directly. This keeps an
/// absolute-path profile (`C:\...\codex.exe`, `/usr/local/bin/claude`) from
/// reading as unavailable, and leaves a relative path (`./codex`) unresolved
/// against the daemon's cwd -- it is judged exactly as written. A path-form
/// shim that [`resolve_program`] rejects counts as unavailable, so the
/// listing never offers a launch the spawn path would refuse.
#[must_use]
pub fn is_available(
    argv: &[String],
    platform: Platform,
    lookup: &ProgramLookup,
    is_executable: &dyn Fn(&Path) -> bool,
) -> bool {
    if let Some(program) = argv.first()
        && has_path_separator(program, platform)
    {
        return is_executable(Path::new(program))
            && resolve_program(argv, platform, lookup, is_executable).is_ok();
    }
    matches!(
        resolve_program(argv, platform, lookup, is_executable),
        Ok(Resolution::Direct(_) | Resolution::CmdShim { .. })
    )
}

/// Whether `program` contains a path separator under `platform`'s rules:
/// `/` on both platforms, plus `\` on [`Platform::Windows`]. Shared by
/// [`resolve_program`] and [`is_available`] so the two never disagree on
/// what counts as "already a path".
fn has_path_separator(program: &str, platform: Platform) -> bool {
    match platform {
        Platform::Windows => program.contains(['/', '\\']),
        Platform::Unix => program.contains('/'),
    }
}

/// Builds the argv to spawn from a resolution and the caller's `argv`.
///
/// [`Platform::Unix`] returns `argv` unchanged whatever the resolution says:
/// resolution exists there only to answer availability. Under
/// [`Platform::Windows`], [`Resolution::Direct`] replaces `argv[0]` with the
/// resolved path, [`Resolution::CmdShim`] prepends the interpreter and its
/// flags (inserting a bare `call` right before the shim when needed -- see
/// [`needs_call_prefix`]), and [`Resolution::NotFound`] passes the argv
/// through untouched.
#[must_use]
pub fn into_spawn_argv(
    resolution: &Resolution,
    argv: &[String],
    platform: Platform,
) -> Vec<String> {
    if platform == Platform::Unix {
        return argv.to_vec();
    }
    let trailing: Vec<String> = argv.iter().skip(1).cloned().collect();
    match resolution {
        Resolution::Direct(path) => std::iter::once(path_argument(path))
            .chain(trailing)
            .collect(),
        Resolution::CmdShim { comspec, shim } => {
            let shim_argument = path_argument(shim);
            let mut spawn_argv = vec![
                path_argument(comspec),
                CMD_NO_AUTORUN.to_string(),
                CMD_RUN_AND_EXIT.to_string(),
            ];
            if needs_call_prefix(&shim_argument, &trailing) {
                spawn_argv.push(CMD_CALL.to_string());
            }
            spawn_argv.push(shim_argument);
            spawn_argv.extend(trailing);
            spawn_argv
        }
        Resolution::NotFound => argv.to_vec(),
    }
}

/// Whether a bare `call` must be inserted right before a `.cmd` / `.bat`
/// shim: exactly when `shim` and at least one of `trailing`'s arguments both
/// need Windows argv quoting (see [`windows_arg_needs_quoting`]).
///
/// `cmd /d /c <shim> <args...>` only preserves a quoted shim path's own
/// quotes when the text after `/c` holds at most one quoted segment: real
/// `cmd.exe` decides this before any of its normal, per-token parsing runs,
/// and its fallback for "more than one quoted segment" strips only the very
/// first and the very last quote character in the whole remainder --
/// mangling the shim path's own quoting rather than leaving it alone,
/// exactly when a trailing argument also needs its own quotes. `call` is a
/// bare word that never itself needs quoting, so prefixing it keeps the
/// remainder from starting with a quote character at all, which sidesteps
/// that special-casing entirely: `cmd.exe` falls back to its ordinary,
/// per-token, quote-respecting parser. Verified by direct experiment against
/// `cmd.exe` (this crate carries no test of `cmd.exe`'s own behavior; the
/// daemon's `tests/ipc_profile.rs` real-spawn cases are the check).
///
/// A shim with no argument needing its own quoting is left untouched.
fn needs_call_prefix(shim_argument: &str, trailing: &[String]) -> bool {
    windows_arg_needs_quoting(shim_argument)
        && trailing.iter().any(|arg| windows_arg_needs_quoting(arg))
}

/// Whether `arg` needs Windows argv quoting: empty, or containing a space,
/// tab, newline or quote character. Mirrors the quoting decision of the spawn
/// layer's own command-line builder (which cannot pass a pre-quoted string
/// through unescaped), so this predicate agrees with what will actually be
/// quoted.
fn windows_arg_needs_quoting(arg: &str) -> bool {
    arg.is_empty()
        || arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"'))
}

/// The filesystem-backed [`resolve_program`] predicate.
///
/// Windows: an existing file qualifies, because what makes it startable is
/// its extension, which the resolution rule has already chosen. Unix: the
/// file must also carry an execute bit.
#[must_use]
pub fn default_is_executable(path: &Path) -> bool {
    #[cfg(windows)]
    {
        crate::platform_windows::is_executable(path)
    }
    #[cfg(not(windows))]
    {
        crate::platform_unix::is_executable(path)
    }
}

fn resolve_windows(
    program: &str,
    trailing: &[String],
    lookup: &ProgramLookup,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Result<Resolution, Rejected> {
    // `resolve_program` routes path-separator names elsewhere, so `program`
    // here is always a bare name.
    for dir in lookup.path_dirs.iter().chain(&lookup.extra_bin_dirs) {
        let Some(found) = windows_match_in(dir, program, is_executable) else {
            continue;
        };
        if !needs_cmd_shim(&found) {
            return Ok(Resolution::Direct(found));
        }
        return wrap_cmd_shim(found, trailing, lookup);
    }
    Ok(Resolution::NotFound)
}

/// A path-form `argv[0]` is taken as written: only a `.cmd` / `.bat` is
/// wrapped, since any other path is started by `CreateProcess` as it is.
fn resolve_windows_path_form(
    program: &str,
    trailing: &[String],
    lookup: &ProgramLookup,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Result<Resolution, Rejected> {
    let shim = PathBuf::from(program);
    if !needs_cmd_shim(&shim) || !is_executable(&shim) {
        return Ok(Resolution::NotFound);
    }
    wrap_cmd_shim(shim, trailing, lookup)
}

fn wrap_cmd_shim(
    shim: PathBuf,
    trailing: &[String],
    lookup: &ProgramLookup,
) -> Result<Resolution, Rejected> {
    // An absent interpreter leaves the shim unstartable rather than
    // unacceptable, so it is reported as unresolved: rejection is
    // reserved for input this boundary refuses to pass on.
    let Some(comspec) = lookup.windows_comspec.clone() else {
        return Ok(Resolution::NotFound);
    };
    reject_cmd_metacharacters(&comspec, &shim, trailing)?;
    Ok(Resolution::CmdShim { comspec, shim })
}

/// A name that already carries an extension names one candidate; an
/// extensionless name is only ever probed with [`WINDOWS_EXTENSIONS`], since
/// an extensionless file is not an executable image `CreateProcess` can start
/// and npm installs a POSIX shell shim under exactly that name next to the
/// `.cmd` shim it can start.
fn windows_match_in(
    dir: &Path,
    program: &str,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if Path::new(program).extension().is_some() {
        let literal = dir.join(program);
        return is_executable(&literal).then_some(literal);
    }
    WINDOWS_EXTENSIONS.iter().find_map(|extension| {
        let candidate = dir.join(format!("{program}.{extension}"));
        is_executable(&candidate).then_some(candidate)
    })
}

fn needs_cmd_shim(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            WINDOWS_SHIM_EXTENSIONS
                .iter()
                .any(|shim| extension.eq_ignore_ascii_case(shim))
        })
}

/// Reports whether `s` contains one of the [`CMD_METACHARACTERS`] cmd.exe
/// reinterprets while parsing a command line: the one check every
/// caller that must guard an argument or a resolved path against cmd.exe's
/// own reinterpretation consults, instead of each keeping its own copy of the
/// reserved set.
#[must_use]
pub fn contains_cmd_metacharacter(s: &str) -> bool {
    s.contains(CMD_METACHARACTERS)
}

fn reject_cmd_metacharacters(
    comspec: &Path,
    shim: &Path,
    trailing: &[String],
) -> Result<(), Rejected> {
    let checked_paths = [
        (
            comspec,
            "the command interpreter's own resolved path contains a character it would \
             reinterpret",
        ),
        (
            shim,
            "the resolved script shim's path contains a character the command interpreter \
             reinterprets",
        ),
    ];
    for (path, message) in checked_paths {
        if contains_cmd_metacharacter(&path.to_string_lossy()) {
            return Err(Rejected(message.to_string()));
        }
    }
    if trailing
        .iter()
        .any(|argument| contains_cmd_metacharacter(argument))
    {
        return Err(Rejected(
            "an argument contains a character the command interpreter reinterprets".to_string(),
        ));
    }
    Ok(())
}

fn resolve_unix(
    program: &str,
    lookup: &ProgramLookup,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Resolution {
    // `resolve_program` has already sent path-separator names to
    // `Resolution::NotFound`, so `program` here is always a bare name.
    lookup
        .path_dirs
        .iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
        .map_or(Resolution::NotFound, Resolution::Direct)
}

fn path_argument(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
