# Launch profile fixtures

Launch profile TOML files used by the `yagura-daemon` integration tests.

Each file carries exactly one property, so a test that fails names the rule
that broke. Nothing here is a realistic profile: the program names are
deliberately unlikely to exist on any machine, so availability is decided only
by the lookup a test injects and never by what happens to be installed on the
host running the suite.

A test that measures enumeration copies only the files it needs into a
temporary directory of its own, because enumeration is defined over the whole
directory and a shared directory would make every case depend on the others.

## Files

| File | Property it carries |
|---|---|
| `valid-default.toml` | A well-formed profile with a detection definition and an argv used on every operating system. |
| `valid-os-override.toml` | Per-operating-system argv wins over `default`, which is present so a wrong selection shows up as the `default` argv. |
| `valid-windows-only.toml` | Only a `windows` argv and no `default`, so every other operating system has no argv to launch. |
| `valid-shell-omitted.toml` | No `[command]` table at all, which means the daemon's default shell. |
| `valid-with-env.toml` | `[env]` values are taken verbatim, including an empty value and text that a shell would have expanded. |
| `invalid-schema.toml` | A schema version the daemon does not support. |
| `invalid-no-display-name.toml` | The required display name is missing. |
| `invalid-display-name.toml` | The display name breaks the window-label rule (it carries a control character, written as an escape so the file stays ASCII). |
| `invalid-empty-arg.toml` | An argv containing an empty element. |
| `invalid-syntax-with-secret.toml` | A genuine TOML syntax error (an unquoted `[env]` value) next to a secret-shaped value; the value is a deliberately fake placeholder (`not-a-real-token-0000`), never a realistic credential. |
| `Bad Id.toml` | Valid contents under a file name that cannot be an identifier; the identifier is the file name stem. |
| `reserved-env.toml` | A reserved `YAGURA_`-prefixed environment key next to a kept one; the reserved key is dropped with a warning and the profile stays usable. |
| `relative-cwd.toml` | A relative working directory, which is warned about and ignored rather than resolved against anything. |
| `tilde-cwd.toml` | A `~` working directory, treated the same way as a relative one: no expansion. |
| `valid-launch-options.toml` | Both launch-option axes mapped: `[model]` with three aliases written in an order that is not alphabetical (so a listing that sorted them would show it), and `[effort]` mapped with no alias table at all. |
| `valid-launch-option-single-axis.toml` | Only `[model]` is mapped, so the other axis is one this profile does not accept. |
| `valid-launch-option-no-aliases.toml` | A mapped axis whose table carries `args` alone: every value passes through unchanged. |
| `invalid-launch-option-no-args.toml` | A launch-option table with no `args` key, which is the one key the table cannot omit. |
| `invalid-launch-option-no-placeholder.toml` | `args` that never spells the `{value}` placeholder, so a supplied value would have nowhere to go. |
| `invalid-launch-option-empty-arg.toml` | An empty element inside a launch option's `args`, held to the same argv authoring rule as `[command]`. |
| `invalid-launch-option-alias-key.toml` | An alias key outside the identifier character set (an uppercase letter). |
| `invalid-launch-option-alias-value.toml` | An alias value breaking the launch option value rule (it carries a control character, written as an escape so the file stays ASCII). |
| `invalid-launch-option-unknown-key.toml` | An unknown key inside a launch-option table, which the table refuses rather than ignores. |
