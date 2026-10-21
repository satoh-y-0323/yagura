# Changelog

The format of this file is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

This release changes how yagura hands text to a CLI, and with it several MCP and IPC values. Anything that reads them needs updating.

- **Text yagura sends of its own accord is no longer typed into the receiving CLI's input line.** Handoff envelopes, the chat announcement and chat posts, and the task, plan and report notices go into the window's inbox, and the CLI takes them through its own hooks, its start-up request, or a command that queues a message on its session. The one exception is a CLI whose profile declares no way to wake it (the bundled Antigravity CLI profiles), which is still typed while the window waits for input. `send_message` still types, as before. See [`docs/mcp-setup.md`](docs/mcp-setup.md#how-the-daemon-delivers-text).
- **`send_handoff`'s `delivery` is now `taken`, `undelivered_timeout` or `undelivered_no_delivery`.** `confirmed`, `unconfirmed`, `not_observed` and `not_written` are gone from this reply, so a client that matches them must change. `taken` says the receiving CLI took the envelope; `undelivered_timeout` says nothing took it within the new `[limits] handoff_delivery_timeout_secs` (default 120) and the envelope was withdrawn; `undelivered_no_delivery` says the window was not opened from a profile that declares how its CLI is handed text, and is answered before the approval decision. `not_written` remains an error code of `send_message`.
- **`read_chat`'s delivery states are now `pending`, `taken`, `undelivered_unknown`, `undelivered_halted`, `undelivered_exhausted` and `undelivered_restart`.** `confirmed`, `unconfirmed`, `not_observed`, `not_written` and `write_failed` are gone. A post to a window whose status is `unknown` now waits (`pending`) instead of being recorded `undelivered_unknown`, which now means the window ended first.
- **Chat delivery records saved by 0.1.0 are removed when the database is upgraded.** They are in the old vocabulary, which recorded what was typed into the input line rather than whether the CLI took the message, so they are not converted. The messages themselves are kept; posts made before the update no longer show a delivery state. The database now refuses a delivery state outside the current vocabulary.
- **A handoff that opens a new window from a `profile` is approved once, before anything starts.** One decision covers the start and the envelope, and the envelope is the CLI's first request; it used to be approved a second time after the window was ready. It skips the dialog only when both `open_window` and `send_handoff` are in `[approval].auto_approve`. There is no readiness wait any more: the reply is `taken` once the process is started, and `window_not_ready` is no longer returned: `yagura-mcp` no longer knows the code, so a daemon left running from before the update that still answers with it shows up as `internal`.
- **A profile must declare `launch_prompt` to be used for a handoff that opens a window or for a task window with an instruction**; otherwise the call is refused as `no_composer` before anything starts. Through a Windows `.cmd` / `.bat` file it is refused as `unsupported`.
- **Which windows take part in chat, and receive handoffs, is decided by the profile.** A window takes part when it was opened from a profile that declares the new `[delivery]` table and its hooks were put in place; a detection definition's `[composer]` and `[input].probe` no longer decide it. `[chat] intro` is now read by the CLI when it starts instead of being posted, and `chat_delivery_max` counts only chat posts being typed into windows. `[limits] handoff_ready_timeout_secs` now bounds only a typed write waiting for its window to finish starting up.
- **Launch profiles have a new `[delivery]` table**, and the bundled profiles declare it and add hooks for it. The table has no `session_pointer` or `wake_timeout_secs` key; writing either is an error. See [`profiles/README.md`](profiles/README.md#delivery).
- **The IPC protocol is now revision 23.** `ProfileInfo` carries `unavailable_kind` and `unavailable_guidance`, a structure of `text`, `command` and `url` (a profile's `[delivery.shim_guidance]` table; `text` is required, `url` is `https` only), and there is a new availability reason: a profile that wakes its CLI with a command is unavailable when its program is a Windows `.cmd` / `.bat` file.
- **The IPC protocol is now revision 24.** The daemon pushes the new `chat_participants_changed` message to every approver when a chat room appears or who takes part in one changes. A client that does not know it can ignore it.
- **On Windows, Codex CLI must be the standalone build, not the npm package.** The daemon wakes a waiting Codex CLI by passing text to `codex queue` on the command line, which the npm package's `.cmd` launcher cannot carry, so the `codex-cli` profile is listed as unavailable until the standalone build is first on `PATH`.
- **After updating, press Update for Codex CLI and Antigravity CLI in Settings > Integrations.** The bundled hooks changed, so the hook trust and the hooks entry recorded earlier no longer match; until they are written again, a Codex CLI window gets no hooks and an Antigravity CLI window is not handed text.
- **A window's name is now unique among the running windows.** Names are compared the way chat mentions are (case-insensitive, `-` and `_` the same), a window that has exited no longer holds its name, and the names chat reserves (`user`, `parent`, `all`) cannot be used. `open_window`, `open_task_window`, `send_handoff` (with `profile`) and renaming a window refuse a name that is taken with the new error `name_taken`, and a default name (a profile's display name, a command's file stem, a task's title) that is taken gets a number: `Claude Code`, `Claude Code 2`, `Claude Code 3`. The output of `open_window`, `open_task_window` and `send_handoff` carries the new `name` key, the name the window actually has. A client that does not know `name_taken` sees it as an unknown error code. A GUI older than this release restoring a session is refused for a window whose name a running window already carries.
- **In the app, restoring a session numbers a window whose name a running window already carries** instead of having it refused, and the Rename window dialog now waits for the daemon: it closes once the window has the new name, and otherwise stays open and shows why, so the name can be corrected in place. The Add a window dialog adds, under a taken name, that leaving the name empty gets a numbered one.
- **A `"` is no longer accepted in a window name**, a profile's `display_name` or a plan task's `title`, because it ends a quoted `@"..."` mention. A profile that already contains one stops loading until it is edited; a plan still loads, but a task whose title contains one cannot open its window without a `name`, and the plan cannot be updated until that title is changed. The label in a model listing and a registration `group` are not window names and still accept it.
- **The IPC protocol is now revision 25.** The open requests `open_window` and `open_window_from_profile` take `on_name_taken` (`refuse`, the default, or `number`), the error code `name_taken` is new, and `window_opened`, `task_window_opened` and `handoff_sent` carry `name`. Every addition is optional on the wire.
- **The IPC protocol is now revision 26.** The approver requests `clear_chat_room` and `delete_chat_room` (answered with `chat_room_cleared` and `chat_room_deleted`), the push `chat_history_cleared`, the error code `room_in_use`, and `ChatRoomInfo.has_open_windows` are new. A message's `seq` now keeps counting up when a room's history is cleared or the room is deleted and re-created.
- **A launch profile's `[mcp_registration]` table no longer takes `bridge_probe`; the conversion moved to the new top-level `[host_paths]` table, as `convert`, with `{path}` in place of `{bridge}`.** How a path is seen from the CLI's environment is one fact about the whole profile, so one table now serves both registration and launch. A profile of your own that still has `bridge_probe` is left out of the profile list with a warning until it is changed: remove the `bridge_probe` line from `[mcp_registration]` and add a table of its own, `[host_paths]`, holding `convert = ["wsl", "-e", "wslpath", "-u", "{path}"]` (your own command, with `{bridge}` written as `{path}`). The bundled WSL profiles already use it. See [`profiles/README.md`](profiles/README.md#host-paths).

### Added

- [`docs/adding-a-cli.md`](docs/adding-a-cli.md): how to check, route by route, how a new CLI can be handed text, and how to turn the result into a profile.
- **A launch profile's `[mcp]` table has two new placeholders.** `{server}`, in `args` and inside `config_file`, is the MCP server name the instance registers under; `{wslenv_toml}` is a `WSLENV` value naming the variables the bridge reads. See [`profiles/README.md`](profiles/README.md#mcp-injection).
- **A launch profile's `[mcp]` and `[hooks]` tables take a `config_dir`**, a table of file name to content that the daemon writes as one directory, named by `{mcp_config_dir}` / `{hooks_config_dir}` in `args`, for a CLI that takes its settings only as a directory. See [`profiles/README.md`](profiles/README.md#mcp-injection).
- **The paths a launch hands a CLI are converted too, when the profile has `[host_paths]`.** The settings file or directory an `[mcp]` or `[hooks]` table names, and the bridge `{bridge_toml}` names, now reach a CLI in another environment (WSL) as the path it can read. The conversion runs only when a launch names such a path, within the same slots and 10-second limit as the registration listing; when it fails, the window opens without that table, and a window left without its `[hooks]` this way is not expected to report and takes no part in chat or handoffs. See [`profiles/README.md`](profiles/README.md#host-paths).
- **A chat room's history can be cleared, and a room deleted, from the chat panel.** Open a room's ⋯ menu and choose Clear history or Delete room. Clearing removes every message in the room and takes back the messages its windows have not received yet; it also lifts a halt. A workspace room can be deleted once none of its windows is running, and comes back empty if a window in that folder uses chat again; the main room can only be cleared.

### Fixed

- A window whose profile's `[hooks]` table was dropped at launch -- no settings directory, settings that could not be written, or a Windows `.cmd` / `.bat` file that would reinterpret them -- still took part in chat and handoffs, so a handoff to it waited for a CLI that never runs the hooks until `undelivered_timeout`. Such a window now takes no part, and a handoff to it is answered `undelivered_no_delivery` at once, as for hooks left out because their registration was not in place.
- A Codex CLI window of a named instance could still see the default instance's registered `yagura` server, which Codex CLI starts without the window's variables, so it connected to the default instance's daemon as an outside client and could not say which window it was. When the daemon opens a Codex CLI window it now reads the CLI's registrations and turns off, for that window only, every yagura bridge registration except its own, through the new `[mcp_registration.list]` table of the bundled `codex-cli` and `wsl-codex-cli` profiles. Claude Code has no documented way to do this at launch, so its windows are unchanged.
- A Claude Code or Codex CLI window of a named instance with yagura registered from Settings > Integrations had two yagura MCP servers, and the registered one could not say which window it was, so its calls were not attributed to the window. The bundled profiles now name the injected server the way Register does (`yagura-<instance>`), so the two are one. Their tools are therefore listed under `yagura-<instance>` rather than `yagura` in such a window. A WSL Codex CLI window could not say which window it was in any instance, because Codex does not pass `WSLENV` on to the bridge; `wsl-codex-cli` now sets it.
- A second daemon started for an instance whose daemon was still starting up could exit as a failure and log that a non-daemon process held the endpoint, because it gave the first one 500 ms to answer a ping. It now asks the endpoint's lock instead, which a daemon takes when it binds and the system releases when it dies, so it exits as a success however long the first one takes. On Windows the lock is new, so a daemon of 0.1.0 still running when a newer one is started is now taken for a process that is not a daemon, and the newer one exits as a failure; the app still connects to the running one.
- A chat mention of a window did not reach anyone when two windows had the same name (opening two windows from one profile gave them both the profile's name), and nothing told the sender. Names are now unique, so a mention reaches the window it names, and a mention that matches no one in the room is now named in a notice above the chat input.
- A window opened after the chat panel was already open now appears in the `@` suggestions and the participant count right away, and leaves them when it ends or is renamed; before, it showed up only after someone posted. Choosing a suggestion with the arrow keys no longer snaps back to the first one when the key is released.
- On macOS, a chat mention picked from the `@` suggestions could reach no one: as the message was typed on, macOS turned the closing `"` of `@"Claude Code"` into a curly quote, which ends no mention. The app's text fields no longer have their quotes, dashes and words rewritten as you type, so mentions, commands, paths and names are sent as typed; they no longer show spelling marks either.
- A handoff or chat message to a window that took it without visibly starting work could be written into that window two or three times: a later attempt read the emptied input line as the text having been lost. Once the submit key has been pressed, yagura no longer writes the text again; it only presses the key again over text it still sees in the input line.

- A message or handoff that is being delivered no longer leaves yagura's own half-written text (including the `x` probe) in a window's input line when someone types, `send_keys` or interrupts meanwhile: the delivery takes its text back in front of that input, writes nothing more, and checks the line before it answers. Text typed in front of a waiting notice no longer sets off a loop of probes that kept the notice from arriving. Where the check cannot confirm the text gone, the call still reports that something may remain.
- A `send_message`, a handoff or a chat message could reach a CLI while it was still drawing an approval dialog, so its keys could answer the dialog (an Enter could approve it). yagura now writes into a window only once the CLI has finished drawing its screen -- the cursor is shown again and no synchronized update is open -- and still never writes on an approval dialog. A CLI that keeps the cursor hidden while its input line sits idle receives nothing from yagura.
- A `send_message`, a handoff or a chat message whose text ended in a word starting with `@` (`please review @src`, or a chat message ending in `@user.`) opened the input completion of Codex CLI and Antigravity CLI, and the delivery could stall there with the text and the probe left in the input line. yagura now ends every text it types and submits with one space of its own, so the cursor never rests on the end of the sender's text.
- On macOS 26, the app icon fills the rounded square like other Mac apps instead of sitting small on a gray plate. If the Dock still shows the old icon after you update, remove yagura from the Dock (Options > Remove from Dock) and open it again.
- Registering Antigravity CLI in Settings > Integrations no longer fails with "no path to the daemon can be written" when the daemon's folder has a space in its path: on macOS the daemon copy that CLI hooks run now lives in `~/.local/state/yagura/bin` (or under `$XDG_STATE_HOME`) instead of `~/Library/Application Support`, and on Windows a path with a space is written by its 8.3 short name; where there is none, the registration fails with a message that says so.
- Commands run once and the detached daemon no longer inherit stray file descriptors or handles from the process that starts them, so a one-shot command no longer waits for an unrelated child started at the same moment to exit. On Windows the daemon is now started through the same explicit handle list as other commands, rather than through the standard library's process launcher.

## [0.1.0] - 2026-10-13

The first public release of yagura: a desktop app that runs any command -- an agent CLI such as
Claude Code, Codex CLI or Antigravity CLI, or a plain shell -- in its own window, watches each
window's status, and lets agents in those windows work with each other through an MCP server.

### Added

#### Windows and layout

- Each window is a pseudo-terminal running any command you choose, drawn in a floating frame you can drag, resize from any edge or corner, minimize, maximize and close.
- Maximizing a window switches to a tab bar: click a tab to switch windows, drag it to reorder, and restore to get every window back where it was.
- The Tile windows button in the sidebar header lays every window out in an even grid; each room heading can also tile, minimize or restore just that room's windows.
- The sidebar lists windows grouped by room (the git repository or directory a window runs in), in a fixed order that does not jump around when a status changes.
- Each row shows the window's name, status, vendor and working directory, with rename, interrupt and close buttons; the interrupt button is shown disabled when interrupting is not allowed.
- A badge counts the windows waiting for you; pressing it jumps to the next one.
- The new-window dialog opens a window from a launch profile, or from a command, working directory, name and detection definition of your own; a Browse button picks the working directory.
- Each window's menu offers a per-window theme and rename.
- Every button that shows only an icon names itself in a tooltip on hover, including the side rail's Windows, Plans, Chat and Settings buttons and every close button.
- A window shows what the daemon's model of its terminal holds -- the same text `read_output` and status detection read -- so the window, its text and its status agree after a resize, with wide characters, on the alternate screen and on the cursor's line. The GUI only draws the cells the daemon sends; box-drawing and block characters are drawn to fit the cell so their lines join.
- The mouse wheel, or `Shift+PageUp` / `Shift+PageDown`, scrolls back through a window's history, unless the program in the window takes the wheel itself. Typing or pasting brings the window back to the bottom.
- Text an input method is still composing is drawn underlined inside the line, at the cursor.
- In a window, `Shift+Enter` inserts a newline in an agent CLI's input, and on Windows `Ctrl+V` pastes.
- Copy and paste follow each platform's terminal convention: `Ctrl+Shift+C` / `Ctrl+Shift+V` on Windows and Linux, `Cmd+C` / `Cmd+V` on macOS. A right-click copies the selection or pastes on Windows, and opens a Copy / Paste menu on macOS and Linux. Copying clears the selection, so the next right-click on Windows pastes. Drag selects, a double-click selects a word and a triple-click a line. When the program in a window captures the mouse, clicks and the right-click go to that program; holding `Shift` brings back yagura's handling.
- Elsewhere in the app, a right-click does not open the web view's own menu, whose Reload item would throw away every open window's view; text fields keep their operating system's cut / copy / paste menu.
- A window's program is told it runs in yagura's terminal (`TERM=xterm-256color`, `COLORTERM=truecolor`), whichever terminal yagura was started from: variables a terminal sets to name itself, such as `TERM_PROGRAM` or `VSCODE_*`, are not passed on, so a CLI does not, for example, turn off right-click paste because it thinks it is inside VS Code. A profile's `[env]` can still set any of them.
- The daemon answers a program's terminal queries itself (device attributes, status and cursor position reports, mode queries), so a program gets its answers even while no GUI shows the window. Color queries are answered with the window's theme colors once the GUI has opened the window.
- Closing or interrupting a window from the GUI asks for confirmation first; closing ends the window's whole process tree.

#### Status detection

- Every window reports one of seven statuses: `running`, `waiting_input`, `waiting_approval`, `interrupted`, `idle`, `exited` and `unknown`, shown in the sidebar, the title bar and the tab bar.
- Status comes from detection definitions (TOML) that read the window's screen. Definitions for `claude-code`, `codex-cli`, `antigravity-cli` and `generic-shell` are built into the daemon; your own go in the `detection` directory under the config root and override a built-in one of the same name. See [`crates/yagura-daemon/detection/README.md`](crates/yagura-daemon/detection/README.md).
- Detection is fail-safe: when a screen cannot be read with confidence, the window is `unknown` rather than guessed, and interrupting, typing text and chat delivery into it are refused. Closing an `unknown` window is always allowed.
- An `unknown` window says why (`no_definition`, `no_rule_matched`, `screen_lost`, `detector_stopped`, `not_yet_judged` or `declared_unknown`), in the GUI and in MCP replies.
- Approval prompts and choice dialogs of the bundled agent CLIs are detected as `waiting_approval`.
- Text being typed in an agent CLI's input line is left out when its status is judged, so a draft that quotes a prompt or a spinner line does not change the status.
- Status is judged only on a screen the CLI has finished drawing, so a repaint split across reads does not make the status flicker; the composer is read the same way before a write.
- Codex CLI shows `running` while its composer holds a draft during a turn, not only while its `Working` line is drawn.
- Antigravity CLI keeps its status while its composer holds a draft with blank lines, including a draft folded under `↑ N more lines`. A definition opts into reading blank lines as part of the body with `blank_lines_in_body` in `[composer]`; declare it only for a CLI whose dialogs hide the cursor.
- Windows opened from the bundled Claude Code, Codex CLI and Antigravity CLI profiles have the CLI report the start and end of each turn through its own hooks (`yagura-daemon --emit-status`, authenticated with a per-window token). A window then shows `running` while a reply streams under an input line that looks idle, and `waiting_approval` when the dialog is too small to read. A screen with a clear status still wins, and a window with no reports is judged by its screen alone. See [`crates/yagura-daemon/detection/README.md`](crates/yagura-daemon/detection/README.md#structured-sources---emit-status).
- Codex CLI and Antigravity CLI report once yagura is registered with them in Settings > Integrations, which also writes the hook settings they need (Codex CLI's hook trust in `~/.codex/config.toml`, a `yagura` entry in `~/.gemini/config/hooks.json`) and takes them out again on Remove.
- A report stands until the next one; `--emit-status` takes only a status and `--source`, with no time-to-live. A turn with no output for 5 seconds is taken as over, which covers the interrupts and denied permissions Claude Code sends no hook for. A turn with no report for 2 minutes is taken as over as well, so a screen that keeps redrawing itself cannot hold it open.
- A window whose CLI was given hooks but has sent no report 10 seconds after its screen shows a running turn gets a yellow mark beside its plug icon, and the daemon logs one warning -- the sign that a CLI update changed its hooks.
- `--emit-status` does nothing and exits successfully outside a yagura window, so a hook a CLI runs in every session stays quiet in sessions yagura did not open.
- A `status_event` must name its window: one without `window_id` is refused as `invalid_argument`.

#### MCP server and tools

- `yagura-mcp` is a stdio MCP server that lets an agent control yagura. Its tools are:
  - Windows: `list_windows`, `get_window_status`, `open_window`, `close_window`, `interrupt_window`, `wait_for_status`, `read_output`
  - Input: `send_message`, `send_keys`
  - Setup: `list_detections`, `list_profiles`
  - Handoff and chat: `send_handoff`, `send_chat`, `broadcast_chat`, `read_chat`
  - Plans: `create_plan`, `update_plan`, `get_plan`, `list_plans`, `delete_plan`, `list_roles`, `open_task_window`
  - Reports: `send_report`, `wait_for_report`
- `read_output` and the status summary return a window's text as the window shows it: a line longer than the window comes back whole, a line redrawn in place reads as its last state, and while a full-screen CLI is running only its current screen is returned.
- `send_message` sends a single line of text; `send_keys` sends up to 16 named keys (`escape`, `backspace`, `ctrl_u`, `enter`, `up`, `down`). The arrow keys follow the program's cursor key mode, as a key pressed in the window does.
- The daemon never types text into a window whose definition has no input line (such as a plain shell); `send_message` and `send_handoff` to it are refused as `no_composer`. A definition that has an input line but no `probe` in `[input]` gets no line of the daemon's own either: a one-line `send_message`, a `send_handoff` and a task instruction are refused there as `no_composer`, and no notice or chat message is typed there, while `send_message` text without a newline is still typed. The definition still loads and the window's status is still detected; adding a `probe` is all it needs.
- `yagura-mcp` runs up to four tool calls at once, each over a connection of its own to the daemon, so a call waiting on a stalled window, an approval or `wait_for_status` does not hold up the agent's other calls; a call beyond that waits for one to finish.
- Cancelling a tool call from the MCP client (`notifications/cancelled`) ends it at once: `yagura-mcp` answers with an error and drops that call's connection to the daemon, which the daemon treats as the requester leaving -- a pending approval is withdrawn, and a message that has not shown yet is never submitted and is taken back where the definition has a `clear` key. A requester that disconnects is treated the same way.
- A CLI that drops a tool call still out when it ends its turn, without cancelling it, is caught by its window instead: when the requester's detection definition sets `[requests] abandoned_at_turn_end` (the bundled Codex CLI definition does), its `send_message` or `send_handoff` is answered `not_written` at once when its window, having been `running`, comes to rest at `waiting_input`, `interrupted`, `idle` or `exited`, and the text is taken back as for a cancelled call.
- A `send_message` waiting for its text to show does not hold up the daemon's other replies on the same connection; it counts toward that connection's limit on pending waits, as `wait_for_status` does.
- `send_keys` is written at once, even into a window where a message is waiting to show, so it can dismiss a dialog that holds a delivery up; the waiting delivery then stops, as it does when you type into the window.
- Before writing, the daemon checks the window's screen, so it never presses Enter into an approval prompt or on top of text someone else is typing.
- Windows yagura opens from the bundled Claude Code and Codex CLI profiles can use the tools without any MCP registration in the CLI; a plug icon shows which windows are connected.
- For sessions yagura did not start, Settings > Integrations registers or removes the bridge with each CLI through the CLI's own `mcp add` / `mcp remove`. See [`docs/mcp-setup.md`](docs/mcp-setup.md).

#### Approvals

- Destructive tools (`close_window`, `interrupt_window`, `send_message`, `send_keys`, `send_handoff`, `send_chat`, `broadcast_chat`) called by another window or an external client wait for your decision in an approval dialog. A window acting on itself, and the GUI itself, are not gated.
- Starting a window is gated too (`open_window`): an agent that asks to open a window -- with `open_window`, with `send_handoff` from a profile, or with `open_task_window` -- can run any command as you, outside its own CLI's permission prompts. The approval dialog shows the command (or profile) and its working directory. The shipped default lets agents start windows without asking; switch **Open a window** off in Settings > Approvals to be asked each time. A request that could not open anyway (an empty command, a missing directory, the window limit) is refused before anyone is asked, the command is shown with each argument quoted where its boundaries would be unclear, and a request waiting for your decision is withdrawn when the agent drops the call.
- The approval policy comes from `playbook.toml`, which the GUI places in the config root on first run and never overwrites. The shipped default auto-approves only `send_chat` and `open_window`.
- A chat post from a connection that has not identified as a window follows the same switch as any other: it waits for your decision unless `send_chat` / `broadcast_chat` is auto-approved.
- Settings > Approvals turns each tool's auto-approval, and the plan review requirement, on or off without restarting the daemon.
- A request nobody decides on within 60 seconds (configurable) is refused.
- If the playbook cannot be loaded, every gated request is refused and chat is turned off.

#### Handoff, chat rooms and reports

- `send_handoff` passes a reference and a note to another window, either an existing one or a new one opened from a launch profile, and reports whether the receiver reacted (`confirmed`, `unconfirmed`, `not_observed` or `not_written`). The receiver can reply with the same tool.
- Whether a message has shown in the receiver's input line is decided by the order the CLI handles its input, never by a timer: yagura types a harmless character (a detection definition's `probe` in `[input]`, `x` in the bundled ones) behind the message, and presses Enter only once the message shows with that character behind it and the character has been erased again (with `erase`, DEL by default). A slow or heavily loaded machine therefore never has a handoff, chat message, notice or `send_message` given up on because it took long to show, and a long envelope that wraps over several lines is checked the same way. A CLI that dropped the message -- the character shows alone -- gets `not_written`, with nothing left behind. `[` cannot be a `probe`, since every line yagura writes on its own starts with it; the detection README explains how to check a character for a new CLI.
- A CLI that stops handling its input -- at a dialog its definition does not know, for instance -- keeps a delivery into it waiting instead of having it abandoned on a timer. Typing into the window, `send_keys`, `interrupt_window`, `close_window` or cancelling the tool call ends the wait. See [`docs/mcp-setup.md`](docs/mcp-setup.md#send_handoff).
- A message whose requester leaves before it shows is not submitted but taken back, and the take-back is confirmed by the same character: it counts as done only once the character shows alone and is erased again. A busy or stalled CLI therefore never has its text reported gone before it has handled it. When the take-back cannot be confirmed, the sender is told the text may remain, and it is never written a second time.
- A window that has stopped handling its input holds at most one share of the daemon-wide chat delivery budget and one daemon-wide handoff slot, however many deliveries queue for it, and at most 8 deliveries may wait for one window's turn; one past that is refused (`resource_exhausted` for a handoff, `undelivered_exhausted` in chat) without affecting deliveries into other windows.
- `reflect_wait_ms` in a definition's `[input]` has no effect; a definition that sets it still loads.
- How long a delivery waits, how a cancelled call ends and how many calls `yagura-mcp` runs at once change no MCP tool name, argument name or IPC wire format.
- A message never gets mixed into something you are typing: before writing into an input line that looks empty, yagura types the same probe character and writes only once it shows alone, so a draft you just typed or pasted that the CLI has not drawn yet is left alone instead of being sent along with the message. A re-send over its own earlier text does the same when anyone else has typed into the window since. A write still waiting to show stops as soon as someone else types into the same window.
- Moving the focus between windows, or the mouse over a window, does not interrupt a delivery into it, even when its CLI asked for focus or mouse reports.
- The click that selects a window is not passed to a CLI tracking the mouse; the next click is. A click in the window you are already in reaches the CLI, and a delivery waiting there stops as it does when you type.
- A delivery that replaces its own earlier, unsubmitted text with a different body takes that text back even when the CLI draws it folded (for example as a `[Pasted text]` placeholder), as long as nothing else has been written into the window since; the take-back is confirmed by the probe as usual.
- Chat rooms let agents and you post to each other with `@name`, `@"a name with spaces"`, `@parent` and `@user` mentions. Each workspace has its own room, and `main` reaches every window.
- A chat post must carry at least one visible character: a body of only spaces, line breaks or zero-width joiners is refused as `invalid_argument`, and the error tells the agent that not posting is how to send no reply.
- A room halts on loops, monologues or too many turns since your last post, and asks for you; only your post resumes it.
- The Chat panel shows the room list, participants, per-recipient delivery state and a halt banner, and has `@` completion and a post-to-everyone button. The rail's Chat button shows a badge when a room needs you.
- A window can report how its work ended (`send_report`) to the window that opened it; the parent reads reports with `wait_for_report` and gets a one-line notice when reports are waiting.

#### Plans

- A window can hand yagura a plan of tasks with dependencies and write targets. yagura derives the waves, rejects cycles and tasks that could write the same path at the same time, and opens each task's window with the profile, model and effort assigned to it. See [`docs/plans.md`](docs/plans.md).
- Assignments come from your override, the plan's own proposal, or `roles.toml` (with an optional `default_role`), in that order.
- A task can carry a multi-line instruction; its window gets a one-line notice and reads the instruction with `get_plan`.
- A new plan waits for your approval before any of its tasks can open, unless `[plans] require_approval = false`; later changes to an approved plan are marked as new or changed for you to review.
- The Plans panel has a Board tab (waves, assignments, task windows, reports, approve, reassign, hand over or delete a plan) and a Roles tab.
- Plans are kept across daemon restarts.

#### Launch profiles and models

- Launch profiles (one TOML file each) save a command per OS, detection definition, working directory and environment. Profiles for Claude Code, Codex CLI, Antigravity CLI, their WSL versions and a plain shell ship with yagura; a file in the config root's `profiles` holds only what you change, so a new version's defaults still reach you. See [`profiles/README.md`](profiles/README.md).
- A profile whose command does not exist for the current OS is not listed; the WSL profiles appear only on Windows.
- A profile can map `model` and `effort` aliases (such as `fast`, `default`, `strong`) to CLI arguments, chosen in the new-window dialog or through MCP.
- The Plans panel's Models tab edits each alias, offering the models the CLI itself lists, and can put a shipped value back.
- Settings > Profiles shows every value a profile uses, marked Shipped or Changed, and edits the command for the current OS one argument per field, with a reset to the shipped command.
- Settings > Profiles turns a profile off; a disabled profile is hidden and refused as `profile_disabled`.
- On Windows, npm `.cmd` / `.bat` shims are run through `cmd.exe` for you.
- On macOS and Linux, a daemon started outside a terminal gives windows the login shell's environment and a UTF-8 locale, so CLIs installed through Homebrew, nvm or `~/.local/bin` are found.
- No bundled profile turns on a CLI's flag for skipping its own approval prompts.

#### Themes and settings

- Dark (default), Light and System themes, plus your own in `themes.toml` (35 color keys per theme). The rail's theme control shows the current theme as an icon. See [`docs/themes.md`](docs/themes.md).
- A theme can be set per window or per vendor; changes apply at once, including open terminals.
- The Settings, Plans and Chat panels can be resized and reopen at their last size; the Chat panel's room list can be widened.

#### Keyboard

- Twelve operations have shortcuts: next and previous window, next window needing you, new window, close, interrupt, minimize, maximize or restore, moving between the sidebar and a window, and the Chat, Plans and Settings panels. Windows and Linux use `Ctrl+Shift+` keys; macOS uses `Cmd`.
- Settings > Keyboard changes, resets or removes a key, saved to `keymap.toml`. Keys that would take input away from the programs in your windows, or that the system owns, are refused with a reason. See [`docs/keymap.md`](docs/keymap.md).
- `Esc` closes the Chat, Plans or Settings panel, and focus returns to where you were.
- Keys that would reload the GUI are turned off, and on Windows the web view's browser keys (print, find, zoom) are turned off.
- A key an input method is still using is never taken as a shortcut.

#### Sessions and the daemon

- The daemon keeps windows running in the background and outlives the GUI: closing the GUI leaves it running. After upgrading yagura, stop it with `yagura-daemon --shutdown` so the new version starts; restarting the daemon closes every window.
- The daemon starts with no window open: every window is one you, a restored session or an agent opens.
- A window opened without a working directory starts in your home directory on every platform, however the daemon was started. If the home directory cannot be found, it starts in the daemon's own directory and the daemon logs a warning.
- On the next start, the GUI offers to restore the windows of your last session with their layout; windows opened from a profile are reopened from that profile.
- If the daemon disappears while you have windows open, the GUI starts it again (with backoff) and offers a Reconnect button when it gives up. The windows that were running are lost with the old daemon. See [`docs/client-config.md`](docs/client-config.md).
- `YAGURA_INSTANCE` runs a second, fully separate instance (for example a development build) beside your everyday one. Any identifier the rule accepts, up to 16 bytes, works on every platform, including macOS with its long temporary directory. See [`docs/instances.md`](docs/instances.md).
- A window the GUI reconnects to comes back whole from the daemon's model of its terminal: its history and screen with colors, wide characters, the alternate screen and the cursor. The GUI names the size it draws at when it attaches, and again when it reconnects, so the window and the view agree on it after the daemon was started anew.
- A GUI does not show windows from a daemon older than itself: it says so in a banner and asks you to restart the daemon with `yagura-daemon --shutdown`.
- When the daemon cannot open windows because the console host bundled with yagura is missing or damaged, it keeps running and the GUI says why in a banner. Daemon clients can ask for the same list with the `get_daemon_health` request.

#### Platforms and installers

- Windows 11, macOS and Linux. Windows 10 is not supported.
- On Windows, every window runs under the ConPTY that ships with yagura (`conpty.dll` and `OpenConsole.exe` from Microsoft's `Microsoft.Windows.Console.ConPTY` 1.25.260930003, MIT) rather than the one built into Windows, so a resize follows the same rule as on macOS and Linux. If those files are missing or altered, no window opens and a banner asks you to reinstall.
- Windows: a per-user NSIS installer (no administrator rights) that bundles `yagura-daemon`, `yagura-mcp` and the ConPTY above, and installs WebView2 when missing (needs network access). It is not code-signed, so SmartScreen warns on first run. The program goes in `%LOCALAPPDATA%\yagura` and your settings, session store and logs in `%LOCALAPPDATA%\io.github.satoh-y-0323.yagura`, which the uninstaller removes when you choose to delete the app's data.
- macOS (Apple silicon): a `.dmg` whose app, `yagura-daemon` and `yagura-mcp` carry an ad-hoc signature. It is not notarized, so the first launch needs **Open Anyway** in **System Settings > Privacy & Security**.
- Linux: a `.deb` for Debian and Ubuntu that installs the GUI, `yagura-daemon` and `yagura-mcp` into `/usr/bin`.
- Other systems and Intel Macs: build from source.
- WSL: the WSL profiles run a CLI inside your default distribution through `bash`. See [`docs/wsl.md`](docs/wsl.md).

### Security

- The daemon's endpoint accepts connections only from the same OS user. Only the GUI executable the daemon was started with can become the approver.
- The GUI, `yagura-mcp` and the daemon's own commands (such as `--shutdown`) connect only to a daemon running as the same OS user, so on a shared machine another user cannot stand in for yours by taking its endpoint name first.
- Any MCP client connected to yagura can read the recent output of every window. Do not run terminals that show secrets as yagura windows.
- Handoff envelopes, chat messages and reports are delivered as data from another agent; a receiving agent should treat them as untrusted input, not instructions.
- The session database stores each window's command line and working directory, chat messages, plans and their reports without encrypting them. Do not put secrets in a window's command line.
- Pasted text cannot end a program's bracketed paste early: yagura removes the paste markers from it before writing.
- A window's status is advisory: another process of the same user can forge it, so do not base security decisions on it alone.

[Unreleased]: https://github.com/satoh-y-0323/yagura/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/satoh-y-0323/yagura/releases/tag/v0.1.0
