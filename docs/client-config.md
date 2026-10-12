# Client Configuration Guide

This guide explains `client.toml`, the file that holds settings for the GUI's own
daemon-recovery supervisor: the part of yagura that watches for the daemon connection
going away while a window is open, and restarts the daemon on a backoff schedule if so.
These settings are separate from the daemon's own configuration.

## File Location

Windows:

```
%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\client.toml
```

macOS:

```
~/Library/Application Support/yagura/client.toml
```

Linux:

```
~/.config/yagura/client.toml
```

(`$XDG_CONFIG_HOME/yagura/client.toml` when `XDG_CONFIG_HOME` is set.)

This is the same directory yagura uses for the rest of its user-owned configuration
(`keymap.toml`, `themes.toml`, `playbook.toml`). The path is a single fixed join: yagura
never searches parent directories for a client config file.

The file is optional and yagura never creates (seeds) it for you. Most installs never
create it and run on the built-in defaults below.

## File Format

The file is TOML with a single `[reconnect]` table:

```toml
[reconnect]
auto_respawn = true
initial_delay_ms = 1000
backoff_factor = 2
attempt_limit = 3
```

Anything outside `[reconnect]` is ignored. Any key inside `[reconnect]` that this guide
does not list below is ignored too, so a newer file stays readable by an older build.
Every key is optional; a file that sets only one of them keeps the built-in defaults for
the rest.

## Keys

| Key                 | Type    | Default | Accepted range |
| -------------------- | ------- | ------- | --------------- |
| `auto_respawn`       | boolean | `true`  | —               |
| `initial_delay_ms`   | integer | `1000`  | 100–60000       |
| `backoff_factor`     | integer | `2`     | 1–10            |
| `attempt_limit`      | integer | `3`     | 1–10            |

- **`auto_respawn`** — whether the supervisor may start the daemon on its own when the
  connection to it is lost. This governs only the supervisor's own backoff loop: it does
  not stop an explicit reconnect action, or a window operation (for example listing
  windows) that starts the daemon on demand. Setting it to `false` does not prevent
  yagura from starting the daemon through those other paths.
- **`initial_delay_ms`** — how long, in milliseconds, the supervisor waits before its
  first restart attempt.
- **`backoff_factor`** — what the wait is multiplied by after each further attempt.
- **`attempt_limit`** — how many attempts are made before the supervisor gives up.

## Resolution Order and Out-of-Range Values

Each key is resolved independently, in this order: the environment variable (see
below, and only for `auto_respawn`), then the value in `client.toml`, then the built-in
default.

A value that lies outside its accepted range is **dropped, not clamped**: that one key
falls back to its default while every other valid key from the file still applies. This
is not reported as a failure — only a file that cannot be understood as a whole is (see
below).

A key of the wrong TOML type (for example, `auto_respawn = "yes"` instead of a boolean,
or `attempt_limit = "three"` instead of an integer) is a structural parse failure: the
entire file is rejected as `invalid_toml` and every key falls back to its default,
exactly as if the file did not exist.

## Environment Variable

`YAGURA_GUI_AUTO_RESPAWN` overrides `auto_respawn` only. The other three keys can only
be set through the file.

| Value                                            | Effect                                                  |
| -------------------------------------------------- | -------------------------------------------------------- |
| Unset, or empty/whitespace-only after trimming     | No override; `auto_respawn` follows the file (or the default) |
| `0` or `false`, case-insensitively, after trimming leading/trailing whitespace | Auto-respawn is disabled |
| Any other non-empty (post-trim) value              | Auto-respawn is enabled                                 |

The value is trimmed of leading/trailing whitespace and compared case-insensitively, so
`False`, `FALSE`, and `false ` all disable auto-respawn the same way `false` does.

## When the Whole File Is Ignored

Some conditions reject the file as a whole. In every case yagura falls back to the
built-in defaults and keeps running — a bad `client.toml` never blocks startup.

| Condition                                                                    | Reported as    |
| ----------------------------------------------------------------------------- | -------------- |
| No `client.toml` at that path (or the configuration root cannot be resolved) | `not_found`    |
| The path exists but cannot be read as text (a directory, invalid UTF-8)      | `read_failed`  |
| The text is not valid TOML, or a key inside `[reconnect]` has the wrong type | `invalid_toml` |
| The file exceeds the size limit below                                        | `too_large`    |

A missing file is the normal way to run on the defaults, so it is the one outcome that
is not surfaced as something gone wrong.

### Size Limit

| Limit     | Value                |
| --------- | --------------------- |
| File size | 65536 bytes (64 KiB) |

A hand-written `client.toml` has no legitimate reason to approach this number.

## Applying Changes

`client.toml` and the `YAGURA_GUI_AUTO_RESPAWN` environment variable are both read
**once, at application startup**. Restart yagura after editing the file or changing the
environment variable; there is no reload command and no file watcher.
