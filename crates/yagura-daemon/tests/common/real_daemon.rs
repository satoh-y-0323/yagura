// Starts the compiled `yagura-daemon` binary as a real child process: what a window's
// child sees in its environment is decided only by the real startup path.
#![allow(clippy::disallowed_methods)]

//! A real `yagura-daemon` process in a temp tree of its own, and a connection to it.
//!
//! The state root is redirected into the temp tree through the environment variables
//! the daemon resolves it from, and each daemon runs under an instance identifier of its
//! own, so a running daemon of the user is never reached.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use yagura_daemon::terminal_identity::launcher_terminal_names;
use yagura_os::{PeerAuth, PlatformIpc};
use yagura_protocol::{ClientMessage, DaemonMessage};
use yagura_test_support::ipc::{Connection, connect_with_retry};
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::env_probe_path;

use super::{read_response, send, write_profile};

/// The `PATH` the daemon process itself is started with: short, so a probe line never
/// wraps far, and naming directories that need not exist.
pub fn daemon_base_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        vec![PathBuf::from(r"C:\ypa"), PathBuf::from(r"C:\ypb")]
    } else {
        vec![PathBuf::from("/ypa"), PathBuf::from("/ypb")]
    }
}

pub fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-daemon"))
}

pub fn shell_path() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        Path::new(&root).join("System32").join("cmd.exe")
    } else {
        PathBuf::from("/bin/sh")
    }
}

static INSTANCE_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A temp tree holding one daemon's whole world: its state, data and config roots, and
/// the profiles it launches from.
pub struct Sandbox {
    pub tree: TempDir,
    pub instance: String,
    pub state_home: PathBuf,
    pub profiles_dir: PathBuf,
}

impl Sandbox {
    pub fn new(tag: &str) -> Self {
        let tree = TempDir::new("yagura-cli-test", tag);
        let state_home = tree.make_dir("state");
        let profiles_dir = tree.make_dir("profiles");
        let instance = format!(
            "cp{:x}{}",
            std::process::id(),
            INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        Self {
            tree,
            instance,
            state_home,
            profiles_dir,
        }
    }

    pub fn namespace(&self) -> yagura_os::InstanceNamespace {
        yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new(&self.instance)))
            .expect("the generated instance identifier must be valid")
    }

    /// The instance's state root, derived the way the daemon derives it.
    pub fn state_root(&self) -> PathBuf {
        let base = yagura_os::state_base_dir_from(
            Some(self.state_home.as_os_str()),
            Some(self.tree.make_dir("home").as_os_str()),
        )
        .expect("the redirected state base must resolve");
        yagura_os::instance_dir(&base, &self.namespace())
    }

    /// The instance's report directory, derived the way the daemon derives it.
    pub fn report_dir(&self) -> PathBuf {
        let base = yagura_os::report_base_dir_from(
            Some(self.state_home.as_os_str()),
            Some(self.tree.make_dir("home").as_os_str()),
        )
        .expect("the redirected report base must resolve");
        yagura_daemon::config::report_dir(&base, &self.namespace())
    }

    /// The instance's data root (the session store's home), derived the way the daemon
    /// derives it from the variables [`Sandbox::start`] sets.
    pub fn data_root(&self) -> PathBuf {
        let primary = if cfg!(windows) {
            self.state_home.clone()
        } else {
            self.tree.make_dir("data")
        };
        let base = yagura_os::data_base_dir_from(
            Some(primary.as_os_str()),
            Some(self.tree.make_dir("home").as_os_str()),
        )
        .expect("the redirected data base must resolve");
        yagura_os::instance_dir(&base, &self.namespace())
    }

    /// The instance's config root, derived the same way as [`Sandbox::data_root`].
    pub fn config_root(&self) -> PathBuf {
        let primary = if cfg!(windows) {
            self.state_home.clone()
        } else {
            self.tree.make_dir("config")
        };
        let base = yagura_os::config_base_dir_from(
            Some(primary.as_os_str()),
            Some(self.tree.make_dir("home").as_os_str()),
        )
        .expect("the redirected config base must resolve");
        yagura_os::instance_dir(&base, &self.namespace())
    }

    pub fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.state_root().join("logs").join("daemon.log"))
            .unwrap_or_else(|err| format!("<no daemon log: {err}>"))
    }

    /// Starts the daemon `bin` with `args` the way a terminal would, with `launcher_env`
    /// laid over the fixed environment as the variables that terminal sets. Without
    /// `--profiles-dir` (`explicit_profiles` false) the daemon reads its profiles from its
    /// config root.
    fn spawn_daemon(
        &self,
        bin: &Path,
        launcher_env: &[(&str, &str)],
        explicit_profiles: bool,
        args: &[&str],
    ) -> Child {
        let home = self.tree.make_dir("home");
        let mut command = Command::new(bin);
        if explicit_profiles {
            command.arg("--profiles-dir").arg(&self.profiles_dir);
        }
        command.args(args);
        command
            .env("YAGURA_INSTANCE", &self.instance)
            .env("YAGURA_SHELL", shell_path())
            // A terminal launch: on Unix the daemon then keeps the inherited environment
            // instead of replacing it with the login shell's.
            .env("TERM", "xterm-256color")
            .env(
                if cfg!(windows) { "Path" } else { "PATH" },
                std::env::join_paths(daemon_base_dirs()).expect("base dirs must join"),
            );
        for name in [
            "YAGURA_STORE_DB",
            "YAGURA_PLAYBOOK",
            "YAGURA_PROFILES_DIR",
            "YAGURA_DETECTION_DIR",
            "YAGURA_ROLES",
            "YAGURA_WINDOW_ID",
            "YAGURA_STATUS_TOKEN",
            "YAGURA_STATUS_TOKEN_OVERRIDE",
            "YAGURA_MCP_BRIDGE",
            "NoDefaultCurrentDirectoryInExePath",
        ] {
            command.env_remove(name);
        }
        // Windows opened over IPC are what these tests drive, not the approval
        // gate, so the playbook lets them open; `--playbook` still overrides it.
        let playbook = self.tree.make_dir("playbook").join("playbook.toml");
        std::fs::write(&playbook, "[approval]\nauto_approve = [\"open_window\"]\n")
            .expect("write the sandbox playbook");
        command.env("YAGURA_PLAYBOOK", playbook);
        if cfg!(windows) {
            command.env("LOCALAPPDATA", &self.state_home);
        } else {
            command
                .env("XDG_STATE_HOME", &self.state_home)
                .env("XDG_DATA_HOME", self.tree.make_dir("data"))
                .env("XDG_CONFIG_HOME", self.tree.make_dir("config"))
                .env("HOME", home);
        }
        // The terminal running the tests is not the launch under test.
        let runner_env: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
        for name in launcher_terminal_names(runner_env.iter().map(OsString::as_os_str)) {
            command.env_remove(name);
        }
        command.envs(launcher_env.iter().copied());
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn the yagura-daemon binary")
    }

    pub async fn start(&self) -> RunningDaemon {
        self.start_from(&[]).await
    }

    /// [`Sandbox::start`] from a terminal that sets `launcher_env`.
    pub async fn start_from(&self, launcher_env: &[(&str, &str)]) -> RunningDaemon {
        self.connect_to(self.spawn_daemon(&daemon_bin(), launcher_env, true, &[]))
            .await
    }

    /// [`Sandbox::start`] with no `--profiles-dir`, so profiles come from the config root.
    pub async fn start_on_config_root(&self) -> RunningDaemon {
        self.connect_to(self.spawn_daemon(&daemon_bin(), &[], false, &[]))
            .await
    }

    /// [`Sandbox::start`] of the daemon executable at `bin` instead of the built one.
    pub async fn start_bin(&self, bin: &Path) -> RunningDaemon {
        self.connect_to(self.spawn_bin(bin)).await
    }

    /// Starts the daemon executable at `bin` as [`Sandbox::start_bin`] does, without
    /// connecting to it.
    pub fn spawn_bin(&self, bin: &Path) -> Child {
        self.spawn_daemon(bin, &[], true, &[])
    }

    /// Runs the built daemon executable with `args` against this sandbox's instance, as a
    /// command such as `--shutdown` is run, without connecting to it.
    pub fn spawn_with_args(&self, args: &[&str]) -> Child {
        self.spawn_daemon(&daemon_bin(), &[], false, args)
    }

    async fn connect_to(&self, child: Child) -> RunningDaemon {
        let user = PlatformIpc::default()
            .current_user_id()
            .expect("the current user id must resolve");
        let endpoint = yagura_os::default_endpoint(&user, &self.namespace());
        let client = connect_with_retry(&endpoint, Duration::from_secs(30)).await;
        RunningDaemon { child, client }
    }

    pub fn write_probe_profile(&self, id: &str, command: &[&str], env_table: &str) {
        let argv: Vec<String> = command.iter().map(|arg| format!("'{arg}'")).collect();
        write_profile(
            &self.profiles_dir,
            id,
            &format!(
                "schema = 1\ndisplay_name = \"{id}\"\n\n[command]\ndefault = [{}]\n{env_table}",
                argv.join(", ")
            ),
        );
    }

    /// A profile whose window reports the variable `name` through `env_probe`.
    pub fn write_var_probe_profile(&self, id: &str, name: &str, env_table: &str) {
        let probe = env_probe_path();
        let probe = probe.to_str().expect("env_probe path must be UTF-8");
        self.write_probe_profile(id, &[probe, name], env_table);
    }
}

/// A daemon process and a connection to it. Dropping it stops the process; the windows'
/// children go with it because the daemon owns them through a kill-on-close group.
pub struct RunningDaemon {
    child: Child,
    pub client: Connection,
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl RunningDaemon {
    pub async fn open_profile_window(&mut self, sandbox: &Sandbox, profile: &str) -> String {
        send(
            &mut self.client,
            &ClientMessage::OpenWindowFromProfile {
                profile: profile.to_string(),
                cwd: None,
                name: None,
                model: None,
                effort: None,
                on_name_taken: Default::default(),
            },
        )
        .await;
        match read_response(&mut self.client, Duration::from_secs(15)).await {
            DaemonMessage::WindowOpened { window_id, .. } => window_id,
            other => panic!(
                "expected window_opened, got {other:?}; daemon log:\n{}",
                sandbox.daemon_log()
            ),
        }
    }

    /// Polls the window's output until `done` accepts it, then returns that output; callers
    /// join lines themselves when a long value may have wrapped.
    pub async fn output_until(
        &mut self,
        window_id: &str,
        what: &str,
        done: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut last = String::new();
        loop {
            send(
                &mut self.client,
                &ClientMessage::ReadOutput {
                    window_id: window_id.to_string(),
                    lines: Some(500),
                },
            )
            .await;
            if let DaemonMessage::WindowOutput { text, .. } =
                read_response(&mut self.client, Duration::from_secs(5)).await
            {
                last = text;
                if done(&last) {
                    return last;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}; last output: {last:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// What the `env_probe` child of `profile` (written by
    /// [`Sandbox::write_var_probe_profile`]) reports for `name`: `None` when unset.
    pub async fn probed_var(
        &mut self,
        sandbox: &Sandbox,
        profile: &str,
        name: &str,
    ) -> Option<String> {
        let window_id = self.open_profile_window(sandbox, profile).await;
        let marker = format!("<<{name}");
        let text = self
            .output_until(&window_id, "the variable report", |text| {
                text.rfind(&marker)
                    .is_some_and(|start| text[start..].contains(">>"))
            })
            .await;
        let start = text.rfind(&marker)? + marker.len();
        text[start..]
            .strip_prefix('=')
            .and_then(|rest| rest.split_once(">>"))
            .map(|(value, _)| value.to_string())
    }
}
