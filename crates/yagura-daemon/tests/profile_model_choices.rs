#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: a profile's `[model.choices]` table -- the rules that
//! exclude a document breaking it, the three forms that are read, and how
//! `resolve_model_choices` turns the table into either a fixed list or the
//! command that lists models.
//!
//! Every profile here is written by the test itself into a directory it just
//! made, and resolution runs against a lookup and an operating system handed
//! in through the configuration, so the Windows resolution rules are measured
//! on whichever host runs this binary.

mod common;

use std::path::{Path, PathBuf};

use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{
    JsonFilter, LaunchOptionError, ModelChoicesError, ModelChoicesPlan, ModelChoicesSpec,
    OutputFormat, Profile, ProfileOs, ProfileParseError, list_profiles, parse_profile,
    resolve_model_choices, unavailable_reason,
};
use yagura_os::program::ProgramLookup;
use yagura_test_support::paths::TempDir;

// --- documents ----------------------------------------------------------------

/// The head every document shares: a launchable argv and a `[model]` table
/// whose own `args` are distinct from anything a choices table appends.
const HEAD: &str = "schema = 1\ndisplay_name = \"Choices\"\n\n[command]\ndefault = \
                    [\"choices-program\"]\n\n";

/// A `[model]` table. Kept out of `format!` so its placeholder is never read as
/// a format argument.
const MODEL_TABLE: &str = "[model]\nargs = [\"--tier\", \"{value}\"]\n\n";

/// A document with a `[model]` table and, under it, `[model.choices]` holding
/// `choices_body` verbatim.
fn with_choices(choices_body: &str) -> String {
    let mut source = String::from(HEAD);
    source.push_str(MODEL_TABLE);
    source.push_str("[model.choices]\n");
    source.push_str(choices_body);
    source.push('\n');
    source
}

const VALUES_FORM: &str = "values = [\"tier-a\", \"tier-b\", \"tier-c\"]";
const LINES_FORM: &str = "args = [\"list\", \"tiers\"]\nformat = \"lines\"\nseparator = \"\\t\"";
const JSON_FORM: &str = "args = [\"list\", \"tiers\", \"--json\"]\nformat = \"json\"\nitems = \
                         \"/tiers\"\nid = \"/slug\"\nlabel = \"/title\"\nfilter = { pointer = \
                         \"/shown\", equals = \"yes\" }";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn parsed(source: &str) -> Profile {
    parse_profile("choices", source)
        .unwrap_or_else(|err| panic!("the document must be accepted, got {err:?}:\n{source}"))
}

fn model_choices(profile: &Profile) -> &ModelChoicesSpec {
    profile
        .options
        .model_choices
        .as_ref()
        .expect("the document declares [model.choices]")
}

// --- the three forms that are read ----------------------------------------------

#[test]
fn a_values_table_parses_into_the_fixed_list_in_the_authors_order() {
    let profile = parsed(&with_choices(
        "values = [\"tier-c\", \"tier-a\", \"tier-b\"]",
    ));
    match model_choices(&profile) {
        ModelChoicesSpec::Values(values) => assert_eq!(
            values,
            &strings(&["tier-c", "tier-a", "tier-b"]),
            "the list keeps the order its author wrote"
        ),
        other => panic!("a values table must parse into the fixed list, got {other:?}"),
    }
}

#[test]
fn a_lines_table_parses_into_a_command_read_line_by_line() {
    let profile = parsed(&with_choices(LINES_FORM));
    match model_choices(&profile) {
        ModelChoicesSpec::Command {
            args,
            format: OutputFormat::Lines { separator },
        } => {
            assert_eq!(args, &strings(&["list", "tiers"]));
            assert_eq!(separator, "\t");
        }
        other => panic!("a lines table must parse into a line-by-line command, got {other:?}"),
    }
}

#[test]
fn a_json_table_with_a_filter_parses_into_a_command_read_through_its_pointers() {
    let profile = parsed(&with_choices(JSON_FORM));
    match model_choices(&profile) {
        ModelChoicesSpec::Command {
            args,
            format:
                OutputFormat::Json {
                    items,
                    id,
                    label,
                    filter,
                },
        } => {
            assert_eq!(args, &strings(&["list", "tiers", "--json"]));
            assert_eq!(items, "/tiers");
            assert_eq!(id, "/slug");
            assert_eq!(label.as_deref(), Some("/title"));
            match filter {
                Some(JsonFilter { pointer, equals }) => {
                    assert_eq!(pointer, "/shown");
                    assert_eq!(equals, "yes");
                }
                None => panic!("the document declares a filter"),
            }
        }
        other => panic!("a json table must parse into a pointer-read command, got {other:?}"),
    }
}

#[test]
fn a_json_table_without_label_or_filter_leaves_both_absent() {
    let profile = parsed(&with_choices(
        "args = [\"list\"]\nformat = \"json\"\nitems = \"\"\nid = \"/slug\"",
    ));
    match model_choices(&profile) {
        ModelChoicesSpec::Command {
            format:
                OutputFormat::Json {
                    items,
                    label,
                    filter,
                    ..
                },
            ..
        } => {
            assert_eq!(
                items, "",
                "the empty pointer names the whole document, which is valid pointer syntax"
            );
            assert!(label.is_none(), "an omitted label stays absent: {label:?}");
            assert!(
                filter.is_none(),
                "an omitted filter stays absent: {filter:?}"
            );
        }
        other => panic!("a json table must parse into a pointer-read command, got {other:?}"),
    }
}

#[test]
fn a_model_table_without_a_choices_table_carries_no_choices() {
    let mut source = String::from(HEAD);
    source.push_str(MODEL_TABLE);
    let profile = parsed(&source);
    assert!(
        profile.options.model_choices.is_none(),
        "a profile that declares no choices has none, got {:?}",
        profile.options.model_choices
    );
    assert!(profile.options.model.is_some());
}

#[test]
fn a_choices_table_leaves_the_model_axis_mapping_as_written() {
    let profile = parsed(&with_choices(VALUES_FORM));
    let model = profile
        .options
        .model
        .as_ref()
        .expect("the [model] table still maps the axis");
    assert_eq!(model.args, strings(&["--tier", "{value}"]));
}

// --- the documents that are refused ----------------------------------------------

/// Each document breaks exactly one rule of the choices table; the label only
/// names the case in a failure message.
const RULE_BREAKING_CHOICES: &[(&str, &str)] = &[
    (
        "both values and args",
        "values = [\"tier-a\"]\nargs = [\"list\"]\nformat = \"lines\"\nseparator = \",\"",
    ),
    ("neither values nor args", ""),
    (
        "values with a key only a command takes",
        "values = [\"tier-a\"]\nformat = \"lines\"",
    ),
    ("an empty values list", "values = []"),
    ("a value with a space", "values = [\"tier a\"]"),
    ("a value with a shell character", "values = [\"$x\"]"),
    ("a duplicated value", "values = [\"tier-a\", \"tier-a\"]"),
    (
        "an empty args list",
        "args = []\nformat = \"lines\"\nseparator = \",\"",
    ),
    ("a missing format", "args = [\"list\"]\nseparator = \",\""),
    (
        "an unknown format",
        "args = [\"list\"]\nformat = \"yaml\"\nseparator = \",\"",
    ),
    (
        "lines without a separator",
        "args = [\"list\"]\nformat = \"lines\"",
    ),
    (
        "lines with an empty separator",
        "args = [\"list\"]\nformat = \"lines\"\nseparator = \"\"",
    ),
    (
        "lines with items",
        "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"\nitems = \"/tiers\"",
    ),
    (
        "lines with id",
        "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"\nid = \"/slug\"",
    ),
    (
        "lines with label",
        "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"\nlabel = \"/title\"",
    ),
    (
        "lines with a filter",
        "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"\nfilter = { pointer = \
         \"/shown\", equals = \"yes\" }",
    ),
    (
        "json without id",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"",
    ),
    (
        "json without items",
        "args = [\"list\"]\nformat = \"json\"\nid = \"/slug\"",
    ),
    (
        "json with a separator",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\nseparator = \
         \",\"",
    ),
    (
        "an items pointer without a leading slash",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"tiers\"\nid = \"/slug\"",
    ),
    (
        "an id pointer without a leading slash",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"slug\"",
    ),
    (
        "a label pointer without a leading slash",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\nlabel = \
         \"title\"",
    ),
    (
        "a filter without equals",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\nfilter = { \
         pointer = \"/shown\" }",
    ),
    (
        "a filter without pointer",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\nfilter = { \
         equals = \"yes\" }",
    ),
    (
        "a filter with an empty equals",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\nfilter = { \
         pointer = \"/shown\", equals = \"\" }",
    ),
    (
        "a filter pointer without a leading slash",
        "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\nfilter = { \
         pointer = \"shown\", equals = \"yes\" }",
    ),
];

#[test]
fn every_choices_table_breaking_a_rule_is_excluded_under_the_launch_option_variant() {
    for (case, body) in RULE_BREAKING_CHOICES {
        let source = with_choices(body);
        let err = parse_profile("choices", &source)
            .expect_err(&format!("{case}: the document must be rejected"));
        assert!(
            matches!(
                err,
                ProfileParseError::InvalidLaunchOption(LaunchOptionError::InvalidChoices)
            ),
            "{case}: a choices rule breaks the document under the launch option variant, got \
             {err:?}"
        );
        assert_eq!(
            err.to_string(),
            "invalid launch option",
            "{case}: the reason a client is shown is the existing fixed phrase"
        );
    }
}

#[test]
fn an_unknown_key_in_a_choices_table_is_refused_while_the_document_is_read() {
    parsed(&with_choices(VALUES_FORM));
    let mut body = String::from(VALUES_FORM);
    body.push_str("\nunknown_key = \"x\"");
    let err = parse_profile("choices", &with_choices(&body))
        .expect_err("an unknown key must be rejected");
    assert!(
        matches!(err, ProfileParseError::Toml(_)),
        "a misspelt key must not silently mean something else: got {err:?}"
    );
    assert_eq!(err.to_string(), "could not be parsed");
}

#[test]
fn an_unknown_key_in_a_filter_is_refused_while_the_document_is_read() {
    parsed(&with_choices(JSON_FORM));
    let body = JSON_FORM.replace(
        "equals = \"yes\" }",
        "equals = \"yes\", unknown_key = \"x\" }",
    );
    assert_ne!(body, JSON_FORM, "the unknown key must have been spliced in");
    let err = parse_profile("choices", &with_choices(&body))
        .expect_err("an unknown key in a filter must be rejected");
    assert!(
        matches!(err, ProfileParseError::Toml(_)),
        "a misspelt filter key must not silently mean something else: got {err:?}"
    );
}

#[test]
fn a_choices_table_under_effort_is_refused() {
    parsed(&with_choices(VALUES_FORM));
    let mut source = String::from(HEAD);
    source.push_str(MODEL_TABLE);
    source.push_str("[effort]\nargs = [\"--level\", \"{value}\"]\n\n[effort.choices]\n");
    source.push_str(VALUES_FORM);
    source.push('\n');
    let err =
        parse_profile("choices", &source).expect_err("only the model axis carries a choices table");
    assert!(
        matches!(err, ProfileParseError::Toml(_)),
        "the effort table does not know a choices key: got {err:?}"
    );
}

#[test]
fn a_listing_excludes_each_rule_breaking_document_with_a_fixed_warning_naming_nothing_inside() {
    let tree = TempDir::new("yagura-cli-test", "profile-choices-listing");
    let dir = tree.make_dir("profiles");
    let excluded: [(&str, String); 4] = [
        (
            "dup-values",
            with_choices("values = [\"leak-sentinel-a\", \"leak-sentinel-a\"]"),
        ),
        (
            "no-format",
            with_choices("args = [\"leak-sentinel-args\"]\nseparator = \"leak-sentinel-sep\""),
        ),
        (
            "bad-pointer",
            with_choices(
                "args = [\"list\"]\nformat = \"json\"\nitems = \"leak-sentinel-pointer\"\nid = \
                 \"/slug\"",
            ),
        ),
        (
            "half-filter",
            with_choices(
                "args = [\"list\"]\nformat = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\n\
                 filter = { equals = \"leak-sentinel-equals\" }",
            ),
        ),
    ];
    for (id, source) in &excluded {
        write_profile(&dir, id, source);
    }
    write_profile(&dir, "accepted", &with_choices(VALUES_FORM));

    let config = config_with(dir, ProfileOs::host(), empty_lookup());
    let listing = list_profiles(&config);

    let ids: Vec<&str> = listing.profiles.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["accepted"],
        "only the well-formed document is listed"
    );
    assert_eq!(
        listing.warnings.len(),
        excluded.len(),
        "one warning per excluded document: {:?}",
        listing.warnings
    );
    for (id, _) in &excluded {
        let expected = format!("profile file \"{id}.toml\" invalid launch option");
        assert!(
            listing.warnings.contains(&expected),
            "the warning must read exactly {expected:?}, got {:?}",
            listing.warnings
        );
    }
    assert!(
        !listing.warnings.iter().any(|w| w.contains("leak-sentinel")),
        "a warning reaches clients, so nothing read out of the document may appear in it: \
         {:?}",
        listing.warnings
    );
}

// --- resolving the choices of a listed profile ------------------------------------

fn empty_lookup() -> ProgramLookup {
    ProgramLookup {
        path_dirs: Vec::new(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: None,
    }
}

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn config_with(profiles_dir: PathBuf, host_os: ProfileOs, lookup: ProgramLookup) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        host_os,
        program_lookup: lookup,
        program_is_executable: file_exists,
        default_shell: None,
        detection_dir: None,
        ..DaemonConfig::default()
    }
}

fn write_profile(dir: &Path, id: &str, source: &str) {
    std::fs::write(dir.join(format!("{id}.toml")), source).expect("profile should be writable");
}

/// A document whose `[command]` is `command_toml`, with a `[model]` table and a
/// `[model.choices]` holding `choices_body`, plus `extra` appended verbatim.
fn listing_profile(command_toml: &str, choices_body: &str, extra: &str) -> String {
    let mut source = String::from("schema = 1\ndisplay_name = \"Listing\"\n\n[command]\n");
    source.push_str(command_toml);
    source.push_str("\n\n");
    source.push_str(MODEL_TABLE);
    source.push_str("[model.choices]\n");
    source.push_str(choices_body);
    source.push_str("\n\n");
    source.push_str(extra);
    source
}

/// A tree holding a profiles directory and a bin directory, with an empty
/// Windows executable image of each name in `programs` placed in the bin
/// directory.
struct Fixture {
    _tree: TempDir,
    profiles: PathBuf,
    bin: PathBuf,
}

fn fixture(tag: &str, programs: &[&str]) -> Fixture {
    let tree = TempDir::new("yagura-cli-test", tag);
    let profiles = tree.make_dir("profiles");
    let bin = tree.make_dir("bin");
    for program in programs {
        std::fs::write(bin.join(program), []).expect("program fixture should be writable");
    }
    Fixture {
        _tree: tree,
        profiles,
        bin,
    }
}

fn windows_lookup(path_dirs: Vec<PathBuf>, extra_bin_dirs: Vec<PathBuf>) -> ProgramLookup {
    ProgramLookup {
        path_dirs,
        extra_bin_dirs,
        windows_comspec: Some(PathBuf::from("cmd.exe")),
    }
}

fn describe(result: &Result<ModelChoicesPlan, ModelChoicesError>) -> String {
    match result {
        Ok(ModelChoicesPlan::NoChoices) => "Ok(NoChoices)".to_string(),
        Ok(ModelChoicesPlan::Values(values)) => format!("Ok(Values({values:?}))"),
        Ok(ModelChoicesPlan::Command { argv, .. }) => format!("Ok(Command {{ argv: {argv:?} }})"),
        Err(err) => format!("Err({err:?})"),
    }
}

/// The parts of a `Command` plan, or a panic naming what came back instead.
struct CommandPlan {
    argv: Vec<String>,
    spawn_argv: Vec<String>,
    launch_argv: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    format: OutputFormat,
}

fn expect_command(result: Result<ModelChoicesPlan, ModelChoicesError>) -> CommandPlan {
    let description = describe(&result);
    match result {
        Ok(ModelChoicesPlan::Command {
            argv,
            spawn_argv,
            launch_argv,
            env,
            cwd,
            format,
        }) => CommandPlan {
            argv,
            spawn_argv,
            launch_argv,
            env,
            cwd,
            format,
        },
        _ => panic!("a command-listed profile must resolve into a command, got {description}"),
    }
}

const WSL_WINDOWS_ARGV: [&str; 6] = [
    "wsl",
    "-e",
    "bash",
    "-lic",
    "exec \"$0\" \"$@\"",
    "listing-cli",
];

#[test]
fn the_listing_argv_is_the_selected_launch_argv_followed_by_the_choices_args() {
    let fx = fixture("profile-choices-order", &["wsl.exe"]);
    let command = "default = [\"not-this-one\"]\nwindows = [\"wsl\", \"-e\", \"bash\", \"-lic\", \
                   \"exec \\\"$0\\\" \\\"$@\\\"\", \"listing-cli\"]";
    let extra = "[mcp]\nargs = [\"--bridge-flag\"]\n";
    write_profile(
        &fx.profiles,
        "wsl-listing",
        &listing_profile(
            command,
            "args = [\"debug\", \"tiers\"]\nformat = \"lines\"\nseparator = \",\"",
            extra,
        ),
    );
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    let plan = expect_command(resolve_model_choices(&config, "wsl-listing"));

    let mut expected = strings(&WSL_WINDOWS_ARGV);
    expected.extend(strings(&["debug", "tiers"]));
    assert_eq!(
        plan.argv, expected,
        "the choices args follow the operating system's own launch argv, and neither the \
         model option's args nor the MCP args are mixed in"
    );
    assert_eq!(
        plan.launch_argv,
        strings(&WSL_WINDOWS_ARGV),
        "the launch argv is the one before the choices args are appended"
    );
    match plan.format {
        OutputFormat::Lines { separator } => assert_eq!(separator, ","),
        other => panic!("the plan carries the profile's own format, got {other:?}"),
    }
}

#[test]
fn the_spawn_argv_resolves_a_bare_name_found_only_in_the_extra_bin_directories() {
    let fx = fixture("profile-choices-resolve", &["listing-program.exe"]);
    write_profile(
        &fx.profiles,
        "resolved",
        &listing_profile(
            "default = [\"listing-program\"]",
            "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"",
            "",
        ),
    );
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(Vec::new(), vec![fx.bin.clone()]),
    );

    let plan = expect_command(resolve_model_choices(&config, "resolved"));

    assert_eq!(
        plan.argv,
        strings(&["listing-program", "list"]),
        "the unresolved argv keeps the name the profile wrote"
    );
    let resolved = fx.bin.join("listing-program.exe");
    assert_eq!(
        plan.spawn_argv,
        vec![resolved.to_string_lossy().into_owned(), "list".to_string()],
        "the argv actually started is resolved through the same lookup a window uses"
    );
}

#[test]
fn a_choices_arg_the_command_interpreter_would_reinterpret_makes_a_shim_profile_unavailable() {
    let fx = fixture("profile-choices-metachar", &["listing-shim.cmd"]);
    write_profile(
        &fx.profiles,
        "shim",
        &listing_profile(
            "default = [\"listing-shim\"]",
            "args = [\"list\", \"a&b\"]\nformat = \"lines\"\nseparator = \",\"",
            "",
        ),
    );
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    // The same reinterpretation spelled in the launch argv itself gives the
    // listing's own reason text, so the expected sentence is not restated here.
    let rejected_launch = parse_profile(
        "rejected-launch",
        &listing_profile("default = [\"listing-shim\", \"a&b\"]", VALUES_FORM, ""),
    )
    .expect("the reference document parses");
    let metacharacter_reason = unavailable_reason(&rejected_launch, &config)
        .expect("a shim with a reinterpreted argument is unavailable");

    let result = resolve_model_choices(&config, "shim");
    let description = describe(&result);
    match result {
        Err(ModelChoicesError::Unavailable(reason)) => assert_eq!(reason, metacharacter_reason),
        _ => {
            panic!("the choices args are checked the same way a launch argv is, got {description}")
        }
    }
}

#[test]
fn an_unavailable_profile_is_refused_with_its_listing_reason() {
    let fx = fixture("profile-choices-unavailable", &[]);
    let source = listing_profile(
        "default = [\"missing-listing-program\"]",
        "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"",
        "",
    );
    write_profile(&fx.profiles, "missing", &source);
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );
    let expected = unavailable_reason(&parsed(&source), &config)
        .expect("a program the lookup cannot find is unavailable");

    let result = resolve_model_choices(&config, "missing");
    let description = describe(&result);
    match result {
        Err(ModelChoicesError::Unavailable(reason)) => assert_eq!(
            reason, expected,
            "the refusal reads exactly as the listing's reason for the same profile"
        ),
        _ => panic!("nothing is started for an unavailable profile, got {description}"),
    }
}

#[test]
fn the_plan_carries_the_profile_env_without_reserved_keys_and_the_profile_cwd() {
    let fx = fixture("profile-choices-env", &["listing-program.exe"]);
    let cwd = fx.profiles.clone();
    let mut extra = String::from("[env]\nLISTING_ENV = \"listing-value\"\nYAGURA_EXTRA = \"x\"\n");
    extra.push_str("yagura_lower = \"y\"\n");
    let mut source = listing_profile(
        "default = [\"listing-program\"]",
        "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"",
        &extra,
    );
    source = source.replace(
        "display_name = \"Listing\"\n",
        &format!("display_name = \"Listing\"\ncwd = '{}'\n", cwd.display()),
    );
    write_profile(&fx.profiles, "with-env", &source);
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    let plan = expect_command(resolve_model_choices(&config, "with-env"));

    let mut env = plan.env.clone();
    env.sort();
    assert_eq!(
        env,
        vec![("LISTING_ENV".to_string(), "listing-value".to_string())],
        "the profile's own environment is carried, and nothing the daemon reserves for itself"
    );
    assert!(
        !plan
            .env
            .iter()
            .any(|(key, _)| key.to_ascii_uppercase().starts_with("YAGURA_")),
        "no reserved key reaches the listing command: {:?}",
        plan.env
    );
    assert_eq!(
        plan.cwd,
        Some(cwd),
        "the working directory is the profile's own"
    );
}

#[test]
fn the_plan_has_no_working_directory_when_the_profile_declares_none() {
    let fx = fixture("profile-choices-no-cwd", &["listing-program.exe"]);
    write_profile(
        &fx.profiles,
        "no-cwd",
        &listing_profile(
            "default = [\"listing-program\"]",
            "args = [\"list\"]\nformat = \"lines\"\nseparator = \",\"",
            "",
        ),
    );
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    let plan = expect_command(resolve_model_choices(&config, "no-cwd"));
    assert_eq!(plan.cwd, None);
}

#[test]
fn a_values_table_resolves_into_its_fixed_list() {
    let fx = fixture("profile-choices-values", &["listing-program.exe"]);
    write_profile(
        &fx.profiles,
        "fixed",
        &listing_profile("default = [\"listing-program\"]", VALUES_FORM, ""),
    );
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    let result = resolve_model_choices(&config, "fixed");
    let description = describe(&result);
    match result {
        Ok(ModelChoicesPlan::Values(values)) => {
            assert_eq!(values, strings(&["tier-a", "tier-b", "tier-c"]));
        }
        _ => panic!("a values table resolves into its list, got {description}"),
    }
}

#[test]
fn a_model_table_without_choices_resolves_into_no_choices() {
    let fx = fixture("profile-choices-none", &["listing-program.exe"]);
    let mut source = String::from(
        "schema = 1\ndisplay_name = \"Listing\"\n\n[command]\ndefault = [\"listing-program\"]\n\n",
    );
    source.push_str(MODEL_TABLE);
    write_profile(&fx.profiles, "plain", &source);
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    let result = resolve_model_choices(&config, "plain");
    let description = describe(&result);
    assert!(
        matches!(result, Ok(ModelChoicesPlan::NoChoices)),
        "a model table that lists no choices has none, got {description}"
    );
}

#[test]
fn a_profile_without_a_model_table_is_refused_as_having_no_model_option() {
    let fx = fixture("profile-choices-no-model", &["listing-program.exe"]);
    write_profile(
        &fx.profiles,
        "no-model",
        "schema = 1\ndisplay_name = \"Listing\"\n\n[command]\ndefault = [\"listing-program\"]\n",
    );
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    let result = resolve_model_choices(&config, "no-model");
    let description = describe(&result);
    assert!(
        matches!(result, Err(ModelChoicesError::NoModelOption)),
        "got {description}"
    );
}

#[test]
fn an_id_the_listing_does_not_report_is_refused_as_unknown() {
    let fx = fixture("profile-choices-unknown", &["listing-program.exe"]);
    write_profile(
        &fx.profiles,
        "broken",
        &with_choices("values = [\"tier-a\", \"tier-a\"]"),
    );
    write_profile(
        &fx.profiles,
        "present",
        &listing_profile("default = [\"listing-program\"]", VALUES_FORM, ""),
    );
    let config = config_with(
        fx.profiles.clone(),
        ProfileOs::Windows,
        windows_lookup(vec![fx.bin.clone()], Vec::new()),
    );

    assert!(
        !matches!(
            resolve_model_choices(&config, "present"),
            Err(ModelChoicesError::UnknownProfile)
        ),
        "a listed profile is known"
    );
    for id in ["absent", "broken", "Bad Id", "../present"] {
        let result = resolve_model_choices(&config, id);
        let description = describe(&result);
        assert!(
            matches!(result, Err(ModelChoicesError::UnknownProfile)),
            "{id:?} is not a profile the listing reports, got {description}"
        );
    }
}
