//! The pieces `CreateProcessW` is given: the program, the command line, the
//! environment block and the working directory, built by one set of rules.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_MORE_DATA;
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
    RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::program::{self, Platform, ProgramLookup, Resolution};

/// Facility code of an `HRESULT` that wraps a Win32 error code.
const FACILITY_WIN32_HRESULT: u32 = 0x8007_0000;

/// Extensions `CreateProcessW` would hand to `cmd.exe` implicitly.
const BATCH_EXTENSIONS: [&str; 2] = ["cmd", "bat"];

/// Converts a `windows` crate error back into the Win32 error code it wraps,
/// so its [`io::ErrorKind`] (e.g. `NotFound`) survives: the crate's own
/// conversion keeps the `HRESULT`, which maps to no kind.
pub(crate) fn win32_error_to_io(error: &windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code & 0xFFFF_0000 == FACILITY_WIN32_HRESULT {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(error.message().to_string())
    }
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// `value` as a NUL-terminated wide string.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] when `value` itself contains a NUL.
pub(crate) fn to_wide_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid_input("a spawn argument contains a NUL character"));
    }
    wide.push(0);
    Ok(wide)
}

/// The command line for `application` followed by `args`, NUL-terminated,
/// quoted so the child's C runtime splits it back into exactly these words.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] when any word contains a NUL.
pub(crate) fn command_line(application: &Path, args: &[String]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    append_quoted(application.as_os_str(), &mut line)?;
    for arg in args {
        line.push(u16::from(b' '));
        append_quoted(OsStr::new(arg), &mut line)?;
    }
    line.push(0);
    Ok(line)
}

/// Appends `arg` by the `CommandLineToArgvW` rules: quoted only when empty or
/// holding whitespace or a quote, with backslashes doubled before a quote and
/// at the end of a quoted word.
fn append_quoted(arg: &OsStr, line: &mut Vec<u16>) -> io::Result<()> {
    let wide: Vec<u16> = arg.encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid_input("a spawn argument contains a NUL character"));
    }
    let quote = u16::from(b'"');
    let backslash = u16::from(b'\\');
    let needs_quoting = wide.is_empty()
        || wide
            .iter()
            .any(|&unit| b" \t\n\x0b\"".map(u16::from).contains(&unit));
    if !needs_quoting {
        line.extend_from_slice(&wide);
        return Ok(());
    }
    line.push(quote);
    let mut backslashes = 0usize;
    for &unit in &wide {
        if unit == backslash {
            backslashes += 1;
            continue;
        }
        let repeat = if unit == quote {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        line.extend(std::iter::repeat_n(backslash, repeat));
        line.push(unit);
        backslashes = 0;
    }
    line.extend(std::iter::repeat_n(backslash, backslashes * 2));
    line.push(quote);
    Ok(())
}

/// A child's environment: one entry per name, names compared without regard
/// to case the way Windows itself compares them.
#[derive(Default)]
pub(crate) struct ChildEnv {
    vars: BTreeMap<Vec<u16>, (OsString, OsString)>,
}

impl ChildEnv {
    /// The calling process's own environment.
    pub(crate) fn inherited() -> Self {
        let mut env = Self::default();
        for (name, value) in std::env::vars_os() {
            env.set(name, value);
        }
        env
    }

    /// The environment a window's child starts from: the calling process's
    /// own, overlaid with the machine's and then the user's persistent
    /// variables from the registry, as a freshly started program would see them.
    ///
    /// The machine `PATH` replaces the process's (so a daemon started from a
    /// stale shell still finds what was installed since) and the user's is
    /// appended to it. `USERNAME` is not taken from the machine key.
    pub(crate) fn window_base() -> Self {
        let mut env = Self::inherited();
        for (name, value) in registry_environment(HKEY_LOCAL_MACHINE, SYSTEM_ENVIRONMENT_KEY) {
            if !name.eq_ignore_ascii_case("USERNAME") {
                env.set(name, value);
            }
        }
        for (name, mut value) in registry_environment(HKEY_CURRENT_USER, USER_ENVIRONMENT_KEY) {
            if name.eq_ignore_ascii_case("PATH")
                && let Some(system) = env.get(&name)
            {
                let mut joined = system.to_os_string();
                joined.push(";");
                joined.push(&value);
                value = joined;
            }
            env.set(name, value);
        }
        env
    }

    /// Sets `name` to `value`, replacing an entry whose name differs only in case.
    pub(crate) fn set(&mut self, name: OsString, value: OsString) {
        self.vars.insert(fold_case(&name), (name, value));
    }

    /// Removes `name`, compared without regard to case.
    pub(crate) fn remove(&mut self, name: &OsStr) {
        self.vars.remove(&fold_case(name));
    }

    /// The value of `name`, compared without regard to case.
    pub(crate) fn get(&self, name: &OsStr) -> Option<&OsStr> {
        self.vars
            .get(&fold_case(name))
            .map(|(_, value)| value.as_os_str())
    }

    /// The `CREATE_UNICODE_ENVIRONMENT` block: `name=value` entries sorted by
    /// their upper-cased name, each NUL-terminated, then one more NUL.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] when a name is empty or holds `=` past
    /// its first character, or a name or value holds a NUL.
    pub(crate) fn to_block(&self) -> io::Result<Vec<u16>> {
        let equals = u16::from(b'=');
        let mut block = Vec::new();
        for (name, value) in self.vars.values() {
            let name: Vec<u16> = name.encode_wide().collect();
            let value: Vec<u16> = value.encode_wide().collect();
            // A leading `=` is how Windows names its per-drive directories (`=C:`).
            if name.is_empty() || name.iter().skip(1).any(|&unit| unit == equals) {
                return Err(invalid_input(
                    "an environment variable name is empty or contains '='",
                ));
            }
            if name.contains(&0) || value.contains(&0) {
                return Err(invalid_input(
                    "an environment variable contains a NUL character",
                ));
            }
            block.extend_from_slice(&name);
            block.push(equals);
            block.extend_from_slice(&value);
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        Ok(block)
    }
}

/// The registry key holding the machine's persistent environment variables.
const SYSTEM_ENVIRONMENT_KEY: &str =
    r"System\CurrentControlSet\Control\Session Manager\Environment";

/// The registry key holding the user's persistent environment variables.
const USER_ENVIRONMENT_KEY: &str = "Environment";

/// The string values (`REG_SZ`, and `REG_EXPAND_SZ` expanded) directly under
/// `root\subkey`, as `(name, value)`. A key that cannot be opened or read
/// yields what was read so far: an unreadable key only leaves its variables
/// out.
fn registry_environment(root: HKEY, subkey: &str) -> Vec<(OsString, OsString)> {
    let Ok(subkey_wide) = to_wide_nul(OsStr::new(subkey)) else {
        return Vec::new();
    };
    let mut key = HKEY::default();
    // SAFETY: `subkey_wide` is NUL-terminated and outlives the call; `key` is a
    // live out-parameter and is closed below.
    if unsafe { RegOpenKeyExW(root, PCWSTR(subkey_wide.as_ptr()), 0, KEY_READ, &mut key) }.is_err()
    {
        return Vec::new();
    }
    let values = read_registry_values(key);
    // SAFETY: `key` was opened above and is closed exactly once here.
    let _ = unsafe { RegCloseKey(key) };
    values
}

fn read_registry_values(key: HKEY) -> Vec<(OsString, OsString)> {
    let mut longest_name = 0u32;
    let mut longest_data = 0u32;
    // SAFETY: both out-parameters are live for the call; the other
    // out-parameters are not requested.
    if unsafe {
        RegQueryInfoKeyW(
            key,
            PWSTR::null(),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&mut longest_name),
            Some(&mut longest_data),
            None,
            None,
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    enumerate_registry_values(key, longest_name, longest_data)
}

/// Reads the string values of `key` into buffers sized for names of up to
/// `longest_name` characters and data of up to `longest_data` bytes. A value
/// that does not fit, because it grew after those sizes were queried, is skipped.
fn enumerate_registry_values(
    key: HKEY,
    longest_name: u32,
    longest_data: u32,
) -> Vec<(OsString, OsString)> {
    // `u16` storage keeps the value bytes aligned for reading them as UTF-16.
    let mut name = vec![0u16; longest_name as usize + 1];
    let mut data = vec![0u16; longest_data as usize / 2 + 1];
    let mut values = Vec::new();
    for index in 0.. {
        let mut name_len = name.len() as u32;
        let mut data_len = (data.len() * 2) as u32;
        let mut kind = 0u32;
        // SAFETY: `name` and `data` are live for the call and their lengths
        // are passed in characters and bytes respectively.
        let read = unsafe {
            RegEnumValueW(
                key,
                index,
                PWSTR(name.as_mut_ptr()),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr().cast::<u8>()),
                Some(&mut data_len),
            )
        };
        match read {
            Ok(()) => {}
            // The value grew after the sizes were queried; skip it.
            Err(error) if error.code() == ERROR_MORE_DATA.to_hresult() => continue,
            Err(_) => break,
        }
        let units = &data[..(data_len as usize / 2).min(data.len())];
        let Some(value) = registry_string(kind, units) else {
            continue;
        };
        values.push((
            OsString::from_wide(&name[..(name_len as usize).min(name.len())]),
            value,
        ));
    }
    values
}

/// A registry string value as text: `REG_SZ` as stored, `REG_EXPAND_SZ` with
/// its `%VARIABLES%` expanded, anything else `None`.
fn registry_string(kind: u32, units: &[u16]) -> Option<OsString> {
    if kind != REG_SZ.0 && kind != REG_EXPAND_SZ.0 {
        return None;
    }
    let end = units
        .iter()
        .rposition(|&unit| unit != 0)
        .map_or(0, |i| i + 1);
    let text = &units[..end];
    if kind == REG_SZ.0 {
        return Some(OsString::from_wide(text));
    }
    let mut source = text.to_vec();
    source.push(0);
    // SAFETY: `source` is NUL-terminated; a missing buffer only asks for the
    // size, in characters including the terminating NUL.
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
    if needed == 0 {
        return None;
    }
    let mut expanded = vec![0u16; needed as usize];
    // SAFETY: `source` is NUL-terminated and `expanded` is live for the call
    // with the length passed.
    let written =
        unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut expanded)) };
    if written == 0 || written as usize > expanded.len() {
        return None;
    }
    let end = expanded[..written as usize]
        .iter()
        .rposition(|&unit| unit != 0)
        .map_or(0, |i| i + 1);
    Some(OsString::from_wide(&expanded[..end]))
}

/// The directory a window's child starts in: `requested` when it is a
/// directory, else the child's `USERPROFILE` when that is, else none (the
/// child inherits the calling process's). A relative directory is taken from
/// the calling process's working directory.
pub(crate) fn window_cwd(requested: Option<&Path>, env: &ChildEnv) -> Option<PathBuf> {
    let home = env
        .get(OsStr::new("USERPROFILE"))
        .map(Path::new)
        .filter(|path| path.is_dir());
    let dir = requested.filter(|path| path.is_dir()).or(home)?;
    if dir.is_relative() {
        return Some(
            std::env::current_dir().map_or_else(|_| dir.to_path_buf(), |current| current.join(dir)),
        );
    }
    Some(dir.to_path_buf())
}

/// `name` upper-cased unit by unit, the key the environment is compared and
/// sorted by. A unit whose upper case is not a single unit is kept as it is.
fn fold_case(name: &OsStr) -> Vec<u16> {
    name.encode_wide()
        .map(|unit| {
            char::from_u32(u32::from(unit))
                .and_then(|c| {
                    let mut upper = c.to_uppercase();
                    match (upper.next(), upper.next()) {
                        (Some(single), None) => u16::try_from(u32::from(single)).ok(),
                        _ => None,
                    }
                })
                .unwrap_or(unit)
        })
        .collect()
}

/// The program `CreateProcessW` is told to start for `program`.
///
/// A name holding a path separator is taken as written, with no extension
/// appended (the `PATHEXT` completion portable-pty did implicitly is not
/// carried over), so one that names no file fails to start. A bare name is
/// searched for in `child_path` (the child's own `PATH`) by
/// [`program::resolve_program`]'s Windows rules and must resolve to an
/// executable image there; it is never handed to `CreateProcessW` unresolved,
/// which would search the calling process's own directories instead.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidInput`] when the program is a `.cmd` / `.bat`
///   file, which `CreateProcessW` would run through `cmd.exe` with no check
///   of its arguments (wrap it with [`program::into_spawn_argv`] instead), or
///   when a bare name carries a drive prefix.
/// - [`io::ErrorKind::NotFound`] when a bare name resolves to no executable
///   image in `child_path`.
pub(crate) fn resolve_application(
    program: &str,
    child_path: Option<&OsStr>,
) -> io::Result<PathBuf> {
    if program.contains(['/', '\\']) {
        let path = PathBuf::from(program);
        if names_batch_file(&path) {
            return Err(invalid_input(
                "a .cmd or .bat file cannot be started directly; wrap it through the command \
                 interpreter",
            ));
        }
        return Ok(path);
    }
    if program.contains(':') {
        // `C:name` is relative to a drive's current directory, not a bare name.
        return Err(invalid_input(
            "a program name with a drive prefix must be a full path",
        ));
    }
    let lookup = ProgramLookup {
        path_dirs: child_path
            .map(|path| std::env::split_paths(path).collect())
            .unwrap_or_default(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: None,
    };
    let argv = [program.to_string()];
    match program::resolve_program(
        &argv,
        Platform::Windows,
        &lookup,
        &program::default_is_executable,
    ) {
        Ok(Resolution::Direct(path)) if !names_batch_file(&path) => Ok(path),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the program was not found as an executable in the child's PATH",
        )),
    }
}

/// Whether `path` names a batch file as `CreateProcessW` reads the name:
/// trailing dots and spaces are dropped and a `:stream` suffix is ignored.
fn names_batch_file(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let name = name.to_string_lossy();
    let name = name.split(':').next().unwrap_or_default();
    let name = name.trim_end_matches(['.', ' ']);
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            BATCH_EXTENSIONS
                .iter()
                .any(|batch| extension.eq_ignore_ascii_case(batch))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{E_FAIL, ERROR_FILE_NOT_FOUND};
    use windows::core::HSTRING;

    fn env_with(name: &str, value: &str) -> ChildEnv {
        let mut env = ChildEnv::default();
        env.set(OsString::from(name), OsString::from(value));
        env
    }

    fn assert_refused(env: &ChildEnv, what: &str) {
        let error = env
            .to_block()
            .expect_err(&format!("{what} must not reach the environment block"));
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{what}");
    }

    // An `=` past the first character or a NUL would make Windows read the
    // block as a different variable than the one that was set.
    #[test]
    fn a_name_the_block_cannot_carry_is_refused() {
        assert_refused(&env_with("A=B", "value"), "a name holding '='");
        assert_refused(&env_with("", "value"), "an empty name");
        assert_refused(&env_with("A\0B", "value"), "a name holding a NUL");
    }

    #[test]
    fn a_value_holding_a_nul_is_refused() {
        assert_refused(
            &env_with("NAME", "value\0PATH=injected"),
            "a value holding a NUL",
        );
    }

    #[test]
    fn a_drive_directory_name_and_ordinary_variables_are_kept() {
        let mut env = env_with("=C:", r"C:\work");
        env.set(OsString::from("Name"), OsString::from("a=b"));
        let block = env.to_block().expect("both entries are valid");
        let text = String::from_utf16(&block).expect("the block is UTF-16");
        assert_eq!(text, "=C:=C:\\work\0Name=a=b\0\0");
    }

    // `CreateProcessW` drops trailing dots, so `x.cmd.` would start `x.cmd`
    // through `cmd.exe` with none of its arguments checked.
    #[test]
    fn a_bare_name_that_reaches_a_batch_file_through_trailing_dots_is_not_started() {
        let dir = yagura_test_support::paths::TempDir::new("yagura-os-batch", "trailing-dots");
        dir.write_file("probe.cmd", "@echo off\r\n");

        let result = resolve_application("probe.cmd.", Some(dir.path().as_os_str()));

        let error = result.expect_err("a batch file must never be started directly");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_win32_error_keeps_its_code_and_its_kind() {
        let error = windows::core::Error::new(ERROR_FILE_NOT_FOUND.to_hresult(), HSTRING::new());

        let converted = win32_error_to_io(&error);

        assert_eq!(
            converted.raw_os_error(),
            Some(ERROR_FILE_NOT_FOUND.0 as i32)
        );
        assert_eq!(converted.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn an_error_that_wraps_no_win32_code_keeps_no_os_code() {
        let error = windows::core::Error::new(E_FAIL, HSTRING::new());

        assert_eq!(win32_error_to_io(&error).raw_os_error(), None);
    }

    /// Runs `body` while another thread repeats `churn`, then stops that thread
    /// and returns how `body` ended.
    fn while_churning(
        churn: impl Fn() + Send + 'static,
        body: impl FnOnce(),
    ) -> std::thread::Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    churn();
                }
            })
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        stop.store(true, Ordering::Relaxed);
        worker.join().expect("the churning thread does not panic");
        result
    }

    // The expansion asks for the size and then fills a buffer of it; a variable
    // that changes in between must end as `None` or a whole value, never a panic.
    #[test]
    fn an_expansion_racing_a_changing_variable_ends_as_none_or_a_whole_value() {
        use windows::Win32::System::Environment::SetEnvironmentVariableW;

        const LONG: usize = 4000;
        let name = to_wide_nul(OsStr::new("YAGURA_EXPAND_RACE")).unwrap();
        let short = to_wide_nul(OsStr::new("s")).unwrap();
        let long = to_wide_nul(OsStr::new(&"l".repeat(LONG))).unwrap();
        let reference: Vec<u16> = "%YAGURA_EXPAND_RACE%".encode_utf16().collect();
        let churn_name = name.clone();

        let result = while_churning(
            move || {
                for value in [&short, &long] {
                    // SAFETY: both strings are NUL-terminated and outlive the call.
                    let _ = unsafe {
                        SetEnvironmentVariableW(PCWSTR(churn_name.as_ptr()), PCWSTR(value.as_ptr()))
                    };
                }
            },
            || {
                for _ in 0..20_000 {
                    if let Some(value) = registry_string(REG_EXPAND_SZ.0, &reference) {
                        // The text stays as written while the variable is not set yet.
                        let length = value.len();
                        assert!(
                            [1, LONG, reference.len()].contains(&length),
                            "a torn value of {length}"
                        );
                    }
                }
            },
        );

        // SAFETY: the name is NUL-terminated; a null value removes the variable.
        let _ = unsafe { SetEnvironmentVariableW(PCWSTR(name.as_ptr()), PCWSTR::null()) };
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    fn set_registry_string(key: HKEY, name: &str, value: &str) {
        use windows::Win32::System::Registry::RegSetValueExW;

        let name = to_wide_nul(OsStr::new(name)).unwrap();
        let data: Vec<u8> = value
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: `name` is NUL-terminated and `data` is live for the call.
        let status = unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(&data)) };
        assert!(status.is_ok(), "the scratch value can be written");
    }

    // Buffers sized before a value grew: that value is skipped and the values
    // after it are still read.
    #[test]
    fn a_registry_value_larger_than_its_buffer_is_skipped_and_the_rest_are_read() {
        use windows::Win32::System::Registry::{
            KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, RegCreateKeyExW, RegDeleteKeyW,
        };

        let subkey = format!(r"Software\yagura-os-test-{}", std::process::id());
        let subkey_wide = to_wide_nul(OsStr::new(&subkey)).unwrap();
        let mut key = HKEY::default();
        // SAFETY: `subkey_wide` is NUL-terminated and `key` is a live out-parameter;
        // the key is closed and deleted below.
        let created = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey_wide.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                None,
                &mut key,
                None,
            )
        };
        assert!(created.is_ok(), "the scratch key can be created");

        let result = std::panic::catch_unwind(|| {
            set_registry_string(key, "A", "a value far longer than the buffer");
            set_registry_string(key, "B", "b");
            set_registry_string(key, "C", "c");

            // Room for one-character names and a one-character value.
            let values = enumerate_registry_values(key, 1, 2);

            let found: Vec<(String, String)> = values
                .iter()
                .map(|(name, value)| {
                    (
                        name.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                })
                .collect();
            assert_eq!(
                found,
                [("B".into(), "b".into()), ("C".into(), "c".into())],
                "only the value that fits is skipped"
            );
        });

        // SAFETY: `key` was created above; it is closed once, then its name deleted.
        unsafe {
            let _ = RegCloseKey(key);
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(subkey_wide.as_ptr()));
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
