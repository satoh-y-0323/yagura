#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: a launch option that sets environment variables instead
//! of (or besides) argv elements.
//!
//! Every profile is written by the test itself into a directory it just made,
//! then read back through `list_profiles` / `resolve_launch`.

use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{
    LaunchOptionValues, ProfileLaunch, ProfileLaunchError, list_profiles, resolve_launch,
};
use yagura_test_support::paths::TempDir;

const ID: &str = "envopt";

/// A profile whose `[model]` carries only `env`, with an alias, and a
/// `[env]` that already names the same variable.
const MODEL_ENV_ONLY: &str = "schema = 1
display_name = \"Envopt\"

[command]
default = [\"envopt-program\"]

[env]
GROK_DEFAULT_MODEL = \"from-profile\"
KEEP = \"kept\"

[model]
env = { GROK_DEFAULT_MODEL = \"{value}\" }

[model.aliases]
fast = \"grok-fast-1\"
";

struct Fixture {
    _tree: TempDir,
    config: DaemonConfig,
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let tree = TempDir::new("yagura-cli-test", tag);
        let dir = tree.make_dir("profiles");
        let config = DaemonConfig {
            profiles_dir: Some(dir.clone()),
            default_shell: None,
            detection_dir: None,
            ..DaemonConfig::default()
        };
        Self {
            _tree: tree,
            config,
            dir,
        }
    }

    fn write(&self, id: &str, source: &str) {
        std::fs::write(self.dir.join(format!("{id}.toml")), source.as_bytes())
            .expect("fixture profile should be writable");
    }

    fn launch(&self, id: &str, model: Option<&str>) -> Result<ProfileLaunch, ProfileLaunchError> {
        let options = LaunchOptionValues {
            model: model.map(str::to_string),
            effort: None,
        };
        resolve_launch(&self.config, id, None, None, &options)
    }

    fn listed(&self, id: &str) -> bool {
        list_profiles(&self.config)
            .profiles
            .iter()
            .any(|entry| entry.id == id)
    }
}

#[test]
fn an_env_only_model_table_sets_the_variable_and_adds_nothing_to_argv() {
    let fixture = Fixture::new("env-only");
    fixture.write(ID, MODEL_ENV_ONLY);

    let plain = fixture
        .launch(ID, None)
        .unwrap_or_else(|err| panic!("an env-only [model] must be accepted, got {err}"));
    let literal = fixture.launch(ID, Some("grok-x")).unwrap();
    let aliased = fixture.launch(ID, Some("fast")).unwrap();

    assert_eq!(literal.argv, plain.argv, "env must add nothing to argv");
    assert_eq!(aliased.argv, plain.argv, "env must add nothing to argv");
    assert_eq!(literal.env["GROK_DEFAULT_MODEL"], "grok-x");
    assert_eq!(
        aliased.env["GROK_DEFAULT_MODEL"], "grok-fast-1",
        "an alias is resolved before it is placed in the environment"
    );
}

#[test]
fn without_a_model_nothing_is_added_and_with_one_the_option_beats_the_profile_env() {
    let fixture = Fixture::new("env-precedence");
    fixture.write(ID, MODEL_ENV_ONLY);

    let plain = fixture
        .launch(ID, None)
        .unwrap_or_else(|err| panic!("an env-only [model] must be accepted, got {err}"));
    assert_eq!(plain.env["GROK_DEFAULT_MODEL"], "from-profile");
    assert_eq!(plain.env["KEEP"], "kept");

    let chosen = fixture.launch(ID, Some("grok-x")).unwrap();
    assert_eq!(chosen.env["GROK_DEFAULT_MODEL"], "grok-x");
    assert_eq!(
        chosen.env["KEEP"], "kept",
        "an unrelated [env] entry is untouched"
    );
    assert_eq!(chosen.env.len(), plain.env.len());
}

/// One profile body with the given `[model]` / `[effort]` text.
fn with_tables(tables: &str) -> String {
    format!(
        "schema = 1
display_name = \"Envopt\"

[command]
default = [\"envopt-program\"]

{tables}"
    )
}

#[test]
fn a_launch_option_table_without_args_or_valid_env_keys_leaves_the_profile_unlisted() {
    let fixture = Fixture::new("env-refused");
    fixture.write(
        "good",
        &with_tables("[model]\nenv = { OK_NAME = \"{value}\" }\n"),
    );
    let refused: &[(&str, &str)] = &[
        ("none", "[model]\n"),
        ("reserved", "[model]\nenv = { YAGURA_X = \"{value}\" }\n"),
        (
            "reservedcase",
            "[model]\nenv = { yagura_x = \"{value}\" }\n",
        ),
        ("wslenv", "[model]\nenv = { WSLENV = \"{value}\" }\n"),
        ("empty", "[model]\nenv = { \"\" = \"{value}\" }\n"),
        ("equals", "[model]\nenv = { \"A=B\" = \"{value}\" }\n"),
        ("nul", "[model]\nenv = { \"A\\u0000B\" = \"{value}\" }\n"),
        (
            "clash",
            "[model]\nenv = { SAME = \"{value}\" }\n[effort]\nenv = { SAME = \"{value}\" }\n",
        ),
        (
            "clashcase",
            "[model]\nenv = { Same = \"{value}\" }\n[effort]\nenv = { SAME = \"{value}\" }\n",
        ),
    ];
    for (id, tables) in refused {
        fixture.write(id, &with_tables(tables));
    }

    assert!(
        fixture.listed("good"),
        "the control profile with a valid env key must be listed"
    );
    for (id, _) in refused {
        assert!(!fixture.listed(id), "profile {id} must be refused");
        assert!(
            matches!(
                fixture.launch(id, None),
                Err(ProfileLaunchError::UnknownProfile)
            ),
            "profile {id} must not resolve"
        );
    }
}
