# Keymap Guide

yagura runs its operations from the keyboard. This guide lists them with their default keys,
explains how to change a key from Settings or by editing `keymap.toml`, and says which keys yagura
refuses to bind and why.

## Default Keys

Most operations have a default key; the ones marked "—" have none until you give them one. Windows
and Linux share a table; macOS has its own. On macOS `Cmd` is the `Meta` modifier in the file (see
[Writing a Key](#writing-a-key)).

| Action id                | Operation                               | Windows / Linux     | macOS         |
| ------------------------ | --------------------------------------- | ------------------- | ------------- |
| `open-window-picker`     | Go to a window (search by name)         | `Ctrl+Shift+o`      | `Meta+p`      |
| `open-command-palette`   | Commands (search the operations)        | `Ctrl+Shift+p`      | `Shift+Meta+p` |
| `focus-next-window`      | Next window (in sidebar order)          | `Ctrl+Shift+f`      | `Shift+Meta+]` |
| `focus-previous-window`  | Previous window                         | `Ctrl+Shift+b`      | `Shift+Meta+[` |
| `focus-next-attention`   | Next window needing you                 | `Ctrl+Shift+j`      | `Shift+Meta+a` |
| `open-window-form`       | New window                              | `Ctrl+Shift+n`      | `Meta+n`      |
| `close-window`           | Close the current window                | `Ctrl+Shift+w`      | `Shift+Meta+w` |
| `interrupt-window`       | Interrupt the current window            | `Ctrl+Shift+k`      | `Meta+.`      |
| `rename-window`          | Rename the current window               | —                   | —             |
| `minimize-window`        | Minimize the current window             | `Ctrl+Shift+m`      | `Shift+Meta+m` |
| `toggle-maximize-window` | Maximize or restore the current window  | `Ctrl+Shift+Enter`  | `Shift+Meta+Enter` |
| `toggle-work-mode`       | Switch between Focus and Canvas         | —                   | —             |
| `tile-windows`           | Tile the windows (Canvas only)          | —                   | —             |
| `toggle-sidebar-focus`   | Move to the sidebar / back to a window  | `Ctrl+Shift+e`      | `Shift+Meta+e` |
| `toggle-needs-you-filter` | Turn the Needs you filter on or off    | —                   | —             |
| `toggle-chat`            | Chat panel                              | `Ctrl+Shift+l`      | `Shift+Meta+l` |
| `toggle-plans`           | Board panel                             | `Ctrl+Shift+t`      | `Shift+Meta+t` |
| `toggle-settings`        | Settings panel                          | `Ctrl+Shift+s`      | `Meta+,`      |
| `toggle-notifications`   | Notifications list                      | —                   | —             |

yagura has two work views. In **Focus** (the default) the current window fills the work area and
the other windows keep running out of sight. In **Canvas** windows float and can be dragged,
resized, minimized and maximized. Switch from the view menu in the sidebar header; yagura
remembers your choice. "The current window" is the window on screen in Focus and the front window
in Canvas. `minimize-window` and `toggle-maximize-window` work only in Canvas, and do nothing in
Focus. With no window on screen, the operations on the current window do nothing.

Next and previous follow the order of the sidebar, top to bottom, and wrap around at both ends; a
window inside a collapsed room opens the room first. While the **Needs you** filter is on they
follow the rows the filter shows (windows that have settled and ones with an unknown status
included) in the order shown, and next window needing you visits only the windows that need you.
While the card for a window that ended is shown, there is no current window: next goes where the
card's **Go to** button goes, previous goes to the window before the one that ended, and neither
wraps around.

A shortcut does nothing while a modal dialog is open or while an input method is composing text.
Holding a shortcut down runs it once.

### Moving Between the Sidebar and a Window

`toggle-sidebar-focus` moves keyboard focus into the sidebar, onto the row of the current window
(or to the start of the sidebar when that window has no row), and moves it back to the current
window when pressed again from inside the sidebar.

The window list in the sidebar is a single `Tab` stop: `Tab` lands on the current window's row (or
the first row shown), the arrow keys, `Home` and `End` move between the rows and stop at the ends,
`Enter` or `Space` goes to the window, and the next `Tab` leaves the list. `Shift+F10` or the
context menu key opens the row's menu, whose **Move up** and **Move down** reorder a window within
its room. Reordering is not offered while the **Needs you** filter is on. The **Windows** button in
the rail sends the keyboard to the current window's terminal.

### Go to Window and Commands

One search box does two jobs. `open-window-picker` opens it to find a window; `open-command-palette`
opens it already in the commands mode. The **Go to window…** button under the sidebar header opens
it for windows too (it is left out while no window is open), and the empty work area shows the key
of Commands.

**Windows.** Type to narrow the list. Every word you type has to be found in the window's name, its
room (or its folder, for a window in no room), its working directory, its vendor or its status as
the sidebar words it; case and character width are ignored.

- With nothing typed, the windows that need you come first under **Needs you**, then the rest under
  **Other windows**; a window whose status is unknown leads the rest. The current window comes last
  in its list.
- With text typed, the list is one run. A window whose whole name is what you typed comes first; the
  rest are grouped by their weakest word: names that start with it, names with a word that starts
  with it, names that contain it, and last the windows where a word is found only in another field.
  Within a group, windows with fewer words found only outside the name come first, then windows
  that need you (or whose status is unknown), then the status and the sidebar order. The current
  window comes last.
- With nothing typed and no window open, the list offers **New window** instead.

**Commands.** A leading `>` turns the list into the operations, so the same box serves both modes:
type `>` in the windows mode to switch, delete it to switch back. The list holds every operation
except Commands itself, under the headings of the Keyboard tab, with the key of each at its right
end. Typing narrows it by name, by words the name does not show (`stop` finds Interrupt window) and
by heading. The matches are grouped as the windows are (whole name, start, word start, inside,
another field). Within a group, the operations with fewer words found only outside the name come
first, then those that can run before those that cannot.

- An operation that would do nothing right now stays in the list, dimmed, with the reason beside it:
  **No window on screen**, **Canvas only**, **No windows open**, **No window to go to**,
  **Nothing needs you**, or why a window cannot be interrupted. `Enter` does nothing on it. The key
  of such an operation does nothing either, and **Tile windows** in the sidebar's view menu shows
  the same reason.
- **Go to window…** in the list switches the box to the windows mode and keeps it open.

**Choosing.** `↑` and `↓` move, `PageUp` and `PageDown` move by a page, `Enter` or a click chooses,
and `Esc` closes. The box closes first, and the window or the operation you chose starts after that,
from where the focus was before you opened the box. An approval dialog never opens on top of the
box: it waits until the box has closed and the chosen row has started.

**While it is open.** The key that opens the other mode switches to that mode; the key of the mode
you are in does nothing. The other shortcuts do nothing, as they do in any dialog. The list follows
the windows as they change without moving the rows under you: a window that is gone loses its row,
and a new window is added at the end of its list.

### Escape Closes a Panel

Pressing a plain `Esc` closes the Chat, Board or Settings panel. This is fixed and not part of the
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

### Where Typing Goes After You Pick a Window or Close a Dialog

- When you pick a window yourself, what you type next goes to that window: its row in the sidebar
  (click, `Enter` or `Space`), a press on its frame (the title, the status or a resize edge in
  Canvas), or its maximize button. After you switch between Focus and Canvas from the view menu,
  focus goes to the terminal of the window the new view shows. Pressing a control in the header,
  such as `⋯` or a button, leaves focus to that control.
- When you minimize or close a window, or reopen windows from the **Reopen windows** dialog, focus
  moves to the current window (or the start of the sidebar when none is left) only if it has
  nowhere to type into: it was on the window that went away, on a window's own buttons, or on nothing. A button in
  the sidebar that is still there keeps focus.
- When a dialog about a window closes (interrupt, close or rename), whether you confirmed or
  cancelled it, focus goes to that window if it is the current window. Otherwise, and for any other
  dialog such as an approval, focus returns to where you were when it opened; if that is gone, to
  the panel you were in, and then to the current window.
- A window that comes to the front without you choosing it, such as one an AI opened, or a window
  whose program exits by itself, does not take focus: keys you are typing keep going where they
  were going. A window that ends on its own, or that an agent closes, does not move you to another
  window either: in Focus a card says which window ended and with what exit code, and offers
  **Go to** the next window or a new window; focus stays where it was, so what you type does not
  reach another CLI. Only a window you close yourself moves you to the next one.
- **Go to window** on the pop-up of a window that needs you is your own choice of that window: it
  comes on screen and what you type next goes to its terminal. A pop-up that arrives, is replaced
  or times out does not move focus.
- When you choose a row in the Go to window and Commands box, the box closes first. A window you
  chose gets what you type next; an operation you chose runs from where the focus was before the
  box opened.

### How Keys Reach the Program

Programs running in a window can ask the terminal for a richer key format than the traditional one,
which cannot tell `Ctrl+Tab` from `Tab` or `Ctrl+Shift+a` from `Ctrl+a`.

- **Windows:** keys are sent the way Windows Terminal sends them. If the program asked for the Kitty
  keyboard protocol, every key is sent in the Kitty format; otherwise every key is sent in Win32
  Input Mode, which ConPTY, the PTY host, requests when a window starts. Only a symbol key outside
  yagura's fixed key table is sent in the traditional format.
- **macOS and Linux:** when a program asks for the Kitty keyboard protocol, yagura supports two of
  its flags, disambiguate and report-all-keys, and sends every key in the Kitty format. Otherwise
  keys are sent in the traditional format.
- **Ctrl+V on Windows:** when the clipboard holds text, yagura pastes it. When it holds something
  else (an image, for example), yagura passes `Ctrl+V` to the program, which reads the clipboard
  itself.
- **WSL windows** (a window whose program is `wsl`): `wsl.exe` turns Win32 Input Mode back into
  the traditional format and drops more of a key press than yagura's traditional format does (for
  example, `Shift+Enter` arrives as `Enter`). Every key is sent in the Kitty format if the program
  asked for it, and in the traditional format otherwise. A window whose command is a shell in which
  you run `wsl` yourself is treated as a Windows window.

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
an operation without a line keeps its default key (none, for the operations marked "—"), unless a
line took that key for another operation, in which case it has no key. The Keyboard tab writes only the operations that differ
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

- its name is not one of the action ids in the table at the top of this page,
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
you remove from an operation (for example `Ctrl+Shift+T`) does not open a browser feature either.
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
