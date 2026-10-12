#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contract for the Windows default-shell search (`windows_shell_candidate`).
//!
//! The candidates are derived from the environment (`%ProgramFiles%`,
//! `%ProgramFiles(x86)%`, `%SystemRoot%`) and from `PATH` the way the operating
//! system looks a program up, never from a bare name judged relative to the
//! daemon's working directory. The search is a pure function of
//! `WindowsShellSearch`, so every host runs these cases: the paths are Windows
//! strings, and the injected `is_executable` compares them with `\` and `/`
//! treated alike (a Unix host joins with `/`).

use std::path::{Path, PathBuf};

use yagura_daemon::{WindowsShellSearch, windows_shell_candidate};
use yagura_os::program::ProgramLookup;

const PROGRAM_FILES: &str = r"C:\Program Files";
const PROGRAM_FILES_X86: &str = r"C:\Program Files (x86)";
const SYSTEM_ROOT: &str = r"C:\Windows";
const PATH_DIR: &str = r"D:\tools\bin";

const PF_PWSH: &str = r"C:\Program Files\PowerShell\7\pwsh.exe";
const PF_X86_PWSH: &str = r"C:\Program Files (x86)\PowerShell\7\pwsh.exe";
const SYSTEM_POWERSHELL: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
const PATH_PWSH: &str = r"D:\tools\bin\pwsh.exe";
const PATH_PWSH_SHIM: &str = r"D:\tools\bin\pwsh.cmd";
const PATH_POWERSHELL: &str = r"D:\tools\bin\powershell.exe";

fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Defines an `is_executable` predicate (a plain `fn`, as the search requires)
/// that reports exactly the listed paths as executable.
macro_rules! executables {
    ($name:ident, [$($path:expr),* $(,)?]) => {
        fn $name(path: &Path) -> bool {
            const SET: &[&str] = &[$($path),*];
            let wanted = normalized(path);
            SET.iter().any(|entry| normalized(Path::new(entry)) == wanted)
        }
    };
}

fn lookup(path_dirs: &[&str]) -> ProgramLookup {
    ProgramLookup {
        path_dirs: path_dirs.iter().map(PathBuf::from).collect(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: Some(PathBuf::from(r"C:\Windows\System32\cmd.exe")),
    }
}

fn standard_search(is_executable: fn(&Path) -> bool) -> WindowsShellSearch {
    WindowsShellSearch {
        program_files: vec![
            PathBuf::from(PROGRAM_FILES),
            PathBuf::from(PROGRAM_FILES_X86),
        ],
        system_root: Some(PathBuf::from(SYSTEM_ROOT)),
        lookup: lookup(&[PATH_DIR]),
        is_executable,
    }
}

fn picked(search: &WindowsShellSearch) -> Option<String> {
    windows_shell_candidate(search).map(|path: PathBuf| normalized(&path))
}

fn expected(path: &str) -> Option<String> {
    Some(normalized(Path::new(path)))
}

#[test]
fn pwsh_found_only_on_path_is_picked_over_system_powershell() {
    executables!(is_exe, [PATH_PWSH, SYSTEM_POWERSHELL]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(PATH_PWSH),
        "a pwsh reachable only through PATH must be looked up there and preferred over \
         Windows PowerShell"
    );
}

#[test]
fn program_files_pwsh_is_preferred_over_path_pwsh() {
    executables!(is_exe, [PATH_PWSH, PF_PWSH]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(PF_PWSH),
        "the Program Files installation of pwsh must win over one found on PATH"
    );
}

#[test]
fn program_files_folders_are_searched_in_order() {
    executables!(is_exe, [PF_X86_PWSH, PF_PWSH]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(PF_PWSH),
        "the first program_files folder must be searched before the second"
    );
}

#[test]
fn second_program_files_folder_is_searched_when_the_first_has_no_pwsh() {
    executables!(is_exe, [PF_X86_PWSH, PATH_PWSH]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(PF_X86_PWSH),
        "every program_files folder must be searched before PATH"
    );
}

#[test]
fn without_pwsh_system_powershell_is_preferred_over_path_powershell() {
    executables!(is_exe, [PATH_POWERSHELL, SYSTEM_POWERSHELL]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(SYSTEM_POWERSHELL),
        "without any pwsh, the System32 Windows PowerShell must win over one on PATH"
    );
}

#[test]
fn without_system_powershell_path_powershell_is_picked() {
    executables!(is_exe, [PATH_POWERSHELL]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(PATH_POWERSHELL),
        "powershell reachable only through PATH must be looked up there"
    );
}

#[test]
fn pwsh_on_path_that_is_only_a_cmd_shim_is_not_picked() {
    executables!(is_exe, [PATH_PWSH_SHIM, SYSTEM_POWERSHELL]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(SYSTEM_POWERSHELL),
        "only a directly startable pwsh on PATH may be picked, never a script shim"
    );
}

#[test]
fn bare_names_relative_to_the_working_directory_are_never_picked() {
    executables!(is_exe, ["pwsh.exe", "powershell.exe"]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        None,
        "a pwsh.exe / powershell.exe judged relative to the working directory must not be picked"
    );
}

#[test]
fn a_working_directory_pwsh_does_not_outrank_system_powershell() {
    executables!(is_exe, ["pwsh.exe", SYSTEM_POWERSHELL]);
    assert_eq!(
        picked(&standard_search(is_exe)),
        expected(SYSTEM_POWERSHELL),
        "a pwsh.exe in the working directory must be ignored, leaving Windows PowerShell"
    );
}

#[test]
fn nothing_executable_yields_none() {
    executables!(is_exe, []);
    assert_eq!(
        picked(&standard_search(is_exe)),
        None,
        "with no shell anywhere, the search must report none"
    );
}

#[test]
fn program_files_on_another_drive_is_where_pwsh_is_derived_from() {
    executables!(is_exe, [PF_PWSH, r"D:\Apps\PowerShell\7\pwsh.exe"]);
    let search = WindowsShellSearch {
        program_files: vec![PathBuf::from(r"D:\Apps")],
        system_root: Some(PathBuf::from(r"E:\Win")),
        lookup: lookup(&[]),
        is_executable: is_exe,
    };
    assert_eq!(
        picked(&search),
        expected(r"D:\Apps\PowerShell\7\pwsh.exe"),
        "pwsh must be derived from the given program_files folder, whatever its drive"
    );
}

#[test]
fn system_root_on_another_drive_is_where_powershell_is_derived_from() {
    executables!(
        is_exe,
        [
            PF_PWSH,
            SYSTEM_POWERSHELL,
            r"E:\Win\System32\WindowsPowerShell\v1.0\powershell.exe",
        ]
    );
    let search = WindowsShellSearch {
        program_files: vec![PathBuf::from(r"D:\Apps")],
        system_root: Some(PathBuf::from(r"E:\Win")),
        lookup: lookup(&[]),
        is_executable: is_exe,
    };
    assert_eq!(
        picked(&search),
        expected(r"E:\Win\System32\WindowsPowerShell\v1.0\powershell.exe"),
        "no C: drive location may be assumed: only the given folders are searched"
    );
}

#[test]
fn absent_system_root_skips_to_path_powershell() {
    executables!(is_exe, [SYSTEM_POWERSHELL, PATH_POWERSHELL]);
    let search = WindowsShellSearch {
        program_files: Vec::new(),
        system_root: None,
        lookup: lookup(&[PATH_DIR]),
        is_executable: is_exe,
    };
    assert_eq!(
        picked(&search),
        expected(PATH_POWERSHELL),
        "without a system_root, no default Windows folder may be assumed"
    );
}

#[cfg(windows)]
mod windows_host {
    use std::path::PathBuf;

    use yagura_daemon::{WindowsShellSearch, resolve_shell};
    use yagura_os::program::{ProgramLookup, default_is_executable};
    use yagura_test_support::paths::{strip_extended_length_prefix, unique_temp_dir};

    #[test]
    fn from_env_derives_the_folders_from_the_environment() {
        let search = WindowsShellSearch::from_env();
        assert_eq!(
            search.system_root,
            std::env::var_os("SystemRoot").map(PathBuf::from),
            "system_root must come from %SystemRoot%"
        );
        let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
            .collect();
        assert_eq!(
            search.program_files, program_files,
            "program_files must be %ProgramFiles% then %ProgramFiles(x86)%"
        );
        assert_eq!(
            search.lookup.path_dirs,
            ProgramLookup::from_env().path_dirs,
            "lookup must be the process's own PATH lookup"
        );
    }

    #[test]
    fn resolve_shell_uses_the_given_search() {
        let dir = unique_temp_dir("yagura-shell-search", "path-pwsh");
        let pwsh = dir.join("pwsh.exe");
        std::fs::write(&pwsh, b"").expect("create placeholder pwsh.exe");
        let search = WindowsShellSearch {
            program_files: Vec::new(),
            system_root: None,
            lookup: ProgramLookup {
                path_dirs: vec![dir.clone()],
                extra_bin_dirs: Vec::new(),
                windows_comspec: None,
            },
            is_executable: default_is_executable,
        };
        let resolved = resolve_shell(None, None, &search);
        let expected = strip_extended_length_prefix(&pwsh.canonicalize().expect("canonicalize"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            resolved,
            Ok(expected),
            "resolve_shell must take its fallback from the search it is given"
        );
    }
}

#[cfg(unix)]
mod unix_host {
    use std::path::{Path, PathBuf};

    use yagura_daemon::{WindowsShellSearch, resolve_shell};

    use super::{PF_PWSH, PROGRAM_FILES, lookup, normalized};

    executables!(is_exe, [PF_PWSH]);

    #[test]
    fn resolve_shell_ignores_the_windows_search_on_unix() {
        let search = WindowsShellSearch {
            program_files: vec![PathBuf::from(PROGRAM_FILES)],
            system_root: None,
            lookup: lookup(&[]),
            is_executable: is_exe,
        };
        assert_eq!(
            resolve_shell(None, None, &search),
            Ok(PathBuf::from("/bin/sh")),
            "the Unix fallback chain must not consult the Windows search"
        );
    }
}
