# Keymap Guide

yagura runs twelve operations from the keyboard. This guide lists them with their default keys,
explains how to change a key from Settings or by editing `keymap.toml`, and says which keys yagura
refuses to bind and why.

## Default Keys

Every operation has one key. Windows and Linux share a table; macOS has its own. On macOS `Cmd`
is the `Meta` modifier in the file (see [Writing a Key](#writing-a-key)).

| Action id                | Operation                               | Windows / Linux     | macOS         |
| ------------------------ | --------------------------------------- | ------------------- | ------------- |
| `focus-next-window`      | Next window (in sidebar order)          | `Ctrl+Tab`          | `Ctrl+Tab`    |
| `focus-previous-window`  | Previous window                         | `Ctrl+Shift+Tab`    | `Ctrl+Shift+Tab` |
| `focus-next-attention`   | Next window needing you                 | `Ctrl+Shift+a`      | `Shift+Meta+a` |
| `open-window-form`       | New window                              | `Ctrl+Shift+n`      | `Meta+n`      |
| `close-window`           | Close the current window                | `Ctrl+Shift+w`      | `Shift+Meta+w` |
| `interrupt-window`       | Interrupt the current window            | `Ctrl+Shift+k`      | `Meta+.`      |
| `minimize-window`        | Minimize the current window             | `Ctrl+Shift+m`      | `Shift+Meta+m` |
| `toggle-maximize-window` | Maximize or restore the current window  | `Ctrl+Shift+Enter`  | `Shift+Meta+Enter` |
| `toggle-sidebar-focus`   | Move to the sidebar / back to a window  | `Ctrl+Shift+e`      | `Shift+Meta+e` |
| `toggle-chat`            | Chat panel                              | `Ctrl+Shift+l`      | `Shift+Meta+l` |
| `toggle-plans`           | Plans panel                             | `Ctrl+Shift+p`      | `Shift+Meta+p` |
| `toggle-settings`        | Settings panel                          | `Ctrl+,`            | `Meta+,`      |

"The current window" is the window shown on screen. Next and previous follow the order of the
sidebar, top to bottom, and wrap around at both ends; a window inside a collapsed room opens the
room first. With no window on screen, the operations on the current window do nothing.

A shortcut does nothing while a modal dialog is open or while an input method is composing text.
Holding a shortcut down runs it once.

### Moving Between the Sidebar and a Window

`toggle-sidebar-focus` moves keyboard focus into the sidebar, onto the row of the current window
(or to the start of the sidebar when that window has no row), and moves it back to the current
window when pressed again from inside the sidebar.

### Escape Closes a Panel

Pressing a plain `Esc` closes the Chat, Plans or Settings panel. This is fixed and not part of the
keymap. It does not apply while the panel has already used the key (for example, an open mention
list in Chat), and an `Esc` typed inside a window still goes to the program running there.

- In Chat, the first `Esc` closes an open mention list and the next one closes the panel. The list
  stays closed until you edit the text or move the caret out of that mention.
- If focus has left every panel without landing anywhere (for example, after **Reset** or
  **Remove** in the Keyboard tab turns the button you pressed off), `Esc` closes the panel you were
  last in.
- When a panel closes while it held focus, focus goes back to where you opened it from: another
  panel that is still open, or the window or sidebar row. A panel opened from the rail returns
  focus to the open panel you used most recently, so closing the upper of two stacked panels lands
  in the lower one; with no other panel open, focus goes to the current window.

## Changing a Key from Settings

Open Settings and choose the **Keyboard** tab. Each operation has a row with **Change**,
**Reset** (back to the default) and **Remove** (no key).

- **Change** listens for the next key combination. Press it to assign it; `Esc` cancels.
- A combination that another operation already uses is moved to the operation you are editing, and
  the other operation is left without a key. The tab tells you when this happens.
- A combination yagura refuses (see [Keys yagura Refuses](#keys-yagura-refuses)) is not saved, and
  the tab says why.
- A change is saved to `keymap.toml` first and takes effect only once the save has succeeded. If
  saving fails, the tab says so and the key stays as it was.
- Changes apply at once; no restart is needed.

If `keymap.toml` could not be read when yagura started (it cannot be read as text, is not valid
TOML, or is too large), the tab turns editing off rather than overwrite the file, and names
the file's path. Fix or delete the file and restart yagura.

## File Location

Windows:

```
%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\keymap.toml
```

macOS:

```
~/Library/Application Support/yagura/keymap.toml
```

Linux:

```
~/.config/yagura/keymap.toml
```

(`$XDG_CONFIG_HOME/yagura/keymap.toml` when `XDG_CONFIG_HOME` is set.)

This is the same directory yagura uses for the rest of its user-owned configuration
(`playbook.toml`). The path is a single fixed join: yagura never searches parent directories for a
keymap.

The file is optional. Most installs never create it and run on the built-in defaults. The Keyboard
tab creates it the first time you change a key.

## File Format

The file is TOML with a single `[keys]` table. Each key is an *action id* (see the table above)
and each value is the key combination for that operation. An empty string means the operation has
no key:

```toml
[keys]
focus-next-window = "Ctrl+Shift+PageDown"
open-window-form = "Ctrl+Shift+n"
toggle-chat = ""
```

Only the operations you want to change need a line. The file is merged **on top of** the defaults:
an operation without a line keeps its default key, unless a line took that key for another
operation, in which case it has no key. The Keyboard tab writes only the operations that differ
from the defaults.

Anything outside `[keys]` is ignored. A file whose `[keys]` table is missing or empty simply
changes nothing.

### Writing a Key

A key is written as modifiers followed by a key name, joined with `+`:

```
Modifier+[Modifier...]+Key
```

- **Modifiers** are `Ctrl`, `Alt`, `Shift` and `Meta`, and must appear **in that order**
  (`Ctrl+Shift+n` is valid; `Shift+Ctrl+n` is not). A modifier may not be repeated. `Meta` is
  `Cmd` on macOS and the Windows key on Windows.
- **Letters** are lowercase: `n`, `w`. A single uppercase letter such as `Ctrl+Shift+N` is not
  accepted; add `Shift` as a modifier instead.
- **Other keys** use their DOM names, which start with an uppercase letter: `ArrowRight`, `Enter`,
  `Tab`, `Escape`, `Home`, `PageDown`, `F5`.
- **A symbol** is the character that key types: `,`, `.`, `;`. Because `+` separates the parts of
  the combination, the `+` key is written `Plus`, and the space bar is written `Space`. A symbol
  typed with Shift is written as the character that comes out plus the `Shift` modifier, for
  example `Ctrl+Shift+?`.
- The key is matched exactly: `Ctrl+Shift+n` fires only when Ctrl and Shift are held and Alt and
  Meta are not.

## Keys yagura Refuses

Keys are shared with the programs running in your windows, so yagura refuses combinations that
would take a key away from them or that the system already owns. The same rules apply to the
Keyboard tab, the default table and the file.

| Rule                                      | Refused because                                                                 |
| ----------------------------------------- | ------------------------------------------------------------------------------- |
| Not a key combination                     | The text cannot be read as modifiers plus a key (wrong order, unknown modifier, uppercase letter, empty key). |
| No modifier, or `Shift` alone             | A key without `Ctrl` or `Cmd` would be taken from the programs in your windows.  |
| `Alt` (Option) without `Ctrl` or `Cmd`    | Programs in your windows use `Alt` combinations for their own commands.          |
| `Ctrl+Alt` with a character key           | Some keyboard layouts type characters with `Ctrl+Alt` (it is how `AltGr` is reported). |
| `Ctrl+Shift+c` and `Ctrl+Shift+v` (Windows and Linux) | Kept for copy and paste.                                    |
| The app menu's keys (macOS)               | `Cmd+Q`, `Cmd+W`, `Cmd+H`, `Cmd+M`, `Cmd+Z`, `Cmd+X`, `Cmd+C`, `Cmd+V`, `Cmd+A`, `Shift+Cmd+Z`, `Option+Cmd+H` and `Ctrl+Cmd+F` belong to the native menu. |
| A key that reaches the shell or CLI       | With `Ctrl` the window would receive something the same key does not send without `Ctrl` (for example `Ctrl+r` searches shell history, `Ctrl+a` moves the cursor to the line start, and `Ctrl+Shift+ArrowRight` moves it by a word). Try adding `Shift`, or use another key. |
| A key that reloads the GUI                | `Ctrl+Shift+r` on Windows and Linux, and `Cmd+R` and `Shift+Cmd+R` on macOS, are kept switched off (see [Keys That Reload the GUI](#keys-that-reload-the-gui)), so a shortcut on them would never run. The other reload keys are already refused by the rules above. |

### Entries That Are Discarded

When yagura reads the file, an entry is discarded, and the rest of the file stays in effect, when:

- its name is not one of the twelve action ids,
- its key is refused by a rule above,
- an earlier operation (in the order of the table at the top of this page) already took the same
  key.

A discarded entry leaves that operation on its default key. yagura shows a notice at startup with
the number of discarded entries.

Saving from the Keyboard tab writes only the entries yagura kept, so the discarded ones (unknown
operation names and refused keys) are removed from the file.

## When the Whole File Is Ignored

Some conditions reject the file as a whole. In every case yagura falls back to the built-in
defaults and keeps running.

| Condition                                                                    | Reported as    | Notice shown |
| ---------------------------------------------------------------------------- | -------------- | ------------ |
| No `keymap.toml` at that path (or the configuration root cannot be resolved) | `not_found`    | No           |
| The path exists but cannot be read as text (a directory, invalid UTF-8)      | `read_failed`  | Yes          |
| The text is not valid TOML, or `[keys]` is not a table of strings            | `invalid_toml` | Yes          |
| The file exceeds the size limits below                                       | `too_large`    | Yes          |

A missing file is the normal way to run on the defaults, so it is the one outcome that stays
silent.

### Size Limits

| Limit                                          | Value                | Checked        |
| ---------------------------------------------- | -------------------- | -------------- |
| File size                                      | 65536 bytes (64 KiB) | While reading  |
| Entries in `[keys]`                            | 200                  | After parsing  |

Exceeding either limit discards the entire file. A saved keymap that would exceed a limit is not
written.

## Applying Changes

- The Keyboard tab applies a change immediately, once it has been saved.
- Editing `keymap.toml` by hand takes effect the next time yagura starts; there is no reload
  command and no file watcher.

## Keys That Reload the GUI

The keys a web view uses to reload its page do nothing in yagura: `F5`, `Shift+F5`, `Ctrl+F5`,
`Ctrl+R` and `Ctrl+Shift+R` (on macOS, `Cmd+R` and `Shift+Cmd+R`), so a stray press cannot reload
the window and interrupt your session. `Ctrl+R` typed inside a window still reaches the program
running there (a shell's history search, for example); on macOS `Ctrl+R` is not treated as a
reload key at all.

None of these keys can be given to an operation.

## Browser Keys on Windows

On Windows, the keys that open the web view's own browser features do nothing in yagura: print
(`Ctrl+P`), find (`Ctrl+F`), zoom, history navigation and the like. This covers every
key the web view handles as a browser, including combinations yagura has no default for, so a key
you remove from an operation (for example `Ctrl+Shift+P`) does not open a browser feature either.
Keys that edit or move through text (`Ctrl+A`, `Ctrl+C`, `Ctrl+V`, `Ctrl+Z`, the arrow keys) keep
working, and so do yagura's own shortcuts.

## Troubleshooting

- **A key I typed in a window does not reach the program.** Another operation owns that
  combination. Open Settings > Keyboard, find the operation and change or remove its key.
- **The Keyboard tab says changes are turned off.** `keymap.toml` could not be read. Fix the file
  (valid TOML, `[keys]` values are quoted strings, within the size limits) or delete it, then
  restart yagura.
- **A startup notice says entries were discarded.** Re-check the spelling (modifier order
  `Ctrl` → `Alt` → `Shift` → `Meta`, lowercase letters, DOM names for special keys), the action
  ids, and the rules in [Keys yagura Refuses](#keys-yagura-refuses).
- **A startup notice says the keymap could not be loaded.** The whole file was rejected; see
  [When the Whole File Is Ignored](#when-the-whole-file-is-ignored).
