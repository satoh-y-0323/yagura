# AGENTS.md

Instructions for coding agents (and humans) working on yagura. Keep this file short: it holds the rules
that are not visible from the code. Anything a tool can enforce (types, clippy, ESLint, svelte-check)
belongs in that tool, not here.

Talk to the person you are working with in the language they use. Everything that lands in the
repository is written in English.

## What yagura is

A desktop app that runs agent CLIs (and any other command) side by side, each in its own window,
watches whether each one is working or waiting for you, and gives the agents an MCP server to see and
talk to each other.

| Path | Role |
|---|---|
| `crates/yagura-daemon` | Owns every window (a PTY and its child process), detects each window's state, and serves the IPC protocol |
| `crates/yagura-protocol` | The wire protocol shared by the daemon and its clients |
| `crates/yagura-os` | The only place that talks to the operating system (pipes and sockets, process trees, paths) |
| `crates/yagura-mcp` | A thin MCP stdio bridge in front of the daemon; owns no PTYs |
| `crates/yagura-build-support`, `crates/yagura-test-support` | Build-script and test helpers |
| `gui/` | Tauri + Svelte client. `gui/src-tauri` is the Rust side of the GUI |
| `profiles/` | The window profiles yagura ships (one TOML file per CLI) |
| `playbook.toml` | The default playbook (approval and limits) the GUI seeds on first start |
| `docs/` | User and developer documentation. Building from source: `docs/building.md` |

## Checks

Run what CI runs before you push. After a fresh clone, stage the GUI's sidecars once
(`cd gui && npm install && node scripts/prepare-sidecar.mjs`), or `cargo` fails on the GUI crate.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace --locked

cd gui
npm run lint
npm run format:check
npm run check
npm run type-check
npm test

python scripts/dep-audit/dep_audit.py   # from the repository root
```

`cargo test` can exit 0 with fewer tests than before when a test binary fails to start. Compare the
counts, not just the exit code.

## Design rules

These are review requirements. A change that breaks one is rejected even if it works.

1. **Mechanism, not policy.** The app provides windows, monitoring, messaging and the GUI. Workflows,
   personas, prompts and operating parameters (what needs approval, limits) live in the playbook and
   other config files, never in code.
2. **A window is a PTY running an arbitrary command.** No vendor branches in core types
   (`if vendor == ...`). Differences between CLIs and between operating systems are absorbed by config
   (profiles, detection definitions) and by `yagura-os`.
3. **Vendor-neutral storage.** No vendor names in stored schemas (SQLite tables and columns, JSON keys).
   Vendor-specific conversion stays inside the conversion layer.
4. **Never touch credentials.** No extracting or reusing OAuth tokens, no hooking into a CLI's internal
   API. Each CLI signs in by itself.
5. **No terminal multiplexer.** Do not depend on tmux or screen; yagura has its own PTY management.
6. **State detection fails safe.** When the state cannot be told, it is `Unknown`, never a guessed
   `Idle` or `Running`. In `Unknown`, nothing that could misfire runs (automatic delivery,
   `wait_for_status` completion, `interrupt_window`). `close_window` stays allowed.
7. **Keep the daemon/GUI process boundary.** The GUI never owns a PTY or a child process; it goes
   through IPC. The MCP bridge and any other client are thin layers over the same IPC.
8. **No `unwrap()` / `expect()` / `panic!` in production code.** Propagate errors (`thiserror` for
   typed errors, `anyhow` at the top). Tests may unwrap.

## Vocabulary

Use these words in code, docs and UI text.

| Term | Meaning | Not |
|---|---|---|
| window | One PTY session running any command | terminal, tab, agent |
| session | The restorable set of windows (commands, cwd, layout) | workspace, project |
| daemon | The resident process that owns PTYs, detection and the MCP side | server, backend |
| playbook | External config for approval, limits and workflow policy | workflow |
| vendor | The company behind an agent CLI | provider, model |
| room | A standing place where agents and the user post; not a window | channel, group |
| composer | A window's input area, where text is written before it is submitted | prompt, input box |
| handoff | A one-to-one message passing a reference and a note to another window | transfer, delegate |
| instance | One independent copy of yagura (its own endpoint, config, state and data) | profile, environment |
| plan, wave, role | A parent window's tasks; the dependency stages derived from them; an opaque key the user names in `roles.toml` | stage, phase |

Window states are fixed to seven: `Running`, `WaitingInput`, `WaitingApproval`, `Interrupted`, `Idle`,
`Exited`, `Unknown`. Core types store the English variant; UI strings come from
`gui/src/i18n/messages-en.ts`.

## Writing

- **Write only what a reader of this repository can understand.** No internal ticket numbers, no
  references to private planning notes or to the tools you used to do the work, no personal names,
  e-mail addresses or user-specific paths (use placeholders such as `C:\Users\someone`). This applies
  to code, comments, tests, test messages, docs, commit messages and the CHANGELOG.
- **Comments say why, briefly.** One or two lines on a non-obvious decision, invariant, workaround or OS
  quirk. Do not restate the code, tell history ("used to"), or defend the change to a reviewer. Do not
  write facts that rot with nearby edits: line numbers, counts, copies of code from other files. Refer
  to other code by identifier and file name. Public items get doc comments that state their error and
  panic conditions. `// SAFETY:` for `unsafe`, and a reason next to every `#[allow(...)]`. No
  commented-out code. `TODO` only with an issue number.
- **MCP tool names and arguments are a public contract.** Tool names are `snake_case`, verb then object
  (`open_window`, `send_chat`). Changing a tool name or argument, the IPC protocol or a stored schema is
  a breaking change: record it in the CHANGELOG and in the version.
- **Commits**: English, one purpose per commit, formatting separate from behavior. `CHANGELOG.md`
  describes what users see, in the present tense.

## Tests

- **Product code is right; tests follow it.** Add, change or delete tests to match the correct
  behavior. Do not bend a design to keep an existing test.
- **Test behavior by running it**: call the function, the module or the daemon and look at the result.
  Critical paths (state detection, PTY management, the MCP tools, the IPC boundary) also need the
  failure and edge cases: unknown output, timeouts, disconnects, malformed messages.
- **Do not write tests that only pin how code is written.** Reading source files as text is allowed
  only for a safety boundary (rendering of user-supplied strings, the IPC, approval and permission
  gates, resource limits) or an external contract (the IPC wire format, MCP tool names, names shared
  between Rust and TypeScript). Layout and wiring are checked by running the app.
- **Do not add a dependency only for tests.**
- **Do not synchronize with sleeps.** Wait on an event or a state the code exposes, not on wall-clock
  time.

## Fixing things

- Before fixing a defect or a review finding, write its root cause in one sentence. Pick the fix that
  removes that cause, even when it touches more code. Do not reword, wrap or swap mechanisms just to
  make a finding or a test go away.
- Confirm the cause against the source and a reproduction before you change code, and show afterwards
  that the cause is gone, not just that the check is green.
- If only a workaround is possible, say why and leave an issue that names the condition for coming
  back to it.
