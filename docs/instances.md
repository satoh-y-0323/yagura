# Instances

This is the single specification for yagura's instance mechanism. Where another document (for
example `docs/client-config.md`, `docs/themes.md`, `docs/keymap.md`,
`crates/yagura-daemon/detection/README.md`, `profiles/README.md` or the root `README.md`) shows a
concrete path, that path is the **default instance's** path; this document is where the rule that
derives every instance's path lives.

## What an instance is

One yagura instance owns one IPC endpoint plus one config / state / data directory. Running two
instances at once -- most commonly a work instance (an installed build, used for real sessions)
alongside a development instance (`target/debug`, rebuilt while you work on yagura itself) -- keeps
their windows, their session store and their logs from colliding. Without this separation, starting
a second `yagura-daemon` either fails to bind the one endpoint name that already exists, or, worse,
silently shares the first daemon's session store and discards its previous-session snapshot on
every start.

## Naming an instance: `YAGURA_INSTANCE`

Set the environment variable `YAGURA_INSTANCE` before starting `yagura-daemon`, `yagura-mcp`, or the
GUI. There is no command-line flag for this: an instance is carried by the process's environment
alone, so every descendant a daemon spawns (a window's shell, and anything `yagura-mcp` that shell
in turn starts) inherits the same value without yagura injecting anything -- the same mechanism that
carries `YAGURA_STATUS_TOKEN` and `YAGURA_WINDOW_ID` to the `yagura-mcp` of a window whose profile keeps
its credentials in the environment (a window that hands them over in a file does not use it for those two;
see [Credentials](../profiles/README.md#credentials)).

An identifier is accepted when it is **1 to 16 bytes of lowercase ASCII letters (`a`-`z`), ASCII
digits (`0`-`9`) and hyphens (`-`), and starts and ends with a letter or a digit** (a hyphen at
either end is rejected). This is an allow-list, not a blocklist: path separators, `.`, `..`, NUL,
whitespace, the characters Windows reserves in file names, and every non-ASCII byte are rejected
because they are outside the allowed set, not because each is checked individually. Uppercase is
rejected too, even though the character itself is otherwise harmless: Windows pipe names are
case-insensitive but Linux directory names are not, so allowing `Dev` and `dev` to both resolve
would let two instances that think they are distinct share one endpoint while keeping separate
stores.

Leaving `YAGURA_INSTANCE` unset selects the **default instance**, whose endpoint, config, state and
data paths use the plain name `yagura` (see the table below). Setting it to `""` (an empty string)
is **not** the same as leaving it unset: it is rejected as an invalid identifier. This is a deliberate asymmetry
with `YAGURA_PLAYBOOK`, `YAGURA_PROFILES_DIR` and `YAGURA_DETECTION_DIR`, which treat an empty value
as unset -- if `YAGURA_INSTANCE=""` rounded silently to the default instance, a terminal that meant
to start a named instance would instead touch the default one's resources without any indication
that it had done so, which is exactly the accident this mechanism exists to prevent.

## What differs between instances

Every position below is derived from the same rule: wherever the name `yagura` appears as a
directory name, a pipe name, or a socket name, it is replaced by `yagura-<id>` for a named
instance -- except inside a leaf file name, which never changes.

| Resource | Default | `YAGURA_INSTANCE=dev` |
|---|---|---|
| Windows named pipe | `\\.\pipe\yagura-{user_id}` | `\\.\pipe\yagura-dev-{user_id}` |
| Unix socket (primary path) | `{dir}/yagura.sock` | `{dir}/yagura-dev.sock` |
| Unix socket (fallback directory) | `{tmp}/yagura-{user_id}/ipc.sock` | `{tmp}/yagura-dev-{user_id}/ipc.sock` |
| Unix endpoint lock (beside the socket) | `{socket}.lock` | `{socket}.lock` |
| Config root | `<config_base>/yagura` | `<config_base>/yagura-dev` |
| State root (logs) | `<state_base>/yagura/logs` | `<state_base>/yagura-dev/logs` |
| Report folder (the daemon copy CLI hooks run) | `<report_base>/yagura/bin` | `<report_base>/yagura-dev/bin` |
| Data root (session store) | `<data_base>/yagura/yagura.db` | `<data_base>/yagura-dev/yagura.db` |

In the fallback directory the socket is always `ipc.sock`: the directory already names the
instance, and macOS's long `TMPDIR` leaves no room to repeat the identifier within the socket
path's 104-byte limit.

The `yagura.db` file name itself never changes -- it is a leaf file name, not a namespace
directory -- so what changes between instances is which directory holds it, not what it is called
inside that directory. `<config_base>`, `<state_base>` and `<data_base>` are the platform's own
per-user directories (`%LOCALAPPDATA%` on Windows for all three; `~/Library/Application Support` on
macOS; `$XDG_CONFIG_HOME` / `$XDG_STATE_HOME` / `$XDG_DATA_HOME`, each falling back to a directory
under `~`, on other Unix) and are the same for every instance -- only the namespace directory under
them changes. `<report_base>` is `<state_base>` everywhere except macOS, where it is
`$XDG_STATE_HOME`, falling back to `~/.local/state`: a CLI cannot run a hook command whose path
holds the space in `Application Support`.

On Windows the three directories use the app's bundle identifier in place of `yagura`:
`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura` for the default instance and
`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura-dev` for `YAGURA_INSTANCE=dev`. The installer puts
the program itself in `%LOCALAPPDATA%\yagura`, so the two never share a folder, and the
uninstaller's option to delete the app's data removes the default instance's directory. The pipe
names keep `yagura`.

Two instances of the same user therefore never overlap: nothing under a named instance's config,
state or data root, and no name it binds, is one the default instance -- or any other named
instance -- also uses.

## Relationship to the existing environment variables

Three existing variables -- `YAGURA_PLAYBOOK`, `YAGURA_PROFILES_DIR` and `YAGURA_DETECTION_DIR` --
are read-only inputs: a daemon reads them once at start-up and never writes through them. Sharing
one of them between a work instance and a development instance is harmless, and **works the same
for both the default and a named instance**: if you want two instances to
use the same playbook or the same profiles directory, exporting the variable before starting either
one still does that. A profiles directory given this way answers alone: the bundled profiles are not
laid under it (see [Where profiles live](../profiles/README.md#where-profiles-live)).

`YAGURA_STORE_DB` is different: it names the SQLite database file the daemon opens, and opening it
is not read-only. **A named instance refuses to start while `YAGURA_STORE_DB` is set.** Two daemons
that opened the same database would each hold what looks like an exclusive session store, and the
one that started later would discard whatever previous-session snapshot the one that started first
had saved -- the exact kind of silent data loss instances exist to prevent. The rejection applies
only to a named instance; for the default instance, `YAGURA_STORE_DB` still takes precedence over
the default database path.

"Set" means the variable has a value at all: an **empty string, and a value that is not valid
UTF-8, both count as set**, matching the rule the store path resolution itself already applies when
deciding whether to adopt an override. This is deliberate -- if the two judgments disagreed, a
daemon could refuse to start based on one rule while a value that rule considered unset was still
adopted by the other, letting the very conflict this rejection exists to prevent through anyway.

This rejection applies only to `yagura-daemon`'s `Run` command and to the GUI (which starts a `Run`
daemon on your behalf). **`--shutdown` and `--emit-status` are never subject to it**, because
neither opens the session store -- the check exists to protect a resource those commands never
touch. Without this exception, a terminal with both `YAGURA_INSTANCE` and `YAGURA_STORE_DB` set
could start a development instance's daemon (as an accident) but then be unable to stop it again,
since `--shutdown` would refuse for the same reason `Run` did. The rejection's own remedy is
simple either way: unset `YAGURA_STORE_DB` in that terminal and start yagura again.

## Behavior on an invalid identifier

When `YAGURA_INSTANCE` is set but the value is not a valid identifier, no process falls back to the
default instance -- doing so would let a mistyped variable silently touch the wrong instance's
resources, which this mechanism exists to prevent. What each process does instead differs, because
of how each one reaches the person running it:

| Process | Behavior |
|---|---|
| `yagura-daemon` | Writes the reason to stderr and exits with a non-zero status. It never binds an endpoint and never touches the default (or any) config, state or data root. |
| `yagura-mcp` | Same as `yagura-daemon`: writes the reason to stderr and exits non-zero, without connecting to any daemon. This matches the existing fail-closed behavior when the current user's identity cannot be resolved. |
| GUI | **Starts.** A release build's GUI runs with no console (`windows_subsystem = "windows"` on Windows), so stderr reaches nobody; exiting silently would leave the person who started it looking at nothing happening for no visible reason. Instead, the GUI opens with a banner explaining what is wrong and how to fix it, and does none of the following: it does not spawn or connect to a daemon, it does not read or write the default instance's config, state or data root, and its window title stays the product's own name (it is not the invalid instance's, since none was resolved). |

None of the three ever echoes the rejected value back -- not to stderr, not to a log, and not to
the GUI's banner. What each of them states instead is the rule the value broke (for example, which
characters are allowed) and how to fix it: correct the value, or unset `YAGURA_INSTANCE` to use the
default instance, then start yagura again. The value itself came from an environment variable you
set yourself, and remains readable there (for example with `echo $env:YAGURA_INSTANCE` in
PowerShell) if you need to see it again.

## Working and development instances

The pattern this mechanism is meant for: run an **installed** build of yagura as your working
instance for real sessions, and a **`target/debug`** build as your development instance while you
work on yagura itself.

```powershell
# work instance: the installed build, unset YAGURA_INSTANCE (the default instance)
# started normally, e.g. from the Start menu

# development instance, from a separate terminal
$env:YAGURA_INSTANCE = 'dev'
.\target\debug\yagura-gui.exe
```

On macOS and Linux, the development instance is started the same way from a shell:

```sh
YAGURA_INSTANCE=dev ./target/debug/yagura-gui
```

Stopping the development instance's daemon (`$env:YAGURA_INSTANCE='dev'; yagura-daemon --shutdown`
in PowerShell, `YAGURA_INSTANCE=dev yagura-daemon --shutdown` in a Unix shell) never affects the
work instance's windows, database, or logs, and vice versa: the two share no
endpoint, no config file, no session store, and no log file. See [`docs/mcp-setup.md`](mcp-setup.md)
for connecting an MCP client to a specific instance's `yagura-mcp` bridge.
