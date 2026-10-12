# dep-audit

A single-file developer command (`dep_audit.py`) that checks the Rust and npm dependencies for
known vulnerabilities: `cargo audit` at the repository root and `npm audit` in `gui/`, each
followed by a look at the advisory databases those tools do not read (see §3). A third check,
`vendor`, confirms that the bundled ConPTY under `vendor/conpty/` is the build the daemon pins.

It exists because CI is not run on every push, so a check that only CI runs leaves a gap. Run this
before every push; CI calls the same script, so the local check and CI fail on the same criteria.

This file is the single source for how to call the command and how to handle a failure; the
`docs/building.md` only points here. **The failure criteria themselves are defined
by the arguments the script passes to each tool**; the table in §3 describes them.

## 1. Prerequisites

- `python` on `PATH` (developed with 3.11.9). **Standard library only** - nothing to install.
- `cargo` and `cargo-audit` 0.22.2 (the version CI uses):
  `cargo install cargo-audit --version 0.22.2 --locked`
- `npm` (ships with Node.js). The versions checked are the ones pinned in
  `gui/package-lock.json`.
- Network access: both tools fetch their advisory databases, and the script queries
  [OSV](https://osv.dev) and the GitHub REST API.
- A GitHub credential for the API: `GITHUB_TOKEN` (or `GH_TOKEN`) in the environment, or a signed-in
  [`gh`](https://cli.github.com) (`gh auth login`), whose `gh auth token` is used. Read access to
  public repositories is all it needs. CI passes the workflow's own token.
- For `npm`: an installed `gui/node_modules` (`npm ci`), where each package's `repository` is read.

A missing tool, a missing credential or an advisory source that cannot be reached is reported with
its cause (and, for `cargo-audit`, the install command above) and counts as a failure, never as a
pass.

## 2. Calling it (verbatim examples)

All examples are run from anywhere - the repository root is derived from the script's own
location, not from the current directory.

```bash
# (1) both checks; this is what to run before a push
python scripts/dep-audit/dep_audit.py

# (2) one check only (CI runs each one in its own job)
python scripts/dep-audit/dep_audit.py --only cargo
python scripts/dep-audit/dep_audit.py --only npm
python scripts/dep-audit/dep_audit.py --only vendor
```

| Argument | Meaning |
|---|---|
| `--only cargo` / `--only npm` / `--only vendor` | Run just that check. Without it all three run. Any other value is an argument error (exit code `2`). |

Both checks always run, even when the first one fails, so a single run reports everything. Each
check prints what it runs before it starts and `passed` / `failed` / `could not start` after it
ends; a `summary:` line closes the run.

Exit codes: `0` every selected check passed, `1` at least one failed or could not start, `2`
invalid arguments.

## 3. What fails

| Check | Fails on | Shown but does not fail |
|---|---|---|
| `cargo audit` (repository root) | a vulnerability | unmaintained / unsound / yanked warnings |
| `cargo`: OSV and repository advisories (in process) | an advisory for a locked crate version that is Medium or above, or has no severity | Low; RustSec notices (unmaintained / unsound) and GitHub's copies of them |
| `npm audit --audit-level=high` (`gui/`) | High or Critical in **any** dependency | moderate and below |
| `npm`: repository advisories (in process) | an advisory for a locked package version that is High or above, or has no severity | Medium and below |
| `vendor` (in process) | a file under `vendor/conpty/x64` whose SHA-256 differs from the table in `vendor/conpty/README.md` or from the hash pinned in `crates/yagura-os/src/bundled_conpty_pins.rs`, or a hash missing from either | - |

`cargo audit` is used with its default exit code, which already leaves warnings green.

Why the extra look: `cargo audit` reads RustSec alone and `npm audit` reads GitHub's reviewed
advisories alone. A project's own advisory, published in its repository, reaches neither until
GitHub reviews it, which has taken more than two weeks; and a reviewed GitHub advisory for a crate
may never reach RustSec. So the script also

- queries OSV once for every crate version `cargo metadata --locked` resolves (OSV carries RustSec
  and reviewed GitHub advisories together), and
- reads the published advisories of every GitHub repository a locked package comes from (the
  crate's `repository` from `cargo metadata`, the npm package's `repository` from its installed
  `package.json`), matching each advisory's affected range against the locked versions. When
  GitHub has reviewed an advisory, the reviewed ranges are used instead of the repository's own,
  which can be looser.

A repository that answers 404 (moved or private) is counted in the `advisories:` line and does not
fail the check.

npm covers development dependencies as well: the dev / production split in `package.json` does not
say what ends up in the bundle (the `svelte` runtime is a devDependency), so the check does not
rely on it.

## 4. When it fails

1. Fix it first: `npm audit fix` in `gui/` (never `--force` without deciding on the major
   upgrade), or a version bump of the affected crate. Run the GUI gates afterwards.
2. If it cannot be fixed, **push only when every remaining failure has been accepted by the user
   and the reason is written in the unit's record.** Nothing in the repository records accepted
   advisories yet; that mechanism is to be decided when the first one is needed.

## 5. Warnings

`cargo audit` warnings (unmaintained, unsound) and Low advisories do not fail the check. Record the ones that are new
since the previous run in the unit's record, one line each, and file a task only for one that
reaches users.

## 6. Tests

```bash
python -m pytest scripts/dep-audit/ -v
```

`test_dep_audit.py` checks the commands, working directories, `--only`, the failure handling and
the advisory matching with the subprocess runner, the executable lookup and the advisory sources
injected; it never starts a real `cargo` or `npm` and never reaches the network.
