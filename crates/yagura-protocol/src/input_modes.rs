//! The terminal modes a window's child set that change what input becomes:
//! the terminal model reads them off the child's output, and the daemon's
//! input encoding takes them in.

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
    /// Win32 Input Mode (`?9001`), which the PTY host asks for to read keys
    /// as console key records.
    pub win32_input: bool,
    /// The Kitty keyboard protocol flags in effect: the child's request
    /// narrowed to the ones supported ([`KITTY_SUPPORTED_FLAGS`]).
    pub kitty_flags: u8,
    /// The window's program turns console key records back into legacy
    /// bytes before its child reads them (`wsl.exe`), so a press sent as
    /// Win32 records arrives with its modifiers dropped. Fixed when the
    /// window opens, so it is not part of [`InputModes::to_bits`].
    pub key_records_flattened: bool,
}

/// Kitty keyboard flag 1: disambiguate escape codes.
pub const KITTY_DISAMBIGUATE: u8 = 0b0001;
/// Kitty keyboard flag 8: report all keys as escape codes.
pub const KITTY_REPORT_ALL_KEYS: u8 = 0b1000;
/// The Kitty keyboard flags the daemon honours. Event types (2) are left out
/// because clients send presses only; alternate keys (4) and associated text
/// (16) because a key press carries neither.
pub const KITTY_SUPPORTED_FLAGS: u8 = KITTY_DISAMBIGUATE | KITTY_REPORT_ALL_KEYS;

impl InputModes {
    /// The modes packed into the low bits of a word, for publishing them
    /// atomically.
    #[must_use]
    pub fn to_bits(self) -> u16 {
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
            | u16::from(self.win32_input) << 9
            | u16::from(self.kitty_flags & KITTY_DISAMBIGUATE != 0) << 10
            | u16::from(self.kitty_flags & KITTY_REPORT_ALL_KEYS != 0) << 11
    }

    /// The inverse of [`InputModes::to_bits`].
    #[must_use]
    pub fn from_bits(bits: u16) -> Self {
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
            win32_input: bits & (1 << 9) != 0,
            kitty_flags: bit_flag(bits, 10, KITTY_DISAMBIGUATE)
                | bit_flag(bits, 11, KITTY_REPORT_ALL_KEYS),
            key_records_flattened: false,
        }
    }
}

fn bit_flag(bits: u16, bit: u32, flag: u8) -> u8 {
    if bits & (1 << bit) != 0 { flag } else { 0 }
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
                    win32_input: flags & 1 != 0,
                    kitty_flags: [0, KITTY_DISAMBIGUATE, KITTY_REPORT_ALL_KEYS, 9]
                        [usize::from((flags >> 4) & 3)],
                    key_records_flattened: false,
                };
                assert_eq!(InputModes::from_bits(modes.to_bits()), modes);
            }
        }
    }
}
