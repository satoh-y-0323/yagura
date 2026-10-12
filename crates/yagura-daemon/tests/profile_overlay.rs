#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! The shipped profiles are the base layer, and a file under the profiles
//! directory carries only what the user changed. Every case reads through
//! `list_profiles` / `resolve_launch` and writes through `save_model_aliases` /
//! `set_profile_enabled`, the same entry points the daemon uses.

mod common;

use std::path::{Path, PathBuf};

use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{LaunchOptionValues, ProfileOs, list_profiles, resolve_launch};
use yagura_daemon::profile_aliases::save_model_aliases;
use yagura_daemon::profile_enabled::set_profile_enabled;
use yagura_protocol::LaunchOptionAlias;
use yagura_test_support::paths::TempDir;

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn config(dir: PathBuf, bundled_profiles: bool) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(dir),
        bundled_profiles,
        host_os: ProfileOs::Windows,
        program_is_executable: file_exists,
        ..DaemonConfig::default()
    }
}

fn aliases_of(config: &DaemonConfig, id: &str) -> Vec<(String, String)> {
    let listing = list_profiles(config);
    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| panic!("{id} must be listed: {:?}", listing.warnings));
    entry
        .model
        .as_ref()
        .expect("the profile maps a model")
        .aliases
        .iter()
        .map(|LaunchOptionAlias { alias, value }| (alias.clone(), value.clone()))
        .collect()
}

#[test]
fn an_empty_directory_lists_every_shipped_profile_for_the_host() {
    let tree = TempDir::new("yagura-cli-test", "overlay-empty");
    let dir = tree.make_dir("profiles");
    let listing = list_profiles(&config(dir.clone(), true));
    let ids: Vec<&str> = listing
        .profiles
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    for id in ["claude-code", "codex-cli", "antigravity-cli", "shell"] {
        assert!(ids.contains(&id), "{id} must be listed, got {ids:?}");
    }
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
}

#[test]
fn a_directory_that_does_not_exist_yet_still_lists_the_shipped_profiles() {
    let tree = TempDir::new("yagura-cli-test", "overlay-absent");
    let dir = tree.make_dir("profiles");
    let listing = list_profiles(&config(dir.join("not-yet"), true));
    assert!(listing.profiles.iter().any(|entry| entry.id == "codex-cli"));
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
}

#[test]
fn a_changed_alias_overrides_only_that_value_and_keeps_the_shipped_order() {
    let tree = TempDir::new("yagura-cli-test", "overlay-alias");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "[model.aliases]\nstrong = \"my-model\"\n",
    )
    .unwrap();
    let aliases = aliases_of(&config(dir.clone(), true), "codex-cli");
    let names: Vec<&str> = aliases.iter().map(|(alias, _)| alias.as_str()).collect();
    assert_eq!(names, ["fast", "default", "strong"]);
    assert_eq!(aliases[2].1, "my-model");
    assert_eq!(
        aliases[0].1, "gpt-5.6-luna",
        "an alias the user left alone stays shipped"
    );
}

#[test]
fn a_shipped_table_the_user_file_does_not_carry_still_takes_effect() {
    let tree = TempDir::new("yagura-cli-test", "overlay-hooks");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "[model.aliases]\nstrong = \"my-model\"\n",
    )
    .unwrap();
    let launch = resolve_launch(
        &config(dir.clone(), true),
        "codex-cli",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .unwrap();
    assert!(
        launch.hooks.is_some(),
        "the shipped [hooks] table must reach the launch"
    );
    assert!(
        launch.mcp.is_some(),
        "the shipped [mcp] table must reach the launch"
    );
}

#[test]
fn a_profile_the_user_switched_off_is_listed_disabled() {
    let tree = TempDir::new("yagura-cli-test", "overlay-disabled");
    let dir = tree.make_dir("profiles");
    std::fs::write(dir.join("claude-code.toml"), "enabled = false\n").unwrap();
    let listing = list_profiles(&config(dir.clone(), true));
    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "claude-code")
        .expect("listed");
    assert!(!entry.enabled);
}

#[test]
fn a_profile_with_no_shipped_counterpart_is_read_from_its_file_alone() {
    let tree = TempDir::new("yagura-cli-test", "overlay-custom");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("my-tool.toml"),
        "schema = 1\ndisplay_name = \"My tool\"\n\n[command]\ndefault = [\"my-tool\"]\n",
    )
    .unwrap();
    let listing = list_profiles(&config(dir.clone(), true));
    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "my-tool")
        .expect("listed");
    assert_eq!(entry.display_name, "My tool");
    assert_eq!(entry.command, ["my-tool"]);
}

#[test]
fn a_user_file_that_breaks_the_merged_profile_is_reported_not_replaced_by_the_shipped_one() {
    let tree = TempDir::new("yagura-cli-test", "overlay-broken");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "[model]\nargs = \"not-an-array\"\n",
    )
    .unwrap();
    let listing = list_profiles(&config(dir.clone(), true));
    assert!(!listing.profiles.iter().any(|entry| entry.id == "codex-cli"));
    assert!(
        listing
            .warnings
            .iter()
            .any(|warning| warning.contains("codex-cli")),
        "{:?}",
        listing.warnings
    );
}

#[test]
fn an_explicit_directory_answers_alone() {
    let tree = TempDir::new("yagura-cli-test", "overlay-explicit");
    let dir = tree.make_dir("profiles");
    let listing = list_profiles(&config(dir.clone(), false));
    assert!(listing.profiles.is_empty(), "{:?}", listing.profiles);
}

fn user_file(dir: &Path, id: &str) -> toml::Table {
    let text = std::fs::read_to_string(dir.join(format!("{id}.toml"))).unwrap();
    toml::from_str(&text).unwrap()
}

fn with_strong(aliases: &[(String, String)], strong: &str) -> Vec<(String, String)> {
    aliases
        .iter()
        .map(|(alias, value)| {
            let value = if alias == "strong" { strong } else { value };
            (alias.clone(), value.to_string())
        })
        .collect()
}

#[test]
fn saving_an_alias_writes_only_that_alias_into_a_new_user_file() {
    let tree = TempDir::new("yagura-cli-test", "overlay-save-new");
    let dir = tree.make_dir("profiles").join("not-yet");
    let config = config(dir.clone(), true);
    let shipped = aliases_of(&config, "codex-cli");

    save_model_aliases(&config, "codex-cli", &with_strong(&shipped, "my-model")).unwrap();

    let expected: toml::Table = toml::from_str(
        "[model.aliases]
strong = \"my-model\"
",
    )
    .unwrap();
    assert_eq!(user_file(&dir, "codex-cli"), expected);
    assert_eq!(
        aliases_of(&config, "codex-cli"),
        with_strong(&shipped, "my-model")
    );
}

#[test]
fn saving_an_alias_back_to_its_shipped_value_removes_it_from_the_user_file() {
    let tree = TempDir::new("yagura-cli-test", "overlay-save-revert");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "enabled = false

[model.aliases]
strong = \"my-model\"
",
    )
    .unwrap();
    let config = config(dir.clone(), true);
    let changed = aliases_of(&config, "codex-cli");
    let shipped_strong = {
        let empty = TempDir::new("yagura-cli-test", "overlay-save-revert-shipped");
        let alone = aliases_of(&self::config(empty.make_dir("profiles"), true), "codex-cli");
        alone
            .into_iter()
            .find(|(alias, _)| alias == "strong")
            .unwrap()
            .1
    };

    save_model_aliases(
        &config,
        "codex-cli",
        &with_strong(&changed, &shipped_strong),
    )
    .unwrap();

    let expected: toml::Table = toml::from_str(
        "enabled = false
",
    )
    .unwrap();
    assert_eq!(
        user_file(&dir, "codex-cli"),
        expected,
        "the reverted alias leaves no trace, and the user's other change stays"
    );
}

#[test]
fn switching_a_shipped_profile_off_and_on_touches_only_the_enabled_key() {
    let tree = TempDir::new("yagura-cli-test", "overlay-enabled");
    let dir = tree.make_dir("profiles");
    let config = config(dir.clone(), true);

    set_profile_enabled(&config, "claude-code", false).unwrap();
    let expected: toml::Table = toml::from_str(
        "enabled = false
",
    )
    .unwrap();
    assert_eq!(user_file(&dir, "claude-code"), expected);

    set_profile_enabled(&config, "claude-code", true).unwrap();
    assert!(user_file(&dir, "claude-code").is_empty());
    let listing = list_profiles(&config);
    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "claude-code")
        .expect("listed");
    assert!(entry.enabled);
}

#[test]
fn saving_an_alias_into_a_user_file_whose_model_table_is_inline_keeps_it() {
    let tree = TempDir::new("yagura-cli-test", "overlay-save-inline");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "model = { args = [\"-m\", \"{value}\"] }\n",
    )
    .unwrap();
    let config = config(dir.clone(), true);
    let current = aliases_of(&config, "codex-cli");

    save_model_aliases(&config, "codex-cli", &with_strong(&current, "my-model")).unwrap();

    assert_eq!(
        aliases_of(&config, "codex-cli"),
        with_strong(&current, "my-model")
    );
    let file = user_file(&dir, "codex-cli");
    assert_eq!(
        file["model"]["aliases"]["strong"].as_str(),
        Some("my-model")
    );
    assert!(
        file["model"].get("args").is_some(),
        "the user's own key stays"
    );
}

#[test]
fn saving_an_alias_into_a_user_file_whose_model_table_is_dotted_keeps_it() {
    let tree = TempDir::new("yagura-cli-test", "overlay-save-dotted");
    let dir = tree.make_dir("profiles");
    std::fs::write(
        dir.join("codex-cli.toml"),
        "model.args = [\"-m\", \"{value}\"]\n",
    )
    .unwrap();
    let config = config(dir.clone(), true);
    let current = aliases_of(&config, "codex-cli");

    save_model_aliases(&config, "codex-cli", &with_strong(&current, "my-model")).unwrap();

    assert_eq!(
        aliases_of(&config, "codex-cli"),
        with_strong(&current, "my-model")
    );
    let file = user_file(&dir, "codex-cli");
    assert_eq!(
        file["model"]["aliases"]["strong"].as_str(),
        Some("my-model")
    );
}

#[test]
fn a_profiles_directory_that_cannot_be_read_lists_no_shipped_profile_in_its_place() {
    let tree = TempDir::new("yagura-cli-test", "overlay-unreadable");
    let not_a_dir = tree.write_file("profiles", "");
    let config = config(not_a_dir, true);

    let listing = list_profiles(&config);

    assert!(
        listing.profiles.is_empty(),
        "without the user's layer a shipped profile the user switched off would be offered: {:?}",
        listing
            .profiles
            .iter()
            .map(|entry| &entry.id)
            .collect::<Vec<_>>()
    );
    assert!(!listing.warnings.is_empty());
}
