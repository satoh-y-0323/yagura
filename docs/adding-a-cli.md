# Adding a CLI

This is the procedure for working out how yagura can hand text to a new agent CLI, before writing its
[launch profile](../profiles/README.md) and [detection definition](../crates/yagura-daemon/detection/README.md).
It checks, one route at a time, whether the CLI documents a way to receive text from outside, and
then measures where that text actually lands. What each route is for is described in
[How the daemon delivers text](mcp-setup.md#how-the-daemon-delivers-text); what you learn here fills
in the profile's [`[delivery]` table](../profiles/README.md#delivery).

## 0. Keep two things apart

- **What the CLI decides**: whether a piece of text reaches the model, in which role it arrives
  (system prompt, developer message, system reminder, user message), whether it starts a turn, and
  whether it can wake an idle session. These are the same on every run and can be checked against the
  CLI's documentation and its own session records.
- **What the model decides**: whether it follows text it was given, and whether it starts work nobody
  asked for. This changes with the model, its reasoning effort and its version.

Base decisions on what the CLI decides. How many times out of how many runs a model used a piece of
text is a tendency of that model; keep it as background, not as a reason.

Use only routes whose behavior is in the CLI's published documentation. A shape that is known only
from reading the implementation, or from another project's observation, can change without notice;
leave it out. If you do use a route the documentation marks experimental, say so in the profile's
comments.

## 1. List the candidate routes from primary sources

For each CLI, fill in the table below. In every cell write either "documented" with the URL and a
quotation, or "not documented". When the source is public, add the file and identifier too.

| Route | What to establish |
| --- | --- |
| Tell it at start-up | Whether an MCP server's `instructions` reach the model; whether a hook runs at session start (a `SessionStart` equivalent) and how its output is treated; flags or settings that add to the system prompt; where instruction files live |
| Wake an idle session | A command or hook that delivers text to an existing session from outside (the equivalents of Claude Code's background hook that wakes the CLI, or Codex CLI's `queue` subcommand) |
| Deliver while it works | Hooks that run in the middle of a turn or at its start, and whether their output can carry text into the next model call |
| Pass it per launch | Whether a flag, an environment variable or a settings-file path can carry it for one launch, or whether it can only live in the user's global settings or in a repository file |
| Relation to the user's settings | Whether what you pass adds to or replaces the user's own; whether there is a trust or approval gate before a hook runs |
| Limits | Characters, tokens and timeouts: of a hook's output, of a hook's run time, of a start-up request |

- The research can be delegated to an agent, provided it must give a quotation and a URL for every
  claim. Check any claim about source code yourself, by file and identifier.
- Read the CLI's `--help` and changelog as primary sources too.

## 2. Measure where the text lands

Take every route the table marks "documented" and measure it once per route per operating system
(Windows, macOS, Linux; for a CLI that runs in WSL, inside the distribution as well).

1. For each route, prepare a text with a marker word that differs from route to route (for example
   `MEASURE-HOOK`).
2. Open a window from a profile that uses the route and wait at least 20 seconds, recording the
   window's status over that time. If it stays `waiting_input`, the route did not start a turn.
3. As the user, ask once: "If the context you received at start-up contains a word starting with
   `MEASURE-`, say which one and where it was." This shows whether the model sees it, not where the
   CLI put it.
4. Look in the CLI's own session record for the marker word, and note where it is and in which role.
   The records are in the CLI's own files; examples as of writing (check the CLI's documentation for
   where yours keeps them):
   - Claude Code: `~/.claude/projects/<working directory>/<session>.jsonl` (the attachment type, and
     the role in the API request)
   - Codex CLI: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (the `role` of each `response_item`;
     tool definitions are not recorded)
   - Antigravity CLI: `~/.gemini/antigravity-cli/brain/<conversation>/.system_generated/logs/transcript.jsonl`
5. For a route that wakes an idle session, deliver to a waiting window and record whether its status
   changes to `running`, and the role the delivered text carries (a user message, or marked as
   something else). For a CLI that cannot be woken, deliver, watch the status for 20 seconds to
   confirm that it stays asleep, and check whether the text arrives at the start of the next turn.

Measure with a separate instance of yagura (see [Running a second instance](instances.md)) so that a
half-finished profile never touches your working one. Back up any settings file of the CLI's you are
going to change, and when you are done restore it and compare it byte for byte with the backup.

## 3. Pitfalls met while investigating the bundled CLIs

- A CLI started over SSH on macOS can come up logged out because the keychain is locked. Ask the
  user to unlock it, and run the measurement from that session.
- A CLI that asks whether to trust a new folder leaves the window in `unknown`, and nothing outside
  can answer it. Mark the measurement folder trusted beforehand in the CLI's own settings (when the
  file uses TOML, add the table after the top-level keys).
- On Windows, a CLI installed from npm starts through a `.cmd` shim, and arguments containing `"`
  and other characters the command interpreter reads cannot pass through it. yagura refuses them
  rather than guess; a route that must put arbitrary text on the command line needs the CLI's native
  executable (see [`.cmd` / `.bat` shims](../profiles/README.md#cmd--bat-shims)).
- A script that writes to standard error on Windows may use the console's code page. When a hook
  must print non-ASCII text, write its bytes as UTF-8 explicitly.
- Some CLIs keep the quotation marks of a hook's `command` and hand them on to the shell as they are.
  Test a command that contains a space.
- If the daemon is started from inside another agent CLI's shell, the CLI's environment variables are
  inherited by every window. Start the measurement daemon with them removed.
- The shared room `main` keeps its history between runs. Move the database aside before each run.
- Some CLIs take launch-time settings only as a directory of files: GitHub Copilot CLI reads
  per-launch hooks only from a plugin directory (`--plugin-dir`), not from one settings file. A
  profile writes such a directory with `config_dir` (see
  [MCP injection](../profiles/README.md#mcp-injection)).
- While the daemon types into a window, it tells the child that the window has the focus, but only when the CLI turned on focus reports (`?1004`). A CLI that changes its cursor or its drawing with the focus, such as GitHub Copilot CLI hiding the cursor without it, therefore draws as if focused for the length of a delivery, even in a window at the back; the view's focus is told again after the CLI's next output.
- A setting a CLI takes only by its path cannot reach a CLI that runs in another environment, such
  as WSL, as the daemon's own path. Give the profile a [`[host_paths]`](../profiles/README.md#host-paths)
  table so the daemon converts the path first, rather than inlining the content to avoid one.
- A CLI may read a hook's standard output as JSON as soon as it starts with `[` and fail the hook
  when it does not parse, and yagura's texts can start with `[yagura ...]`. It may also require a
  JSON answer to name the event it runs for. Give each door its own `output` template then (see
  [Delivery](../profiles/README.md#delivery)).
- A CLI may clear a hook's environment, and an MCP server's, down to a fixed list, so
  `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN` never reach them. Look for a setting that names the
  variables to keep (Muse Code's `managed_hooks_env_vars`, or a server's `env`), and print the
  hook's environment once to confirm it.
- A CLI's own launcher can lose arguments: Muse Code's Windows `muse.cmd` runs PowerShell 5.1,
  which drops `"` and `--`. Check what reaches the CLI itself, not what you passed, and record the
  result as a limit with guidance rather than leave the option to fail at launch (see the last
  item).
- A flag can be saved as the user's default: Muse Code writes `--reasoning-effort` into its
  settings file. Prefer a one-run override, and compare the settings file before and after a
  launch. Then check that the override takes effect with the user's settings as they are: Muse's
  `-c reasoning_effort=...` is ignored unless the settings already hold that key, which a probe
  run after the flag had been saved once did not show.
- A hook that ends with exit code 2 can block the user's prompt or keep the turn going. Make sure
  no door yagura's hooks call can exit 2 at that event.
- An Enter sent right after the text can add a line instead of sending (Muse Code needs about
  100 ms after the text shows). Measure the shortest gap that sends, and set `submit_delay_ms`
  well above it.
- A CLI can run hooks for its own sub-sessions after the turn's `Stop`: Muse Code's tool hooks
  fire for its internal reviewer too. A status hook on such an event leaves the window `running`;
  map only the events the main session alone runs.
- A settings file can accept an unknown key without complaint while the command line refuses it
  (Muse Code takes `tool_timeout_sec` in `settings.json` but not with `-c`). That a key was
  accepted is not proof that it works; measure its effect.
- A CLI can refuse to start when a launch argument touches a server its settings already define.
  Then a per-launch injection under the same name needs an `absent` guard (see
  [MCP injection](../profiles/README.md#mcp-injection)), and a registered one cannot be switched
  off for a single window.
- A hook a registration writes into the user's settings runs in every session of that CLI, inside
  a window or not. Put the check for the window in the hook's command itself, before the daemon is
  started, and on Windows have the same command stop `cmd` from searching the working folder for
  the daemon (see
  [Hooks that run outside a window](../profiles/README.md#hooks-that-run-outside-a-window)).
- When a CLI's launcher cannot carry a launch option or a start-up request on one host, the
  profile's `[shim_guidance]` says what to use instead; the daemon then refuses the request before
  approval and shows the limit ahead of time (see
  [Shim guidance](../profiles/README.md#shim-guidance)).

## 4. Turn the findings into a profile

- Each route you will use maps to a key of the profile's [`[delivery]` table](../profiles/README.md#delivery):
  the hook output's shape to `output`, the hook's output limit to `max_chars`, the way to wake an
  idle session to `wake` (and `wake_args` or the wait's exit code), the way to pass a start-up
  request to `launch_prompt`.
- A CLI with no documented way to wake an idle session gets `wake = "none"`. The daemon then types
  waiting text into its composer while the window waits for input, so its detection definition needs
  a composer and an `[input].probe` (see [Choosing `probe` for a CLI](../crates/yagura-daemon/detection/README.md#choosing-probe-for-a-cli)).
  Write down why, and what would let you drop the workaround (a documented way to wake the CLI).
- The hooks that call the doors go into the profile's `[hooks]` table, or a `json_entry` for a CLI
  that reads hooks only from its own files (see [Hooks injection](../profiles/README.md#hooks-injection)).
  If the CLI asks the user to trust hooks, the profile's `[mcp_registration]` has to record that trust.

## 5. Where to keep the results

Keep the table and the measurements in your project's own notes, each fact in one place. Put only
the conclusions that shape the design in the profile and its comments: which route is used, which
part is a workaround, and the condition under which to go back to the documented route.
