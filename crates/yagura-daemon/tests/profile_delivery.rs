#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: a launch profile's `[delivery]` table -- how the daemon hands text to
//! that profile's CLI without typing it into the composer.
//!
//! The table is read like every other profile table: an unknown key, a value of the wrong
//! type or outside its domain, and a template slot nothing fills each take the whole
//! profile out of the listing with one warning. The profiles this repository ships are read
//! as data and measured through what `parse_profile` returns, so a shipped table that stops
//! parsing, or one that drifts from what the CLI needs, fails here.
//!
//! Availability is decided against a lookup handed in through the configuration, never
//! against the machine running the suite, and the operating system is a value in the
//! configuration: the Windows and the Unix rules are both measured on whichever host runs
//! this binary.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::docs::read_doc_verbatim;
use common::{Harness, approver_base_config, write_profile};
use yagura_daemon::DaemonConfig;
use yagura_daemon::OpenWindowRequest;
use yagura_daemon::inbox::{DEFAULT_MAX_CHARS, DEFAULT_WAKE_EXIT, OutputForm};
use yagura_daemon::profile::{
    LaunchOptionValues, Profile, ProfileOs, list_profiles, parse_profile, resolve_launch,
    unavailable_reason,
};
use yagura_daemon::profile_delivery::{ProfileDelivery, WakeMode, launch_prompt_injection};
use yagura_daemon::window::{CliInjection, InjectionKind, WindowProvenance};
use yagura_os::program::ProgramLookup;
use yagura_protocol::DeliveryStream;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::argv_probe_path;

// --- fixtures ---------------------------------------------------------------

/// A profile document with `before_command` (top-level keys and tables) placed ahead of its
/// `[command]` table, so a bare `key = value` fragment stays top-level.
fn source_with(before_command: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = \"Delivery\"\n\n{before_command}\n[command]\ndefault = \
         [\"some-program\"]\n"
    )
}

fn parse_with(before_command: &str) -> Result<Profile, yagura_daemon::profile::ProfileParseError> {
    parse_profile("delivery", &source_with(before_command))
}

fn delivery_of(before_command: &str) -> ProfileDelivery {
    parse_with(before_command)
        .unwrap_or_else(|err| panic!("{before_command:?} should be accepted, got {err:?}"))
        .delivery
        .unwrap_or_else(|| panic!("{before_command:?} declares a [delivery] table"))
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn shipped(id: &str) -> Profile {
    let source = read_doc_verbatim(&format!("../../profiles/{id}.toml"));
    parse_profile(id, &source)
        .unwrap_or_else(|err| panic!("shipped profile {id} must parse as a valid profile: {err}"))
}

fn shipped_delivery(id: &str) -> ProfileDelivery {
    shipped(id)
        .delivery
        .unwrap_or_else(|| panic!("shipped profile {id} must declare a [delivery] table"))
}

// =============================================================================
// P1: reading the table
// =============================================================================

#[test]
fn a_profile_without_a_delivery_table_declares_no_delivery() {
    assert!(parse_with("").unwrap().delivery.is_none());
}

#[test]
fn an_empty_delivery_table_takes_the_documented_defaults() {
    let delivery = delivery_of("[delivery]\n");

    assert_eq!(delivery.output, OutputForm::Plain);
    assert_eq!(delivery.max_chars, DEFAULT_MAX_CHARS);
    assert_eq!(delivery.wake, WakeMode::None);
    assert_eq!(delivery.wake_stream, DeliveryStream::Stderr);
    assert_eq!(delivery.wake_exit, DEFAULT_WAKE_EXIT);
    assert_eq!(delivery.wake_timeout_secs, None);
    assert_eq!(delivery.session_pointer, None);
    assert_eq!(delivery.launch_prompt, None);
    assert_eq!(delivery.shim_guidance, None);
}

#[test]
fn every_key_of_the_table_is_read_with_the_value_it_was_given() {
    let delivery = delivery_of(
        "[delivery]\n\
         output = '{\"v\":{text}}'\n\
         max_chars = 777\n\
         wake = \"command\"\n\
         wake_args = [\"queue\", \"--thread\", \"{session}\", \"--message\", \"{text}\"]\n\
         wake_stream = \"stdout\"\n\
         wake_exit = 3\n\
         wake_timeout_secs = 120\n\
         session_pointer = \"/a/b\"\n\
         launch_prompt = [\"-i\", \"{prompt}\"]\n\
         shim_guidance = \"Install the standalone build.\"\n",
    );

    assert_eq!(
        delivery.output,
        OutputForm::Template("{\"v\":{text}}".into())
    );
    assert_eq!(delivery.max_chars, 777);
    assert_eq!(
        delivery.wake,
        WakeMode::Command(strings(&[
            "queue",
            "--thread",
            "{session}",
            "--message",
            "{text}"
        ]))
    );
    assert_eq!(delivery.wake_stream, DeliveryStream::Stdout);
    assert_eq!(delivery.wake_exit, 3);
    assert_eq!(delivery.wake_timeout_secs, Some(120));
    assert_eq!(delivery.session_pointer.as_deref(), Some("/a/b"));
    assert_eq!(delivery.launch_prompt, Some(strings(&["-i", "{prompt}"])));
    assert_eq!(
        delivery.shim_guidance.as_deref(),
        Some("Install the standalone build.")
    );
}

#[test]
fn the_three_ways_to_wake_a_waiting_cli_are_all_readable() {
    assert_eq!(
        delivery_of("[delivery]\nwake = \"none\"\n").wake,
        WakeMode::None
    );
    assert_eq!(
        delivery_of("[delivery]\nwake = \"wait\"\n").wake,
        WakeMode::Wait
    );
    assert_eq!(
        delivery_of("[delivery]\nwake = \"command\"\nwake_args = [\"go\", \"{text}\"]\n").wake,
        WakeMode::Command(strings(&["go", "{text}"]))
    );
}

#[test]
fn the_edges_of_each_value_domain_are_accepted() {
    for (what, fragment) in [
        ("max_chars of one", "[delivery]\nmax_chars = 1\n"),
        ("wake_exit of zero", "[delivery]\nwake_exit = 0\n"),
        ("wake_exit of 255", "[delivery]\nwake_exit = 255\n"),
        (
            "wake_timeout_secs of one",
            "[delivery]\nwake_timeout_secs = 1\n",
        ),
        ("the plain output form", "[delivery]\noutput = \"plain\"\n"),
        (
            "a template with the text twice",
            "[delivery]\noutput = '{\"a\":{text},\"b\":{text}}'\n",
        ),
        (
            "stderr as the wake stream",
            "[delivery]\nwake_stream = \"stderr\"\n",
        ),
        (
            "a nested pointer",
            "[delivery]\nsession_pointer = \"/a/0/b\"\n",
        ),
    ] {
        parse_with(fragment)
            .unwrap_or_else(|err| panic!("{what}: should be accepted, got {err:?}"));
    }
}

#[test]
fn a_table_that_breaks_a_rule_takes_its_profile_out_of_the_reading() {
    let cases: [(&str, &str); 27] = [
        ("an unknown key", "[delivery]\nbogus = 1\n"),
        ("a delivery that is not a table", "delivery = 1\n"),
        ("max_chars as a string", "[delivery]\nmax_chars = \"10\"\n"),
        ("max_chars of zero", "[delivery]\nmax_chars = 0\n"),
        ("a negative max_chars", "[delivery]\nmax_chars = -5\n"),
        (
            "an output that is neither plain nor a template",
            "[delivery]\noutput = \"xml\"\n",
        ),
        (
            "a template with a slot nothing fills",
            "[delivery]\noutput = '{\"a\":{text},\"b\":{session}}'\n",
        ),
        (
            "a template that is not JSON once the text is in",
            "[delivery]\noutput = '{\"a\":{text}'\n",
        ),
        ("an unknown wake mode", "[delivery]\nwake = \"poll\"\n"),
        ("wake as a number", "[delivery]\nwake = 1\n"),
        (
            "wake command without its arguments",
            "[delivery]\nwake = \"command\"\n",
        ),
        (
            "wake arguments without the command mode",
            "[delivery]\nwake = \"wait\"\nwake_args = [\"go\", \"{text}\"]\n",
        ),
        (
            "a wake argument slot nothing fills",
            "[delivery]\nwake = \"command\"\nwake_args = [\"go\", \"{nope}\", \"{text}\"]\n",
        ),
        (
            "wake arguments that never carry the text",
            "[delivery]\nwake = \"command\"\nwake_args = [\"go\", \"{session}\"]\n",
        ),
        (
            "an unknown wake stream",
            "[delivery]\nwake_stream = \"stdin\"\n",
        ),
        ("a wake exit above 255", "[delivery]\nwake_exit = 256\n"),
        ("a negative wake exit", "[delivery]\nwake_exit = -1\n"),
        ("a wake exit as a string", "[delivery]\nwake_exit = \"2\"\n"),
        (
            "a wake timeout of zero",
            "[delivery]\nwake_timeout_secs = 0\n",
        ),
        (
            "a negative wake timeout",
            "[delivery]\nwake_timeout_secs = -1\n",
        ),
        (
            "a wake timeout as a string",
            "[delivery]\nwake_timeout_secs = \"10\"\n",
        ),
        (
            "a session pointer that is not a JSON pointer",
            "[delivery]\nsession_pointer = \"session_id\"\n",
        ),
        (
            "a launch prompt that is not a list",
            "[delivery]\nlaunch_prompt = \"x\"\n",
        ),
        (
            "a launch prompt slot nothing fills",
            "[delivery]\nlaunch_prompt = [\"--\", \"{nope}\"]\n",
        ),
        (
            "a launch prompt that never carries the prompt",
            "[delivery]\nlaunch_prompt = [\"--\"]\n",
        ),
        (
            "a launch prompt with an empty element",
            "[delivery]\nlaunch_prompt = [\"\", \"{prompt}\"]\n",
        ),
        (
            "a shim guidance that is not a string",
            "[delivery]\nshim_guidance = 5\n",
        ),
    ];
    for (what, fragment) in cases {
        assert!(
            parse_with(fragment).is_err(),
            "{what}: the profile must not be accepted"
        );
    }
}

#[test]
fn an_invalid_delivery_table_leaves_its_profile_out_of_the_listing_with_one_warning() {
    let tree = TempDir::new("yagura-cli-test", "delivery-invalid-listing");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "delivery-ok",
        &source_with("[delivery]\nmax_chars = 100\n"),
    );
    let invalid = [
        ("delivery-unknown-key", "[delivery]\nbogus = 1\n"),
        ("delivery-bad-range", "[delivery]\nmax_chars = 0\n"),
        (
            "delivery-unknown-slot",
            "[delivery]\nlaunch_prompt = [\"{nope}\"]\n",
        ),
        (
            "delivery-secret",
            "[delivery]\nshim_guidance = \"SECRET-TOKEN-XYZ\"\nbogus = 1\n",
        ),
    ];
    for (id, fragment) in invalid {
        write_profile(&profiles_dir, id, &source_with(fragment));
    }
    let config = config_with(Some(profiles_dir), lookup_nothing(), ProfileOs::host());

    let listing = list_profiles(&config);

    assert!(
        listing
            .profiles
            .iter()
            .any(|entry| entry.id == "delivery-ok"),
        "a valid table leaves its profile listed: {:?}",
        listing.profiles
    );
    for (id, _) in invalid {
        assert!(
            !listing.profiles.iter().any(|entry| entry.id == id),
            "{id} must be left out of the listing"
        );
        let naming: Vec<_> = listing.warnings.iter().filter(|w| w.contains(id)).collect();
        assert_eq!(
            naming.len(),
            1,
            "{id} must produce one warning: {:?}",
            listing.warnings
        );
    }
    assert!(
        listing
            .warnings
            .iter()
            .all(|w| !w.contains("SECRET-TOKEN-XYZ")),
        "a warning never quotes the document: {:?}",
        listing.warnings
    );
}

// =============================================================================
// P2: the profiles this repository ships
// =============================================================================

const AGY_OUTPUT: &str = r#"{"injectSteps":[{"systemMessage":{"systemMessage":{text}}}]}"#;

#[test]
fn the_claude_code_profiles_deliver_plain_text_and_wake_a_waiting_cli_through_a_wait() {
    for id in ["claude-code", "wsl-claude-code"] {
        let delivery = shipped_delivery(id);
        assert_eq!(delivery.output, OutputForm::Plain, "{id}");
        assert_eq!(delivery.max_chars, 10_000, "{id}");
        assert_eq!(delivery.wake, WakeMode::Wait, "{id}");
        assert_eq!(delivery.wake_stream, DeliveryStream::Stderr, "{id}");
        assert_eq!(delivery.wake_exit, 2, "{id}");
        assert_eq!(delivery.wake_timeout_secs, Some(86_400), "{id}");
        assert_eq!(delivery.session_pointer, None, "{id}");
        assert_eq!(
            delivery.launch_prompt,
            Some(strings(&["--", "{prompt}"])),
            "{id}: the prompt follows `--` because the MCP config flag takes several values"
        );
    }
    assert_eq!(shipped_delivery("claude-code").shim_guidance, None);
}

#[test]
fn the_codex_profiles_wake_a_waiting_cli_by_queueing_a_message_on_its_session() {
    for id in ["codex-cli", "wsl-codex-cli"] {
        let delivery = shipped_delivery(id);
        assert_eq!(delivery.output, OutputForm::Plain, "{id}");
        assert_eq!(delivery.max_chars, 10_000, "{id}");
        assert_eq!(
            delivery.wake,
            WakeMode::Command(strings(&[
                "queue",
                "--thread",
                "{session}",
                "--message",
                "{text}"
            ])),
            "{id}"
        );
        assert_eq!(
            delivery.session_pointer.as_deref(),
            Some("/session_id"),
            "{id}: the hook's stdin names the session"
        );
        assert_eq!(delivery.launch_prompt, Some(strings(&["{prompt}"])), "{id}");
    }
}

#[test]
fn the_host_codex_profile_says_how_to_get_a_build_that_is_not_a_windows_shim() {
    let guidance = shipped_delivery("codex-cli")
        .shim_guidance
        .expect("the profile that cannot work through a shim carries guidance");
    assert!(
        guidance.contains("standalone") && guidance.contains("install.ps1"),
        "{guidance:?}"
    );
    assert!(guidance.contains("https://"), "{guidance:?}");
}

#[test]
fn the_antigravity_profiles_inject_text_as_a_json_system_message_and_have_no_way_to_wake() {
    for id in ["antigravity-cli", "wsl-antigravity-cli"] {
        let delivery = shipped_delivery(id);
        assert_eq!(
            delivery.output,
            OutputForm::Template(AGY_OUTPUT.into()),
            "{id}"
        );
        assert_eq!(delivery.max_chars, 10_000, "{id}");
        assert_eq!(delivery.wake, WakeMode::None, "{id}");
        assert_eq!(delivery.session_pointer, None, "{id}");
        assert_eq!(
            delivery.launch_prompt,
            Some(strings(&["-i", "{prompt}"])),
            "{id}"
        );
        assert_eq!(delivery.shim_guidance, None, "{id}");
    }
}

#[test]
fn the_shell_profile_declares_no_delivery() {
    assert!(shipped("shell").delivery.is_none());
}

#[test]
fn the_limits_the_hooks_carry_are_the_profiles_own_values() {
    // The hook files are measured on their own (`profile_hooks.rs`,
    // `profile_hooks_codex.rs`) with these literals; here they are tied to the table.
    for id in [
        "codex-cli",
        "wsl-codex-cli",
        "claude-code",
        "wsl-claude-code",
    ] {
        assert_eq!(shipped_delivery(id).max_chars, 10_000, "{id}");
    }
    for id in ["claude-code", "wsl-claude-code"] {
        assert_eq!(shipped_delivery(id).wake_timeout_secs, Some(86_400), "{id}");
    }
}

// =============================================================================
// P4: availability
// =============================================================================

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn lookup_nothing() -> ProgramLookup {
    ProgramLookup {
        path_dirs: Vec::new(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: None,
    }
}

fn lookup_in(dir: &Path) -> ProgramLookup {
    ProgramLookup {
        path_dirs: vec![dir.to_path_buf()],
        extra_bin_dirs: Vec::new(),
        windows_comspec: Some(PathBuf::from("cmd.exe")),
    }
}

fn config_with(
    profiles_dir: Option<PathBuf>,
    lookup: ProgramLookup,
    host_os: ProfileOs,
) -> DaemonConfig {
    DaemonConfig {
        profiles_dir,
        host_os,
        program_lookup: lookup,
        program_is_executable: file_exists,
        default_shell: None,
        detection_dir: None,
        ..DaemonConfig::default()
    }
}

const WAKE_COMMAND: &str = "wake = \"command\"\nwake_args = [\"queue\", \"{text}\"]\n";

fn profile_running(program: &str, delivery_lines: &str) -> Profile {
    parse_profile(
        "shimmed",
        &format!(
            "schema = 1\ndisplay_name = \"Shimmed\"\n\n[command]\ndefault = [\"{program}\"]\n\n\
             [delivery]\n{delivery_lines}"
        ),
    )
    .expect("the profile is valid")
}

/// The reason with `guidance` taken off its end: the fixed sentence the core owns.
fn fixed_sentence(reason: &str, guidance: &str) -> String {
    reason
        .strip_suffix(guidance)
        .unwrap_or_else(|| panic!("{reason:?} must end with the profile's guidance {guidance:?}"))
        .trim_end()
        .to_string()
}

#[test]
fn a_command_wake_profile_that_resolves_to_a_windows_shim_is_unavailable() {
    let tree = TempDir::new("yagura-cli-test", "delivery-shim-unavailable");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shimmed.cmd", "");
    tree.write_file("bin/batched.bat", "");
    let config = config_with(None, lookup_in(&bin), ProfileOs::Windows);

    for program in ["shimmed", "batched"] {
        let profile = profile_running(program, WAKE_COMMAND);
        let reason = unavailable_reason(&profile, &config);
        assert!(
            reason.is_some(),
            "{program}: a message cannot be queued through a shim, so the profile is unusable"
        );
    }
}

#[test]
fn the_reason_is_a_fixed_sentence_followed_by_the_profiles_own_guidance() {
    let tree = TempDir::new("yagura-cli-test", "delivery-shim-reason");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shimmed.cmd", "");
    let config = config_with(None, lookup_in(&bin), ProfileOs::Windows);
    let guidance_a = "Guidance A: install the other build.";
    let guidance_b = "Guidance B, a different text.";

    let reason_a = unavailable_reason(
        &profile_running(
            "shimmed",
            &format!("{WAKE_COMMAND}shim_guidance = \"{guidance_a}\"\n"),
        ),
        &config,
    )
    .expect("unavailable");
    let reason_b = unavailable_reason(
        &profile_running(
            "shimmed",
            &format!("{WAKE_COMMAND}shim_guidance = \"{guidance_b}\"\n"),
        ),
        &config,
    )
    .expect("unavailable");
    let reason_bare = unavailable_reason(&profile_running("shimmed", WAKE_COMMAND), &config)
        .expect("unavailable");

    let sentence = fixed_sentence(&reason_a, guidance_a);
    assert_eq!(
        sentence,
        fixed_sentence(&reason_b, guidance_b),
        "the core's sentence is the same whatever the profile adds"
    );
    assert_eq!(
        reason_bare.trim_end(),
        sentence,
        "without guidance the reason is the sentence alone"
    );
    assert!(
        sentence.contains(".cmd") && sentence.contains(".bat") && sentence.contains("Windows"),
        "the sentence says what cannot be used: {sentence:?}"
    );
    let lowered = sentence.to_lowercase();
    for foreign in ["http", "codex", "claude", "npm", "install"] {
        assert!(
            !lowered.contains(foreign),
            "the core's sentence names no vendor, URL or remedy ({foreign}): {sentence:?}"
        );
    }
}

#[test]
fn the_shim_reason_is_listed_beside_the_entry_and_matches_the_availability_flag() {
    let tree = TempDir::new("yagura-cli-test", "delivery-shim-listing");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shimmed.cmd", "");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "shimmed",
        &format!(
            "schema = 1\ndisplay_name = \"Shimmed\"\n\n[command]\ndefault = [\"shimmed\"]\n\n\
             [delivery]\n{WAKE_COMMAND}shim_guidance = \"Use the native build.\"\n"
        ),
    );
    let config = config_with(Some(profiles_dir), lookup_in(&bin), ProfileOs::Windows);

    let listing = list_profiles(&config);

    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "shimmed")
        .expect("the profile is listed");
    assert!(!entry.available);
    let reason = entry.unavailable_reason.as_deref().expect("a reason");
    assert!(reason.ends_with("Use the native build."), "{reason:?}");
}

#[test]
fn a_profile_that_only_declares_a_launch_prompt_stays_usable_through_a_shim() {
    let tree = TempDir::new("yagura-cli-test", "delivery-shim-launch-prompt");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shimmed.cmd", "");
    let config = config_with(None, lookup_in(&bin), ProfileOs::Windows);

    for lines in [
        "launch_prompt = [\"{prompt}\"]\n",
        "wake = \"wait\"\n",
        "wake = \"none\"\n",
        "",
    ] {
        assert_eq!(
            unavailable_reason(&profile_running("shimmed", lines), &config),
            None,
            "{lines:?}: only a wake by command needs a program that is not a shim"
        );
    }
}

#[test]
fn a_profile_without_a_delivery_table_is_unaffected_by_the_shim_rule() {
    let tree = TempDir::new("yagura-cli-test", "delivery-shim-no-table");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shimmed.cmd", "");
    let config = config_with(None, lookup_in(&bin), ProfileOs::Windows);
    let profile = parse_profile(
        "plain",
        "schema = 1\ndisplay_name = \"Plain\"\n\n[command]\ndefault = [\"shimmed\"]\n",
    )
    .unwrap();

    assert_eq!(unavailable_reason(&profile, &config), None);
}

#[test]
fn a_command_wake_profile_that_resolves_to_a_native_program_is_usable() {
    let tree = TempDir::new("yagura-cli-test", "delivery-native");
    let windows_bin = tree.make_dir("winbin");
    tree.write_file("winbin/native.exe", "");
    let unix_bin = tree.make_dir("unixbin");
    tree.write_file("unixbin/native", "");
    let profile = profile_running("native", WAKE_COMMAND);

    assert_eq!(
        unavailable_reason(
            &profile,
            &config_with(None, lookup_in(&windows_bin), ProfileOs::Windows)
        ),
        None,
        "an .exe starts as it is"
    );
    for os in [ProfileOs::Linux, ProfileOs::MacOs] {
        assert_eq!(
            unavailable_reason(&profile, &config_with(None, lookup_in(&unix_bin), os)),
            None,
            "{os:?}: nothing is a shim off Windows"
        );
    }
}

#[test]
fn a_program_that_is_not_found_keeps_the_not_found_reason_whatever_the_delivery_says() {
    let tree = TempDir::new("yagura-cli-test", "delivery-not-found");
    let bin = tree.make_dir("bin");
    let with_wake = profile_running("missing", WAKE_COMMAND);
    let without = profile_running("missing", "");
    let config = config_with(None, lookup_in(&bin), ProfileOs::Windows);

    let reason = unavailable_reason(&with_wake, &config).expect("not found");
    assert_eq!(Some(reason), unavailable_reason(&without, &config));
}

// =============================================================================
// P5: the launch prompt is an argument of the spawn only
// =============================================================================

#[test]
fn a_launch_carries_the_profiles_delivery_and_its_argv_never_holds_the_prompt_template() {
    let tree = TempDir::new("yagura-cli-test", "delivery-launch-argv");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "prompted",
        "schema = 1\ndisplay_name = \"Prompted\"\n\n[command]\ndefault = [\"prog\", \"--own\"]\n\n\
         [model]\nargs = [\"--model\", \"{value}\"]\n\n[delivery]\nlaunch_prompt = [\"--\", \"{prompt}\"]\n",
    );
    let config = config_with(Some(profiles_dir), lookup_nothing(), ProfileOs::host());

    let launch = resolve_launch(
        &config,
        "prompted",
        None,
        None,
        &LaunchOptionValues {
            model: Some("m1".into()),
            effort: None,
        },
    )
    .expect("the profile launches");

    assert_eq!(
        launch.argv,
        strings(&["prog", "--own", "--model", "m1"]),
        "the recorded argv is the profile's, then the launch options, and nothing of the prompt"
    );
    assert_eq!(
        launch
            .delivery
            .as_ref()
            .and_then(|d| d.launch_prompt.clone()),
        Some(strings(&["--", "{prompt}"]))
    );
}

#[test]
fn a_launch_of_a_profile_without_a_delivery_table_carries_none() {
    let tree = TempDir::new("yagura-cli-test", "delivery-launch-none");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(&profiles_dir, "plain", &source_with(""));
    let config = config_with(Some(profiles_dir), lookup_nothing(), ProfileOs::host());

    let launch = resolve_launch(&config, "plain", None, None, &LaunchOptionValues::default())
        .expect("the profile launches");

    assert!(launch.delivery.is_none());
}

const AWKWARD_PROMPT: &str = "say \"hi\" and 'bye' with $HOME and %PATH% ; | & {prompt} {text}";

#[test]
fn the_prompt_is_put_into_the_template_once_and_whatever_it_holds_stays_as_written() {
    let delivery = delivery_of("[delivery]\nlaunch_prompt = [\"--\", \"{prompt}\"]\n");

    let injection = launch_prompt_injection(&delivery, AWKWARD_PROMPT)
        .expect("a profile with a launch prompt template yields an injection");

    assert_eq!(injection.args, strings(&["--", AWKWARD_PROMPT]));
    assert!(injection.config_file.is_none());
}

#[test]
fn a_template_with_the_prompt_inside_a_longer_element_keeps_the_rest_of_the_element() {
    let delivery = delivery_of("[delivery]\nlaunch_prompt = [\"--prompt={prompt}\"]\n");

    let injection = launch_prompt_injection(&delivery, "go {prompt}").unwrap();

    assert_eq!(injection.args, strings(&["--prompt=go {prompt}"]));
}

#[test]
fn a_profile_without_a_launch_prompt_template_yields_no_injection() {
    let delivery = delivery_of("[delivery]\nwake = \"wait\"\n");

    assert!(launch_prompt_injection(&delivery, "anything").is_none());
}

fn probe_config() -> DaemonConfig {
    DaemonConfig {
        host_os: ProfileOs::host(),
        ..approver_base_config()
    }
}

fn sentinel(text: &str, name: &str) -> Option<String> {
    let prefix = format!("<<{name}=");
    let start = text.rfind(&prefix)? + prefix.len();
    let end = text[start..].find(">>")? + start;
    Some(text[start..end].to_string())
}

#[tokio::test]
async fn the_prompt_arrives_in_the_childs_argv_after_every_other_injection_and_verbatim() {
    let h = Harness::start("delivery-prompt-argv", probe_config()).await;
    let delivery = delivery_of("[delivery]\nlaunch_prompt = [\"--\", \"{prompt}\"]\n");
    let injections = vec![
        CliInjection {
            kind: InjectionKind::Hooks,
            args: strings(&["--hooks-arg"]),
            config_file: None,
        },
        launch_prompt_injection(&delivery, AWKWARD_PROMPT).expect("an injection"),
    ];

    let opened = h
        .registry
        .open_with_extra_env(
            OpenWindowRequest {
                command: vec![argv_probe_path().to_str().expect("UTF-8").to_string()],
                cwd: None,
                name: None,
                vendor: None,
                detection: None,
            },
            Vec::new(),
            injections,
            WindowProvenance::default(),
        )
        .await
        .expect("the window opens");
    let window_id = opened.window_id;

    h.wait_for_scrollback(&window_id, b"<<argv[3]=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert_eq!(sentinel(&text, "argv[1]").as_deref(), Some("--hooks-arg"));
    assert_eq!(sentinel(&text, "argv[2]").as_deref(), Some("--"));
    assert_eq!(sentinel(&text, "argv[3]").as_deref(), Some(AWKWARD_PROMPT));
}
