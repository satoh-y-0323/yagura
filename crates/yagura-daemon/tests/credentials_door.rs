#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the compiled `yagura-daemon` binary as a real child process against a daemon
// served in this process: what a CLI's hook sees is its stdout, its stderr and its exit
// code.
#![allow(clippy::disallowed_methods)]

//! Integration test: a hook door started without the window's credentials in its
//! environment names its window through `--credentials <path>`, a file holding
//! `{"window_id":"<id>","token":"<hex>"}`. A file that is missing, unreadable or
//! carries an empty field is no claim: the door stays silent and exits 0.
//!
//! Every spawn sets `YAGURA_INSTANCE` to an identifier of this test's own, so the endpoint
//! it connects to is the in-process daemon's and never a running daemon of the user, and
//! points the data, state and config base directories into a temporary directory.

use crate::common;
use crate::common::door_relay::run_door_without_credentials;

use std::path::{Path, PathBuf};
use std::process::Output;

use yagura_daemon::inbox::Parcel;

use common::{Harness, VALID_TOKEN, approver_base_config, detection_config, unique_temp_dir};

struct Setup {
    h: Harness,
    files: PathBuf,
}

impl Setup {
    async fn start() -> Self {
        let config = detection_config(&unique_temp_dir("credentials-door"), approver_base_config());
        let h = Harness::start("credentials-door", config).await;
        Self {
            h,
            files: unique_temp_dir("credentials-door-files"),
        }
    }

    // An Unknown window is never pushed to, so the door is measured on a window whose
    // transition to idle has been observed.
    async fn window_with_parcel(&self, text: &str) -> String {
        let mut client = self.h.connect().await;
        let id = self.h.open_idle_window(&mut client).await;
        self.entry(&id).inbox().deposit(Parcel::new("a", text));
        id
    }

    fn entry(&self, window_id: &str) -> std::sync::Arc<yagura_daemon::WindowEntry> {
        self.h.registry.get(window_id).expect("window exists")
    }

    fn credentials_file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.files.join(name);
        std::fs::write(&path, contents).expect("write the credentials file");
        path
    }

    /// Runs `--credentials <path> --deliver-take` with neither window variable inherited,
    /// through a relay to this test's daemon.
    async fn take_with(&self, credentials: &Path) -> Output {
        let credentials = credentials.to_str().expect("a UTF-8 path");
        let (output, _) = run_door_without_credentials(
            &self.h,
            &["--credentials", credentials, "--deliver-take"],
            Vec::new(),
        )
        .await;
        output
    }
}

fn credentials_json(window_id: &str, token: &str) -> String {
    serde_json::json!({ "window_id": window_id, "token": token }).to_string()
}

#[tokio::test]
async fn a_door_without_window_variables_takes_its_windows_inbox_through_the_credentials_file() {
    let setup = Setup::start().await;
    let id = setup.window_with_parcel("PARCEL-ALPHA").await;
    let path = setup.credentials_file("credentials.json", &credentials_json(&id, VALID_TOKEN));

    let output = setup.take_with(&path).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PARCEL-ALPHA"),
        "the door must take the window's parcel through --credentials; stdout: {:?} \
         stderr: {stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(setup.entry(&id).inbox().pending_count(), 0);
}

#[tokio::test]
async fn a_door_whose_credentials_file_names_no_window_is_silent_and_leaves_the_inbox() {
    let setup = Setup::start().await;
    let id = setup.window_with_parcel("PARCEL-ALPHA").await;
    let cases = [
        ("a missing file", setup.files.join("absent.json")),
        (
            "a file that is not JSON",
            setup.credentials_file("garbled.json", "not json"),
        ),
        (
            "an empty window id",
            setup.credentials_file("empty-id.json", &credentials_json("", VALID_TOKEN)),
        ),
        (
            "an empty token",
            setup.credentials_file("empty-token.json", &credentials_json(&id, "")),
        ),
    ];
    for (situation, path) in cases {
        let output = setup.take_with(&path).await;
        assert!(
            output.status.success() && output.stdout.is_empty() && output.stderr.is_empty(),
            "{situation}: the door must print nothing and exit 0; code {:?} stdout {:?} \
             stderr {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        setup.entry(&id).inbox().pending_count(),
        1,
        "no door without a usable credentials file may take the window's parcel"
    );
}
