#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What the GUI's profile details read and write: every value a profile uses
//! with the layer it comes from, and the host's launch argv set or put back
//! to the shipped one. The library entry points are measured directly; the
//! IPC cases check that only the approver reaches them.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Harness, approver_base_config, expect_error, read_response, send};
use serde_json::json;
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::profile_command::{ProfileCommandError, set_profile_command};
use yagura_daemon::profile_detail::read_profile_detail;
use yagura_protocol::{ClientMessage, ProfileDetailEntry};
use yagura_test_support::paths::TempDir;

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn config(dir: PathBuf) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(dir),
        bundled_profiles: true,
        host_os: ProfileOs::Windows,
        program_is_executable: file_exists,
        ..DaemonConfig::default()
    }
}

fn entry<'a>(entries: &'a [ProfileDetailEntry], path: &[&str]) -> &'a ProfileDetailEntry {
    entries
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("no entry at {path:?}"))
}

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn user_file(dir: &Path, id: &str) -> toml::Table {
    toml::from_str(&std::fs::read_to_string(dir.join(format!("{id}.toml"))).unwrap()).unwrap()
}

#[test]
fn a_changed_alias_is_marked_with_the_shipped_value_beside_it() {
    let tree = TempDir::new("yagura-cli-test", "detail-alias");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "[model.aliases]\nstrong = \"my-model\"\n",
    )
    .unwrap();

    let detail = read_profile_detail(&config(dir), "codex-cli").unwrap();

    assert!(detail.shipped_base);
    let strong = entry(&detail.entries, &["model", "aliases", "strong"]);
    assert_eq!(strong.value, json!("my-model"));
    assert!(strong.changed);
    assert!(strong.shipped.is_some() && strong.shipped != Some(json!("my-model")));
    let fast = entry(&detail.entries, &["model", "aliases", "fast"]);
    assert!(!fast.changed);
    assert_eq!(fast.shipped.as_ref(), Some(&fast.value));
    assert_eq!(detail.command.argv, argv(&["codex"]));
    assert_eq!(detail.command.shipped, Some(argv(&["codex"])));
    assert!(!detail.command.changed);
}

#[test]
fn a_table_is_never_one_value_and_an_array_always_is() {
    let tree = TempDir::new("yagura-cli-test", "detail-leaves");
    let dir = tree.make_dir("profiles");

    let detail = read_profile_detail(&config(dir), "codex-cli").unwrap();

    assert!(detail.entries.iter().all(|entry| !entry.value.is_object()));
    assert_eq!(
        entry(&detail.entries, &["model", "args"]).value,
        json!(["-m", "{value}"])
    );
}

#[test]
fn a_profile_of_the_users_own_has_no_shipped_values() {
    let tree = TempDir::new("yagura-cli-test", "detail-own");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("my-tool.toml"),
        "schema = 1\ndisplay_name = \"My tool\"\n\n[command]\ndefault = [\"my-tool\"]\n\n[env]\nTOKEN = \"x\"\n",
    )
    .unwrap();

    let detail = read_profile_detail(&config(dir), "my-tool").unwrap();

    assert!(!detail.shipped_base);
    assert!(
        detail
            .entries
            .iter()
            .all(|entry| entry.shipped.is_none() && entry.changed)
    );
    assert_eq!(entry(&detail.entries, &["env", "TOKEN"]).value, json!("x"));
    assert_eq!(detail.command.shipped, None);
}

#[test]
fn an_array_of_tables_is_one_value_holding_each_table() {
    let tree = TempDir::new("yagura-cli-test", "detail-array-of-tables");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("my-tool.toml"),
        "schema = 1\ndisplay_name = \"My tool\"\n\n[command]\ndefault = [\"my-tool\"]\n\n[[notes]]\ntext = \"a\"\n\n[[notes]]\ntext = \"b\"\n",
    )
    .unwrap();

    let detail = read_profile_detail(&config(dir), "my-tool").unwrap();

    assert_eq!(
        entry(&detail.entries, &["notes"]).value,
        json!([{ "text": "a" }, { "text": "b" }])
    );
}

#[test]
fn setting_the_command_writes_this_hosts_key_and_putting_it_back_removes_it() {
    let tree = TempDir::new("yagura-cli-test", "detail-command");
    let dir = tree.make_dir("profiles").join("not-yet");
    let config = config(dir.clone());

    let set = set_profile_command(
        &config,
        "codex-cli",
        Some(&argv(&["C:\\tools\\codex.exe", "--a b"])),
    )
    .unwrap();

    assert_eq!(set.argv, argv(&["C:\\tools\\codex.exe", "--a b"]));
    assert!(set.changed);
    let expected: toml::Table =
        toml::from_str("[command]\nwindows = ['C:\\tools\\codex.exe', '--a b']\n").unwrap();
    assert_eq!(user_file(&dir, "codex-cli"), expected);

    let back = set_profile_command(&config, "codex-cli", None).unwrap();

    assert_eq!(back.argv, argv(&["codex"]));
    assert!(!back.changed);
    assert!(user_file(&dir, "codex-cli").is_empty());
    assert_eq!(
        read_profile_detail(&config, "codex-cli").unwrap().command,
        back
    );
}

#[test]
fn an_empty_command_or_an_empty_argument_is_refused_and_writes_nothing() {
    let tree = TempDir::new("yagura-cli-test", "detail-command-invalid");
    let dir = tree.make_dir("profiles");
    let config = config(dir.clone());

    for bad in [argv(&[]), argv(&["codex", ""])] {
        let err = set_profile_command(&config, "codex-cli", Some(&bad)).unwrap_err();
        assert!(
            matches!(err, ProfileCommandError::Refused(_)),
            "{bad:?}: {err:?}"
        );
    }
    assert!(!dir.join("codex-cli.toml").exists());
}

#[tokio::test]
async fn reading_details_or_setting_a_command_is_refused_to_a_connection_that_is_not_the_approver()
{
    let tree = TempDir::new("yagura-cli-test", "detail-ipc");
    let dir = tree.make_dir("profiles");
    let config = DaemonConfig {
        profiles_dir: Some(dir.clone()),
        bundled_profiles: true,
        ..approver_base_config()
    };
    let h = Harness::start("profile-detail-auth", config).await;
    let mut client = h.connect().await;

    for message in [
        ClientMessage::GetProfileDetail {
            profile: "codex-cli".to_string(),
        },
        ClientMessage::SetProfileCommand {
            profile: "codex-cli".to_string(),
            argv: Some(argv(&["x"])),
        },
    ] {
        send(&mut client, &message).await;
        expect_error(
            read_response(&mut client, Duration::from_secs(5)).await,
            "auth_failed",
        );
    }
    assert!(!dir.join("codex-cli.toml").exists());
}

#[test]
fn setting_the_command_keeps_every_other_value_of_an_existing_file_and_its_crlf_endings() {
    let tree = TempDir::new("yagura-cli-test", "detail-command-existing");
    let dir = tree.make_dir("profiles");
    let before = "# kept by hand\r\nenabled = false\r\n\r\n[command]\r\nmacos = [\"codex-mac\"]  # other OS\r\n\r\n[model.aliases]\r\nstrong = \"my-model\"\r\n";
    std::fs::write(dir.join("codex-cli.toml"), before).unwrap();
    let config = config(dir.clone());

    set_profile_command(&config, "codex-cli", Some(&argv(&["codex", "--x"]))).unwrap();

    let after = std::fs::read_to_string(dir.join("codex-cli.toml")).unwrap();
    assert!(after.contains("# kept by hand\r\n"), "{after:?}");
    assert!(
        !after.replace("\r\n", "").contains('\n'),
        "every line end stays CRLF: {after:?}"
    );
    let file = user_file(&dir, "codex-cli");
    assert_eq!(file["enabled"].as_bool(), Some(false));
    assert_eq!(
        file["model"]["aliases"]["strong"].as_str(),
        Some("my-model")
    );
    assert_eq!(file["command"]["macos"].as_array().unwrap().len(), 1);
    assert_eq!(file["command"]["windows"].as_array().unwrap().len(), 2);
}

#[test]
fn putting_the_command_back_keeps_every_other_value_of_the_file() {
    let tree = TempDir::new("yagura-cli-test", "detail-command-reset-existing");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "enabled = false\n\n[command]\nwindows = [\"mine\"]\n\n[model.aliases]\nstrong = \"my-model\"\n",
    )
    .unwrap();
    let config = config(dir.clone());

    set_profile_command(&config, "codex-cli", None).unwrap();

    let expected: toml::Table =
        toml::from_str("enabled = false\n\n[model.aliases]\nstrong = \"my-model\"\n").unwrap();
    assert_eq!(user_file(&dir, "codex-cli"), expected);
}
