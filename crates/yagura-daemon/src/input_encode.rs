//! Turns structured input into the bytes a terminal writes to its PTY.
//!
//! The daemon's model is the window's only terminal, so the bytes a key, a
//! paste or a pointer event becomes depend on the modes the child set on it
//! (cursor keys mode, bracketed paste, mouse tracking and so on). Every
//! function here is pure: the caller passes the modes in.
//!
//! The sequences follow xterm.js 6.0.0
//! (`Keyboard.ts` `evaluateKeyboardEvent`, `Clipboard.ts`, the mouse
//! encoders of `CoreMouseService.ts`), plus Shift+Enter as `ESC CR`.

use yagura_protocol::{
    CellPosition, ControlKey, Key, KeyInput, Modifiers, NamedKey, PointerButton, PointerInput,
    PointerKind, WheelInput,
};

const ESC: u8 = 0x1b;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Which pointer events the child asked to have reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseTracking {
    /// None: the pointer selects.
    #[default]
    Off,
    /// Mode 9: presses only, without modifiers.
    X10,
    /// Mode 1000: presses and releases.
    Click,
    /// Mode 1002: presses, releases and moves with a button held.
    Drag,
    /// Mode 1003: presses, releases and every move.
    Motion,
}

/// The terminal modes that change what input becomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputModes {
    /// Cursor keys mode (DECCKM, `?1`).
    pub app_cursor: bool,
    /// Bracketed paste (`?2004`).
    pub bracketed_paste: bool,
    /// Mouse tracking (`?9`, `?1000`, `?1002`, `?1003`).
    pub mouse: MouseTracking,
    /// SGR mouse encoding (`?1006`).
    pub sgr_mouse: bool,
    /// Wheel as arrow keys on the alternate screen (`?1007`).
    pub alternate_scroll: bool,
    /// The alternate screen is shown.
    pub alt_screen: bool,
    /// Focus reports (`?1004`).
    pub focus_reports: bool,
}

impl InputModes {
    /// The modes packed into the low bits of a word, for publishing them
    /// atomically.
    pub(crate) fn to_bits(self) -> u16 {
        let mouse: u16 = match self.mouse {
            MouseTracking::Off => 0,
            MouseTracking::X10 => 1,
            MouseTracking::Click => 2,
            MouseTracking::Drag => 3,
            MouseTracking::Motion => 4,
        };
        u16::from(self.app_cursor)
            | u16::from(self.bracketed_paste) << 1
            | u16::from(self.sgr_mouse) << 2
            | u16::from(self.alternate_scroll) << 3
            | u16::from(self.alt_screen) << 4
            | u16::from(self.focus_reports) << 5
            | mouse << 6
    }

    /// The inverse of [`InputModes::to_bits`].
    pub(crate) fn from_bits(bits: u16) -> Self {
        let mouse = match (bits >> 6) & 0b111 {
            1 => MouseTracking::X10,
            2 => MouseTracking::Click,
            3 => MouseTracking::Drag,
            4 => MouseTracking::Motion,
            _ => MouseTracking::Off,
        };
        Self {
            app_cursor: bits & 1 != 0,
            bracketed_paste: bits & (1 << 1) != 0,
            sgr_mouse: bits & (1 << 2) != 0,
            alternate_scroll: bits & (1 << 3) != 0,
            alt_screen: bits & (1 << 4) != 0,
            focus_reports: bits & (1 << 5) != 0,
            mouse,
        }
    }
}

/// What a pointer event becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerOutcome {
    /// These bytes report it to the child.
    Report(Vec<u8>),
    /// It is not reported: it selects text in the window.
    Select,
    /// The child tracks the mouse but this event is not reported.
    Ignore,
}

/// What a wheel event becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WheelOutcome {
    /// These bytes go to the child (reports or arrow keys).
    Write(Vec<u8>),
    /// Nothing goes to the child: the view of the window moves by `lines`
    /// (negative is up).
    ScrollView {
        /// Lines to move the view by.
        lines: i32,
    },
}

/// The bytes a key press writes; empty when it writes nothing.
#[must_use]
pub fn encode_key(input: &KeyInput, modes: InputModes) -> Vec<u8> {
    match input.key {
        Key::Named(key) => encode_named(key, input.mods, modes),
        Key::Char(c) => encode_char(c, input.mods),
    }
}

/// Which way a key press pages the window's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewPage {
    /// One screen back into the history.
    Up,
    /// One screen toward the live screen.
    Down,
}

/// The page a key press moves the view by instead of reaching the child
/// (Shift+PageUp / Shift+PageDown, which write nothing); `None` for every
/// other key.
#[must_use]
pub fn view_page(input: &KeyInput) -> Option<ViewPage> {
    match input.key {
        Key::Named(key) => pages_view(key, input.mods),
        Key::Char(_) => None,
    }
}

fn pages_view(key: NamedKey, mods: Modifiers) -> Option<ViewPage> {
    match key {
        NamedKey::PageUp if mods.shift => Some(ViewPage::Up),
        NamedKey::PageDown if mods.shift => Some(ViewPage::Down),
        _ => None,
    }
}

/// The bytes committed text writes: its UTF-8.
#[must_use]
pub fn encode_text(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// The bytes a paste writes. Every line break becomes CR, as a terminal's
/// Enter does. With bracketed paste the text is wrapped in its markers, and
/// any marker inside the text is removed first, so the paste cannot end the
/// bracket early (no removal can leave a new marker behind).
#[must_use]
pub fn encode_paste(text: &str, modes: InputModes) -> Vec<u8> {
    if !modes.bracketed_paste {
        return line_breaks_to_cr(text.as_bytes());
    }
    let body = line_breaks_to_cr(&without_paste_markers(text.as_bytes()));
    let mut out = Vec::with_capacity(body.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START);
    out.extend_from_slice(&body);
    out.extend_from_slice(PASTE_END);
    out
}

/// What a pointer event becomes. Holding Shift always selects, as it does in
/// xterm.js.
#[must_use]
pub fn encode_pointer(input: &PointerInput, modes: InputModes) -> PointerOutcome {
    if modes.mouse == MouseTracking::Off || input.mods.shift {
        return PointerOutcome::Select;
    }
    let held = input.button != PointerButton::None;
    let reported = match (modes.mouse, input.kind) {
        (MouseTracking::Off, _) => false,
        (MouseTracking::X10, kind) => kind == PointerKind::Down,
        (MouseTracking::Click, kind) => kind != PointerKind::Move,
        (MouseTracking::Drag, kind) => kind != PointerKind::Move || held,
        (MouseTracking::Motion, _) => true,
    };
    if !reported || (input.kind != PointerKind::Move && !held) {
        return PointerOutcome::Ignore;
    }
    let mods = if modes.mouse == MouseTracking::X10 {
        Modifiers::default()
    } else {
        input.mods
    };
    let mut code = button_code(input.button) | modifier_bits(mods);
    if input.kind == PointerKind::Move {
        code |= 32;
    }
    match mouse_report(
        code,
        input.kind == PointerKind::Up,
        input.cell,
        modes.sgr_mouse,
    ) {
        Some(bytes) => PointerOutcome::Report(bytes),
        None => PointerOutcome::Ignore,
    }
}

/// What a wheel event of `input.delta` lines becomes: one report per line
/// while the child tracks clicks, drags or motion; otherwise one arrow key
/// per line on the alternate screen with alternate scroll; otherwise a move
/// of the view. The caller bounds `delta`, which sets the output's length.
#[must_use]
pub fn encode_wheel(input: &WheelInput, modes: InputModes) -> WheelOutcome {
    let lines = input.delta.unsigned_abs() as usize;
    let up = input.delta < 0;
    match modes.mouse {
        MouseTracking::Click | MouseTracking::Drag | MouseTracking::Motion => {
            let code = (if up { 64 } else { 65 }) | modifier_bits(input.mods);
            let one = mouse_report(code, false, input.cell, modes.sgr_mouse).unwrap_or_default();
            WheelOutcome::Write(one.repeat(lines))
        }
        MouseTracking::Off | MouseTracking::X10 if modes.alt_screen && modes.alternate_scroll => {
            let arrow = cursor_key(if up { b'A' } else { b'B' }, 1, modes.app_cursor);
            WheelOutcome::Write(arrow.repeat(lines))
        }
        MouseTracking::Off | MouseTracking::X10 => WheelOutcome::ScrollView { lines: input.delta },
    }
}

/// The bytes a focus change writes: a report while the child asked for
/// focus reports, nothing otherwise.
#[must_use]
pub fn encode_focus(focused: bool, modes: InputModes) -> Vec<u8> {
    match (modes.focus_reports, focused) {
        (false, _) => Vec::new(),
        (true, true) => b"\x1b[I".to_vec(),
        (true, false) => b"\x1b[O".to_vec(),
    }
}

/// The bytes a `send_keys` key writes, the same as the matching key press;
/// `None` for [`ControlKey::Unknown`].
#[must_use]
pub fn encode_control_key(key: ControlKey, modes: InputModes) -> Option<Vec<u8>> {
    let plain = Modifiers::default();
    let named = |key| Some(encode_named(key, plain, modes));
    match key {
        ControlKey::Escape => named(NamedKey::Escape),
        ControlKey::Backspace => named(NamedKey::Backspace),
        ControlKey::Enter => named(NamedKey::Enter),
        ControlKey::Up => named(NamedKey::ArrowUp),
        ControlKey::Down => named(NamedKey::ArrowDown),
        ControlKey::CtrlU => Some(encode_char(
            'u',
            Modifiers {
                ctrl: true,
                ..plain
            },
        )),
        ControlKey::Unknown => None,
    }
}

/// xterm's modifier parameter: 1 plus Shift 1, Alt 2, Ctrl 4.
fn modifier_param(mods: Modifiers) -> u8 {
    1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl)
}

fn encode_named(key: NamedKey, mods: Modifiers, modes: InputModes) -> Vec<u8> {
    let param = modifier_param(mods);
    match key {
        NamedKey::Enter if mods.alt || (mods.shift && !mods.ctrl) => vec![ESC, b'\r'],
        NamedKey::Enter => vec![b'\r'],
        NamedKey::Tab if mods.shift => b"\x1b[Z".to_vec(),
        NamedKey::Tab => vec![b'\t'],
        NamedKey::Backspace => {
            let byte = if mods.ctrl { 0x08 } else { 0x7f };
            alt_prefixed(mods.alt, &[byte])
        }
        NamedKey::Escape => alt_prefixed(mods.alt, &[ESC]),
        NamedKey::ArrowUp => cursor_key(b'A', param, modes.app_cursor),
        NamedKey::ArrowDown => cursor_key(b'B', param, modes.app_cursor),
        NamedKey::ArrowRight => cursor_key(b'C', param, modes.app_cursor),
        NamedKey::ArrowLeft => cursor_key(b'D', param, modes.app_cursor),
        NamedKey::Home => cursor_key(b'H', param, modes.app_cursor),
        NamedKey::End => cursor_key(b'F', param, modes.app_cursor),
        NamedKey::PageUp | NamedKey::PageDown if pages_view(key, mods).is_some() => Vec::new(),
        NamedKey::PageUp => tilde_key(5, if mods.ctrl { param } else { 1 }),
        NamedKey::PageDown => tilde_key(6, if mods.ctrl { param } else { 1 }),
        // Shift or Ctrl with Insert is a clipboard chord on some systems.
        NamedKey::Insert if mods.shift || mods.ctrl => Vec::new(),
        NamedKey::Insert => tilde_key(2, 1),
        NamedKey::Delete => tilde_key(3, param),
        NamedKey::F1 => ss3_key(b'P', param),
        NamedKey::F2 => ss3_key(b'Q', param),
        NamedKey::F3 => ss3_key(b'R', param),
        NamedKey::F4 => ss3_key(b'S', param),
        NamedKey::F5 => tilde_key(15, param),
        NamedKey::F6 => tilde_key(17, param),
        NamedKey::F7 => tilde_key(18, param),
        NamedKey::F8 => tilde_key(19, param),
        NamedKey::F9 => tilde_key(20, param),
        NamedKey::F10 => tilde_key(21, param),
        NamedKey::F11 => tilde_key(23, param),
        NamedKey::F12 => tilde_key(24, param),
    }
}

fn encode_char(c: char, mods: Modifiers) -> Vec<u8> {
    let Modifiers { shift, alt, ctrl } = mods;
    if alt {
        let control = if ctrl { ctrl_alt_char(c) } else { None };
        return match control {
            Some(byte) => vec![ESC, byte],
            None => alt_prefixed(true, utf8(c).as_bytes()),
        };
    }
    match (ctrl, shift) {
        (true, false) => ctrl_char(c).map(|byte| vec![byte]).unwrap_or_default(),
        (true, true) => ctrl_shift_char(c)
            .map(|byte| vec![byte])
            .unwrap_or_default(),
        (false, _) => utf8(c).as_bytes().to_vec(),
    }
}

fn utf8(c: char) -> String {
    c.to_string()
}

/// Ctrl with a character: the C0 control xterm.js writes for it.
fn ctrl_char(c: char) -> Option<u8> {
    match c {
        'a'..='z' | 'A'..='Z' => Some(c.to_ascii_uppercase() as u8 - 0x40),
        ' ' => Some(0x00),
        '3'..='7' => Some(c as u8 - b'3' + 0x1b),
        '8' => Some(0x7f),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '/' => Some(0x1f),
        _ => None,
    }
}

/// Ctrl+Shift with a character: only the shifted symbols that name a C0
/// control.
fn ctrl_shift_char(c: char) -> Option<u8> {
    match c {
        '@' => Some(0x00),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

/// Ctrl+Alt: letters and space become their control; other characters are
/// written as they are.
fn ctrl_alt_char(c: char) -> Option<u8> {
    match c {
        'a'..='z' | 'A'..='Z' | ' ' => ctrl_char(c),
        _ => None,
    }
}

fn alt_prefixed(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if alt {
        out.push(ESC);
    }
    out.extend_from_slice(bytes);
    out
}

/// A cursor key: `CSI 1;m X` with modifiers, else `SS3 X` in cursor keys
/// mode and `CSI X` outside it.
fn cursor_key(letter: u8, param: u8, app_cursor: bool) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[1;{param}{}", char::from(letter)).into_bytes()
    } else if app_cursor {
        vec![ESC, b'O', letter]
    } else {
        vec![ESC, b'[', letter]
    }
}

/// F1 to F4: `SS3 X`, or `CSI 1;m X` with modifiers, in either cursor mode.
fn ss3_key(letter: u8, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[1;{param}{}", char::from(letter)).into_bytes()
    } else {
        vec![ESC, b'O', letter]
    }
}

fn tilde_key(number: u8, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[{number};{param}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

fn line_breaks_to_cr(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied().peekable();
    while let Some(byte) = iter.next() {
        match byte {
            b'\r' => {
                iter.next_if_eq(&b'\n');
                out.push(b'\r');
            }
            b'\n' => out.push(b'\r'),
            other => out.push(other),
        }
    }
    out
}

/// `bytes` with every bracketed paste marker removed, including the ones a
/// removal would join together: each byte is appended and a marker that
/// ends at it is dropped at once, so the result never holds one.
fn without_paste_markers(bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        out.push(byte);
        if out.ends_with(PASTE_START) || out.ends_with(PASTE_END) {
            out.truncate(out.len() - PASTE_START.len());
        }
    }
    out
}

fn button_code(button: PointerButton) -> u16 {
    match button {
        PointerButton::Left => 0,
        PointerButton::Middle => 1,
        PointerButton::Right => 2,
        PointerButton::None => 3,
    }
}

fn modifier_bits(mods: Modifiers) -> u16 {
    u16::from(mods.shift) * 4 + u16::from(mods.alt) * 8 + u16::from(mods.ctrl) * 16
}

/// One mouse report in SGR or the default encoding; `None` when the default
/// encoding cannot carry it (a value past 255).
fn mouse_report(code: u16, release: bool, cell: CellPosition, sgr: bool) -> Option<Vec<u8>> {
    let col = u32::from(cell.col) + 1;
    let row = u32::from(cell.row) + 1;
    if sgr {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{col};{row}{end}").into_bytes());
    }
    let code = if release { code | 3 } else { code };
    let byte = |value: u32| u8::try_from(value + 32).ok();
    Some(vec![
        ESC,
        b'[',
        b'M',
        byte(u32::from(code))?,
        byte(col)?,
        byte(row)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_survive_packing_into_bits() {
        let mice = [
            MouseTracking::Off,
            MouseTracking::X10,
            MouseTracking::Click,
            MouseTracking::Drag,
            MouseTracking::Motion,
        ];
        for mouse in mice {
            for flags in 0u8..64 {
                let modes = InputModes {
                    app_cursor: flags & 1 != 0,
                    bracketed_paste: flags & 2 != 0,
                    sgr_mouse: flags & 4 != 0,
                    alternate_scroll: flags & 8 != 0,
                    alt_screen: flags & 16 != 0,
                    focus_reports: flags & 32 != 0,
                    mouse,
                };
                assert_eq!(InputModes::from_bits(modes.to_bits()), modes);
            }
        }
    }
}
