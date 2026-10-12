//! Unique temporary directories, definition files and path normalisation.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

/// Per-process counter behind [`unique_suffix`].
static SUFFIX_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Per-process counter behind the names handed out inside a run directory.
static NAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Directory under the temp root holding one run directory per test process.
const RUNS_DIR_NAME: &str = "yagura-tests";

/// File inside a run directory whose exclusive lock marks its owner as alive.
const OWNER_LOCK_NAME: &str = "owner.lock";

/// Name the owner lock is created and locked under before it is published.
const OWNER_LOCK_STAGING_NAME: &str = "owner.lock.new";

/// The run directory of this process, together with the handle that keeps
/// its owner lock held. Kept in a static so the handle lives until the
/// process exits: children of a test outlive the test function, so tying the
/// directory to the process lets the next run sweep it once nobody holds it.
struct RunDir {
    path: PathBuf,
    _owner: File,
}

static RUN_DIR: OnceLock<RunDir> = OnceLock::new();

/// The run directory of this process under `std::env::temp_dir()`, created
/// (after sweeping the run directories of dead processes) on first use.
pub(crate) fn run_dir() -> &'static Path {
    &RUN_DIR
        .get_or_init(|| create_run_dir(&std::env::temp_dir()))
        .path
}

/// The run directory for Unix socket fixtures: under `/tmp` so a long
/// `$TMPDIR` (macOS) cannot push a socket path over `sun_path`. The same
/// directory as [`run_dir`] when the temp directory already is `/tmp`, and
/// [`run_dir`] itself when `/tmp` is missing.
#[cfg(unix)]
pub(crate) fn socket_run_dir() -> &'static Path {
    static SOCKET_RUN_DIR: OnceLock<RunDir> = OnceLock::new();
    let tmp = Path::new("/tmp");
    if !tmp.is_dir() || std::env::temp_dir() == tmp {
        return run_dir();
    }
    &SOCKET_RUN_DIR.get_or_init(|| create_run_dir(tmp)).path
}

/// A name for a directory inside a run directory: `{prefix}-{tag}-{n}`. The
/// run directory already separates processes, so the name carries only a
/// per-process sequence number and stays short.
pub(crate) fn run_entry_name(prefix: &str, tag: &str) -> String {
    let sequence = NAME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{tag}-{sequence}")
}

/// Run directory names are `{pid:x}-{t:x}` with `t` under 2^28, at most 16
/// bytes: every fixture path, and every message quoting one, carries it.
const RUN_NAME_TIME_MODULUS: u128 = 1 << 28;

fn run_dir_name(attempt: u128) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_millis();
    let time = (millis + attempt) % RUN_NAME_TIME_MODULUS;
    format!("{:x}-{time:x}", std::process::id())
}

fn create_run_dir(root: &Path) -> RunDir {
    let base = root.join(RUNS_DIR_NAME);
    std::fs::create_dir_all(&base).expect("create the yagura-tests directory");
    sweep_dead_runs(&base);

    // The short name is not unique on its own (a reused pid, a wrapped
    // clock), so a name already taken is skipped rather than shared.
    let mut attempt = 0;
    let path = loop {
        let candidate = base.join(run_dir_name(attempt));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            Err(e) => panic!("create this process's run directory {candidate:?}: {e}"),
        }
    };
    let staging = path.join(OWNER_LOCK_STAGING_NAME);
    let owner = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&staging)
        .expect("create the run directory's owner lock");
    owner
        .try_lock()
        .expect("lock the freshly created owner lock");
    // Published only once locked: a concurrent sweep that opened an unlocked
    // owner.lock would take this live directory for a dead one.
    std::fs::rename(&staging, path.join(OWNER_LOCK_NAME))
        .expect("publish the run directory's owner lock");
    RunDir {
        path,
        _owner: owner,
    }
}

/// Removes every run directory directly under `base` whose `owner.lock` can
/// be locked, i.e. whose owning process has exited.
///
/// A run directory whose `owner.lock` is held, missing or cannot be opened is
/// left alone, and so is anything that cannot be removed (a child of a dead
/// test may still use it as its working directory): the next sweep retries.
/// `owner.lock` is removed last so a partly removed directory stays
/// recognisable as a run directory.
pub fn sweep_dead_runs(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            remove_run_if_dead(&entry.path());
        }
    }
}

fn remove_run_if_dead(run: &Path) {
    let lock_path = run.join(OWNER_LOCK_NAME);
    let Ok(owner) = File::options().read(true).write(true).open(&lock_path) else {
        return;
    };
    if owner.try_lock().is_err() {
        return;
    }
    if !remove_contents_except(run, OWNER_LOCK_NAME) {
        return;
    }
    // Closed before removal: Windows cannot remove a directory while a file
    // inside it is still open.
    drop(owner);
    if std::fs::remove_file(&lock_path).is_ok() {
        let _ = std::fs::remove_dir(run);
    }
}

/// Removes every entry of `dir` except `keep`, returning whether all of them
/// were removed.
fn remove_contents_except(dir: &Path, keep: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut all_removed = true;
    for entry in entries {
        let Ok(entry) = entry else {
            all_removed = false;
            continue;
        };
        if entry.file_name() == keep {
            continue;
        }
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            // A directory symlink or junction is removed as a directory on Windows.
            std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir(&path))
        };
        all_removed &= removed.is_ok();
    }
    all_removed
}

/// A name fragment unique to this call, shaped `{pid}-{nanos}-{sequence}`.
///
/// The elements separate, in turn, concurrent test binaries, successive runs
/// of one binary (and a resident daemon), and two calls within one clock tick.
///
/// # Panics
///
/// Panics if the system clock is set before the Unix epoch.
pub fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let sequence = SUFFIX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{sequence}", std::process::id())
}

/// Creates a temporary directory named `{prefix}-{tag}-{n}` in this
/// process's run directory (`<temp_dir>/yagura-tests/<run>/`) and returns its
/// path. Nothing removes it while the process lives; a later process's
/// [`sweep_dead_runs`] removes it with the run directory (use [`TempDir`] to
/// remove it on drop instead).
///
/// The path must not contain whitespace: [`crate::process::grandchild_command`]
/// interpolates a path from such a directory into a command line the child
/// re-parses internally (`cmd.exe`, `/bin/sh -c`), where a quoted path cannot
/// be re-quoted safely.
///
/// # Panics
///
/// Panics if the system clock is set before the Unix epoch, if the run
/// directory or the directory cannot be created, if its path is not valid
/// UTF-8, or if that path contains whitespace.
pub fn unique_temp_dir(prefix: &str, tag: &str) -> PathBuf {
    let dir = run_dir().join(run_entry_name(prefix, tag));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let text = dir.to_str().expect("temp dir path must be valid UTF-8");
    assert!(
        !text.chars().any(char::is_whitespace),
        "temp dir must not contain whitespace (see fixture note): {text}"
    );
    dir
}

/// Creates a temporary directory named `{prefix}-{tag}-{n}` in this
/// process's run directory, reporting a creation failure with the caller's
/// own `context`.
///
/// A fresh directory is built rather than using `std::env::temp_dir()` itself
/// because the OS temp directory is not guaranteed to be canonical (short 8.3
/// names, junctions), which makes path assertions against it flaky.
///
/// # Panics
///
/// Panics if the system clock is set before the Unix epoch, if the run
/// directory cannot be set up, or if the directory cannot be created -- the
/// latter reported with `context`.
pub fn unique_detection_dir(prefix: &str, tag: &str, context: &str) -> PathBuf {
    let dir = run_dir().join(run_entry_name(prefix, tag));
    std::fs::create_dir_all(&dir).expect(context);
    dir
}

/// Writes a detection definition as `{name}.toml` inside `dir`.
///
/// # Panics
///
/// Panics if the file cannot be written.
pub fn write_definition(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(format!("{name}.toml")), contents)
        .expect("write detection definition file");
}

/// Removes the Windows extended-length prefix from `path`, restoring a UNC
/// path to its `\\server\share` form and leaving any other path untouched.
///
/// Mirrors the normalisation `resolve_shell` is contracted to perform after
/// `canonicalize()` as an independent implementation, so a test built
/// on it asserts the contract instead of re-running the code under test.
#[must_use]
pub fn strip_extended_length_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// A uniquely named temporary directory that is removed when it is dropped.
///
/// Both constructors -- [`TempDir::new`] and [`short_tmp_dir`] -- enforce the
/// whitespace rule documented on [`unique_temp_dir`].
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a directory named `{prefix}-{label}-{n}` in this
    /// process's run directory.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`unique_temp_dir`], including a
    /// resulting path that contains whitespace.
    #[must_use]
    pub fn new(prefix: &str, label: &str) -> Self {
        Self {
            path: unique_temp_dir(prefix, label),
        }
    }

    /// The directory this fixture owns.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `contents` to `file_name` inside the directory and returns the
    /// path written, so a caller can assert on it without rebuilding the join.
    ///
    /// # Panics
    ///
    /// Panics if the file cannot be written.
    pub fn write_file(&self, file_name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(file_name);
        std::fs::write(&path, contents).expect("write fixture file");
        path
    }

    /// Creates the directory `relative` (and any missing parents) inside the
    /// directory and returns its path.
    ///
    /// # Panics
    ///
    /// Panics if the directory cannot be created.
    pub fn make_dir(&self, relative: &str) -> PathBuf {
        let path = self.path.join(relative);
        std::fs::create_dir_all(&path).expect("create fixture subdirectory");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Bounded-origin scratch directory for Unix socket-path fixtures:
/// `/tmp/yagura-tests/<run>/{prefix}-{label}-{n}`, mode 0700, in the run
/// directory of this process under `/tmp` (under `std::env::temp_dir()` when
/// `/tmp` is missing or not a directory).
///
/// # Panics
///
/// Panics, whichever origin was used, if the resulting path breaks the
/// endpoint length budget or contains whitespace.
#[cfg(unix)]
#[must_use]
pub fn short_tmp_dir(prefix: &str, label: &str) -> TempDir {
    let dir = socket_run_dir().join(run_entry_name(prefix, label));
    let text = dir
        .to_str()
        .expect("short tmp dir path must be valid UTF-8");
    // Reuses the single endpoint length budget rather than a second
    // threshold, so both origins fail loud on the same rule.
    assert!(
        crate::ipc::endpoint_candidate_within_budget(text),
        "short_tmp_dir path breaks the endpoint length budget: {text}"
    );
    assert!(
        !text.chars().any(char::is_whitespace),
        "temp dir must not contain whitespace (see fixture note): {text}"
    );
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .expect("create short tmp dir");
    TempDir { path: dir }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_dir_skips_names_already_taken() {
        let root = unique_temp_dir("yagura-run-dir", "taken");
        let base = root.join(RUNS_DIR_NAME);
        // Taking every name of the coming second makes the first candidates collide.
        for attempt in 0..1000 {
            std::fs::create_dir_all(base.join(run_dir_name(attempt))).expect("take a run dir name");
        }

        let run = create_run_dir(&root);

        assert!(
            run.path.join(OWNER_LOCK_NAME).is_file(),
            "the run dir must be created past the taken names: {:?}",
            run.path
        );
    }

    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "create this process's run directory")]
    fn a_run_dir_that_cannot_be_created_fails_instead_of_retrying() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_temp_dir("yagura-run-dir", "read-only");
        let base = root.join(RUNS_DIR_NAME);
        std::fs::create_dir_all(&base).expect("create the runs dir");
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o555))
            .expect("make the runs dir read-only");

        create_run_dir(&root);
    }
}
