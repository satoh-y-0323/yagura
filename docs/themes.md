# Theme Guide

This guide explains how to create custom color themes with a `themes.toml` file.

## File Location

Windows:

```
%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\themes.toml
```

macOS:

```
~/Library/Application Support/yagura/themes.toml
```

Linux:

```
~/.config/yagura/themes.toml
```

(`$XDG_CONFIG_HOME/yagura/themes.toml` when `XDG_CONFIG_HOME` is set.)

This is the same directory yagura uses for the rest of its user-owned configuration
(`keymap.toml`, `playbook.toml`). The path is a single fixed join: yagura never searches
parent directories for a themes file.

The file is optional. Most installs never create it and run on the built-in palettes only (`dark` and `light`).

## File Format

The file is TOML with two sections: a `[themes.*]` table for custom palette definitions and an `[assignments]`
section for vendor and default theme selection:

```toml
[themes.my-dark-theme]
color-background = "#1e1e1e"
color-foreground = "#ffffff"
# ... all 35 palette keys required (see below)

[assignments]
default = "my-dark-theme"

[assignments.vendor]
claude = "my-dark-theme"
codex = "dark"
```

Quoting the theme name (e.g., `"my-dark-theme"`) is required when it contains special characters.

Anything outside the `[themes.*]` and `[assignments]` sections is ignored. A file with no `[themes.*]` entries
is not an error — yagura will continue using the built-in palettes, and any vendor assignments will be applied
to them.

## Palette Keys (35 total)

Each theme table must include **all 35 keys**. No more, no fewer. Each value must be a color in the format `#RRGGBB`
(hex RGB with exactly 6 digits). Omitting a required key or including unknown keys will cause the entire theme to be rejected.

### Color Tokens (16)

```toml
color-background = "#1e1e1e"
color-foreground = "#ffffff"
color-surface = "#2d2d2d"
color-border = "#404040"
color-primary = "#4a9eff"
color-primary-hover = "#2e7fd4"
color-success = "#28a745"
color-warning = "#ffc107"
color-error = "#dc3545"
color-error-hover = "#d32f2f"
color-warning-action = "#ff9800"
color-warning-action-hover = "#f57c00"
color-on-warning = "#333333"
color-on-warning-action = "#ffffff"
color-text-primary = "#ffffff"
color-text-secondary = "#a0a0a0"
```

### Terminal Selection (3)

```toml
term-selection-background = "#264f78"
term-selection-foreground = "#ffffff"
term-selection-inactive-background = "#3d3d3d"
```

### ANSI Colors (16)

```toml
term-ansi-black = "#000000"
term-ansi-red = "#cd3131"
term-ansi-green = "#0dbc79"
term-ansi-yellow = "#e5e510"
term-ansi-blue = "#007acc"
term-ansi-magenta = "#bc3fbc"
term-ansi-cyan = "#11a8cd"
term-ansi-white = "#e5e5e5"
term-ansi-bright-black = "#666666"
term-ansi-bright-red = "#f14c4c"
term-ansi-bright-green = "#23d18b"
term-ansi-bright-yellow = "#f5f543"
term-ansi-bright-blue = "#3b8eea"
term-ansi-bright-magenta = "#d670d6"
term-ansi-bright-cyan = "#29b8db"
term-ansi-bright-white = "#ffffff"
```

## Reserved Names

Three names are reserved for built-in modes and cannot be used as custom theme names:

- `dark` — built-in dark theme
- `light` — built-in light theme
- `system` — follow OS color scheme preference (available only in the app selector, not as a palette)

An attempt to define a theme named `dark`, `light`, or `system` will cause that theme to be rejected.

## Assignments

The optional `[assignments]` section controls which theme is used for different contexts.

### Default Theme

The `default` key sets the initial theme when yagura starts:

```toml
[assignments]
default = "my-dark-theme"
```

The value must be one of:

- A reserved mode name: `"dark"`, `"light"`, or `"system"`
- The name of a custom theme defined in `[themes.*]`
- An unknown name results in a warning and defaults to `dark`

When omitted, `dark` is used. When set to `"system"`, yagura will follow the OS color scheme preference at
startup (but not update during execution; restart after changing OS theme to see the effect).

### Vendor Assignments

The `[assignments.vendor]` section maps agent vendors to specific themes. When you open a terminal window for a
particular vendor (Claude, Codex, Grok, etc.), that vendor's theme will be applied to just that window, overriding
the app-wide selection:

```toml
[assignments.vendor]
claude = "my-dark-theme"
codex = "dark"
```

The value must be one of:

- The name of a custom theme defined in `[themes.*]`
- A built-in theme name: `"dark"` or `"light"`
- `"system"` is **not** valid as a vendor assignment (rejected with a warning)
- An unknown name results in a warning and that vendor will use the app selection

When omitted or empty, vendor windows use the app-wide selection.

A vendor key is not a detection definition name. `codex` above picks the theme
for Codex windows, while `codex-cli` is the name of the file in
[detection/](../crates/yagura-daemon/detection/README.md) whose patterns that window's output is
matched against — a Codex window normally sets both, which in `open_window`
reads `vendor: "codex"` and `detection: "codex-cli"`.

## Validation and Limits

### File Limits

yagura enforces three hard limits to prevent resource exhaustion:

1. **File size**: `themes.toml` must not exceed 64 KB. Files larger than this are rejected.
2. **Theme count**: The file must not contain more than 50 theme definitions. Exceeding this is rejected.
3. **Keys per theme**: Each theme table must not have more than 64 keys. Exceeding this is rejected.

These are safety limits only; a well-formed palette file will not approach them.

### Failure Modes

If `themes.toml` cannot be loaded, yagura continues running with the built-in palettes (`dark` and `light`).
The app displays a banner indicating the failure reason:

- **File not found** — `themes.toml` does not exist; this is normal. Yagura uses built-in palettes.
- **Read failed** — The file exists but could not be read (e.g. permission denied, or is a directory instead
  of a file). Yagura uses built-in palettes.
- **Invalid TOML** — The file is not valid TOML syntax. Check your brackets, quotes, and structure.
  Yagura uses built-in palettes.
- **File too large** — The file size, theme count, or keys-per-theme exceeds the limits listed above.
  Yagura uses built-in palettes.
- **Invalid palette** — A theme table is missing keys, contains non-string values, or has a key with a
  non-`#RRGGBB` color format. That theme is skipped, but other themes and vendor assignments are still loaded.
  A banner lists the rejected theme names.
- **Reserved name** — A theme is named `dark`, `light`, or `system`. That theme is skipped.
  A banner lists the rejected theme names.
- **Unknown reference** — A vendor or default assignment refers to a theme that does not exist.
  That assignment is skipped. A banner lists the references that could not be resolved.

### Contrast and Color Validity

yagura does **not** validate color contrast ratios or test for accessibility. The 35 palette keys represent
a semantic layout (e.g. "button primary state" vs "button hover state"), but yagura enforces only the hex
format (`#RRGGBB`) and that all 35 keys are present. It is your responsibility to choose values that are
readable on your display and for your use case.

## Loading and Updating

### Startup Behavior

The `themes.toml` file is read **once at startup**. Changes to the file while yagura is running are not
reflected in the app. You must close and restart yagura to load an updated file.

### Selecting a Theme

Use the app's **Theme** selector (in the navigation rail) to select between available themes:

- Built-in themes (`dark`, `light`)
- Any custom themes successfully loaded from `themes.toml`
- `system` mode (if the OS supports preference queries)

Changing the selection takes effect immediately in the app and all open terminal windows
(except where an individual window has a vendor or window-specific override).

### Window-Specific Overrides

Each terminal window can have its own theme override, independent of the app-wide selection. Use the
**Theme for this window** selector in the window menu (opened from the window's title bar or its tab)
to override that window. Setting it to "Same as app" removes the override and makes that window follow the app
selection again.

Window overrides persist within the same yagura session (they survive window dragging and reordering).
When yagura restarts, only the app-wide selection is restored; window-specific overrides are cleared.

### Storage Key

The app-wide selection is stored in the browser's `localStorage` under the key:

```
yagura.theme-selection
```

The stored value is one of: `"dark"`, `"light"`, `"system"`, or `"named:<theme-name>"` (e.g., `"named:my-dark-theme"`).

### Recovery

If the selection is corrupted or lost, yagura falls back to the `default` assignment from `themes.toml`, or `dark` if
no `default` is specified. Clearing `localStorage` (via browser developer tools in yagura's WebView) will reset the
selection to the default on the next startup.

## Examples

### Minimal File (No Custom Themes)

```toml
[assignments]
default = "light"

[assignments.vendor]
codex = "dark"
```

This file uses only the built-in themes, but sets `light` as the app default and routes Codex windows to `dark`.

### Custom Dark Theme

```toml
[themes.my-theme]
color-background = "#0d0d0d"
color-foreground = "#e8e8e8"
color-surface = "#1a1a1a"
color-border = "#333333"
color-primary = "#6ba3ff"
color-primary-hover = "#4a7fb3"
color-success = "#22c55e"
color-warning = "#f59e0b"
color-error = "#ef4444"
color-error-hover = "#dc2626"
color-warning-action = "#fb923c"
color-warning-action-hover = "#f97316"
color-on-warning = "#292524"
color-on-warning-action = "#fafaf9"
color-text-primary = "#f5f5f5"
color-text-secondary = "#9ca3af"
term-selection-background = "#1e3a5f"
term-selection-foreground = "#e8e8e8"
term-selection-inactive-background = "#2d2d2d"
term-ansi-black = "#000000"
term-ansi-red = "#d84444"
term-ansi-green = "#35d788"
term-ansi-yellow = "#e6d600"
term-ansi-blue = "#1e90ff"
term-ansi-magenta = "#d946ef"
term-ansi-cyan = "#06b6d4"
term-ansi-white = "#e5e5e5"
term-ansi-bright-black = "#808080"
term-ansi-bright-red = "#ff6b6b"
term-ansi-bright-green = "#4ade80"
term-ansi-bright-yellow = "#fbbf24"
term-ansi-bright-blue = "#60a5fa"
term-ansi-bright-magenta = "#f472b6"
term-ansi-bright-cyan = "#22d3ee"
term-ansi-bright-white = "#ffffff"

[assignments]
default = "my-theme"
```

This defines a complete custom dark theme and makes it the default.

### Multiple Themes with Vendor Routing

```toml
[themes.office-light]
# ... all 35 keys, light palette
color-background = "#ffffff"
# ...

[themes.coding-dark]
# ... all 35 keys, dark palette
color-background = "#1e1e1e"
# ...

[assignments]
default = "office-light"

[assignments.vendor]
claude = "coding-dark"
codex = "office-light"
```

This file defines two themes and uses different themes for different vendors.
