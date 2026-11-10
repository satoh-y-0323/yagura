//! The window's relay (`yagura-mcp`) running as a child whose stdin and stdout
//! the test speaks as the CLI does: one JSON-RPC message per line.
//!
//! The workspace does not enable `rmcp`'s client features, so the lines are
//! written and read by hand.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::HANG_GUARD;

/// Variables removed from every relay's environment before the test's own
/// are applied, so no ambient value of the test runner's shell (such as a
/// developer's `YAGURA_INSTANCE=dev`) changes which daemon it dials or which
/// window it names.
const CLEARED_ENV: [&str; 4] = [
    "YAGURA_INSTANCE",
    "YAGURA_STORE_DB",
    "YAGURA_WINDOW_ID",
    "YAGURA_STATUS_TOKEN",
];

/// A relay process with its stdin and stdout piped to the test. Killed on
/// drop, so a failing assertion never leaves it running.
pub struct RelayProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl RelayProcess {
    /// Starts the relay executable `bin` with [`CLEARED_ENV`] removed, then
    /// `env` set and `args` on its command line. The test's own environment
    /// is not changed.
    ///
    /// # Panics
    ///
    /// Panics if the process cannot be started.
    // The relay is started the way a CLI starts its MCP server: as a plain
    // child process with piped stdio, which no window spawn of yagura-os is.
    #[allow(clippy::disallowed_methods)]
    #[must_use]
    pub fn spawn(bin: &Path, env: &[(&str, &str)], args: &[&OsStr]) -> Self {
        let mut command = Command::new(bin);
        command.args(args);
        for name in CLEARED_ENV {
            command.env_remove(name);
        }
        command
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .unwrap_or_else(|err| panic!("failed to start the relay {}: {err}", bin.display()));
        let stdin = child.stdin.take().expect("the relay's stdin is piped");
        let stdout = child.stdout.take().expect("the relay's stdout is piped");
        Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        }
    }

    /// The relay's process id while it runs.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Writes `message` as one line of compact JSON.
    pub async fn send(&mut self, message: &Value) {
        self.send_raw(&message.to_string()).await;
    }

    /// Writes `line` and a newline, whatever `line` holds.
    ///
    /// # Panics
    ///
    /// Panics if the relay's stdin was closed or cannot be written.
    pub async fn send_raw(&mut self, line: &str) {
        let stdin = self
            .stdin
            .as_mut()
            .expect("the relay's stdin is still open");
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        stdin
            .write_all(&bytes)
            .await
            .expect("write a line to the relay's stdin");
        stdin.flush().await.expect("flush the relay's stdin");
    }

    /// The next line the relay writes, or `None` once its stdout ended.
    ///
    /// # Panics
    ///
    /// Panics if no line comes within [`HANG_GUARD`], or on a line that is
    /// not JSON.
    pub async fn next_line_or_closed(&mut self) -> Option<Value> {
        let mut line = String::new();
        let read = tokio::time::timeout(HANG_GUARD, self.stdout.read_line(&mut line))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for a line on the relay's stdout"));
        match read {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap_or_else(|err| {
                panic!("the relay must write JSON-RPC lines ({err}); it wrote {line:?}")
            })),
        }
    }

    /// The next line the relay writes.
    ///
    /// # Panics
    ///
    /// Panics as [`Self::next_line_or_closed`] does, and if stdout ended.
    pub async fn next_line(&mut self) -> Value {
        self.next_line_or_closed()
            .await
            .expect("the relay's stdout ended")
    }

    /// Reads lines until the answer to the request `id` (a line carrying that
    /// `id` and no `method`), and returns it with every line read before it.
    pub async fn answer_with_preceding(&mut self, id: &Value) -> (Value, Vec<Value>) {
        let mut before = Vec::new();
        loop {
            let line = self.next_line().await;
            if line.get("method").is_none() && line.get("id") == Some(id) {
                return (line, before);
            }
            before.push(line);
        }
    }

    /// The answer to the request `id`; see [`Self::answer_with_preceding`].
    pub async fn answer(&mut self, id: &Value) -> Value {
        self.answer_with_preceding(id).await.0
    }

    /// Closes the relay's stdin, as a CLI ending its MCP server does.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Kills the relay and waits for it to be gone.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }

    /// Waits for the relay to end by itself and returns how it ended.
    ///
    /// # Panics
    ///
    /// Panics if it has not ended within [`HANG_GUARD`].
    pub async fn exited(&mut self) -> ExitStatus {
        tokio::time::timeout(HANG_GUARD, self.child.wait())
            .await
            .unwrap_or_else(|_| panic!("the relay did not end within {HANG_GUARD:?}"))
            .expect("wait for the relay")
    }
}
