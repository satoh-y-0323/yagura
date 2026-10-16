//! Test fixture reproducing, without depending on any real vendor CLI, two
//! startup/timing behaviors `send_handoff`'s delivery must survive: input
//! dropped right after start
//! (`discard_pending_input_before_interactive_screen` in `openai/codex`),
//! modeled as the first `discard_reads` reads dropped whole, so which
//! write is lost never depends on timing,
//! and burst suppression around a bracketed paste's edges (`paste_burst.rs`
//! in the same source), modeled as the first `paste_guard_keys` submit keys
//! after each bracketed paste being taken as line breaks. A third mode (`react_on_body`) simulates a receiver
//! whose observable state reacts to the body alone, before any submit key
//! arrives.
//!
//! Positional argv, no parser (same convention as `ready_child`):
//! `submit_child <marker> <discard_reads> <paste_guard_keys> <react_on_body> <startup>
//! [<composer_marker> <separator> <continuation_prefix> <reflect_delay_ms>
//! [<fold_placeholder> [<clear_scope> [<overlay> <overlay_heading>]]]]`.
//! `react_on_body` is compared to the literal string `"1"`; any other value
//! (including a missing argument) means non-react, with no validation error.
//! Given `startup` as `until|<path>`, the fixture paints its marker only once
//! the file `<path>` exists, so the window becomes ready when the test
//! decides; any other value starts it at once. Given the four trailing arguments the fixture also
//! paints its unsubmitted input as one marked composer region; without them
//! it writes the same text it wrote before that mode existed, which is what
//! every caller of the five-argument launcher relies on. One further argument
//! folds that region: its placeholder is painted wherever a non-empty body
//! would stand, so the body never reaches the screen. A further argument
//! (`clear_scope`) changes what the `clear` byte (`0x15`) does. Omitted or
//! empty keeps the byte's ordinary behavior, clearing whatever is buffered.
//! `"hold-until-clear"` holds every typed write unpainted, however long it
//! waits, until a `clear` byte arrives; that byte then releases the held
//! write instead of erasing it (it never painted), prints `RELEASED`, and
//! paints it `reflect_delay_ms` later. A stall past any upper bound is thus
//! caused by the order of events, not by how fast the machine runs.
//! `"literal-clear"` types the `clear` byte as the two literal characters
//! `^U` instead of clearing anything, the way PowerShell 5.1's line editor
//! treats an unbound Ctrl-U. `"partial-clear"` has the `clear` byte erase
//! only the buffer's last UTF-16 unit, the way a binding that silently
//! no-ops past the cursor would leave a body neither intact nor gone.
//! `"line-clear"` has the `clear` byte erase only the last display line the
//! composer paints, the way a CLI whose line editor kills one visual line per
//! Ctrl-U leaves a body that wraps over several lines: it takes one `clear`
//! byte per line to empty the buffer, and an extra one on an empty buffer does
//! nothing. `"ignore-clear"` drops the
//! `clear` byte altogether, the way a CLI with no binding for it does.
//! `"held:<scope>"` (`<scope>` one of the values above but
//! `"hold-until-clear"`, or empty) never shows a pasted body by itself: what a
//! bracketed paste put in the buffer stays unpainted until the next `clear`
//! byte, which then takes effect the way `<scope>` says and the result is
//! painted. Typed characters paint as usual, so whether a body shows is
//! decided by when its take-back arrives, never by a delay.
//! `"stall-until|<path>|<scope>"` reads on until a bracketed paste begins,
//! prints `STALL-BEGAN`, then reads nothing more until the file `<path>`
//! exists, prints `STALL-ENDED`, and processes what queued meanwhile in order, the `clear`
//! byte behaving as `<scope>` (any of the values above) says: a CLI stuck
//! from the body on for as long as a test chooses, whatever the machine's
//! speed, while what was typed before the body still shows.
//! `"frozen:<scope>"` paints nothing that arrives from the first bracketed
//! paste on, however long it waits: a CLI that stops showing input once the
//! body arrives, the `clear` byte behaving as `<scope>` says. `DEL` (`0x7f`) and `BS` (`0x08`) erase the buffer's last UTF-16 unit.
//! `"discard-paste:<scope>"` drops what every bracketed paste carries and
//! paints what is typed as usual, the `clear` byte behaving as `<scope>` says:
//! a CLI that throws a pasted body away but keeps the keystrokes behind it, so
//! a probe typed after the body shows alone. `"discard-first-paste:<scope>"`
//! drops only the first paste's content, the way a CLI still starting up does.
//! `"drain:<scope>"` drops every byte read until the first `Ctrl-C` (`0x03`),
//! which is then handled as usual: a CLI that swallows all input while it
//! starts and is taken out of that state by a keystroke. `drain:` and then a
//! `discard-*` prefix may stand in front of `held:` or a plain scope.
//! `"keep-on-interrupt:<scope>"` makes `Ctrl-C` (`0x03`) leave the composer as
//! it is, the way a CLI that takes the key for something else does; it stands
//! in front of `held:` or a plain scope.
//! `"takes-paste-on-typing:<scope>"` empties the buffer when a typed byte
//! arrives after a paste, the way a receiver that has taken a folded envelope
//! in leaves a probe typed behind it alone on the screen; it stands in front
//! of `held:` or a plain scope.
//!
//! The last two arguments draw a dialog or a status line over the composer
//! region, the way a CLI raises an approval prompt. Omitted or empty, nothing
//! changes. `overlay` is `<trigger>:<style>[:until|<path>|:until-byte|<hex>]`, and
//! `overlay_heading` is the first line drawn, the one a detection rule
//! matches. `trigger` is `key` (byte `0x01` draws the overlay, `0x02` removes
//! it), `body` (drawn once, as soon as a paste leaves the buffer non-empty)
//! `clear` (drawn once, right after the read carrying the first `clear`
//! byte has been applied to the buffer), `byte=<hex>` (the same, for the
//! first read carrying that byte: how a dialog is raised by the daemon's own
//! probe or erase) or `erase` (the same, for the first `DEL` read after a
//! bracketed paste began). A `body`, `clear`, `byte` or `erase` overlay is
//! removed once the file `<path>` exists when `until|<path>` is given, or by
//! the first read of the byte `<hex>` when `until-byte|<hex>` is given: that
//! byte is then handled as if no dialog had been drawn, and the screen shows
//! its effect with the dialog gone in a single frame. It stays drawn
//! otherwise. Either end may be preceded by `torn|<shape>|<path>|` (or the
//! whole third part be `torn|<shape>|<path>`): the dialog then takes input at
//! once but is painted in two writes, the way a CLI caught between erasing
//! its composer and drawing a dialog shows a torn frame. The first write
//! draws only `<shape>`; the full dialog follows once the file `<path>`
//! exists. `<shape>` is `erase` (the composer region erased, nothing drawn),
//! `option` (one indented option row carrying the composer's marker, as a
//! dialog's selected row drawn with the composer's glyph), `marker` (the
//! composer's marker and separator alone, the cursor right after them: a
//! dialog's option rows cut short where they begin, reading as an empty
//! composer), or `echo=<text>` (more continuation rows reading `<text>` than
//! the screen holds, the cursor after a last, empty one: a response indented
//! like the composer). The first write of `torn|` hides the cursor and never
//! shows it again; `torn-sync|` in its place opens a synchronized update
//! (`ESC[?2026h`) instead, the cursor left shown, and the second write closes
//! it at the frame's end. `style` is
//! `hidden` (option rows follow the heading and the composer is not drawn),
//! `option` (the selected option is drawn as a composer-shaped line reading
//! `1. Yes`), `beside` (the composer stays drawn below the heading) or
//! `working` (a status line above a composer that keeps taking input). Every
//! style but `working` is a dialog: while it is drawn, the bytes written to
//! this process never reach the buffer, and each read prints `APPROVED` if it
//! held a carriage return and `DIALOG-INPUT` if it held anything else.
//!
//! Every frame hides the cursor while it paints and shows it again at its
//! end, the way a real CLI brackets a repaint; a frame drawing a dialog leaves
//! it hidden, as the real CLIs' approval dialogs do.
//!
//! One further argument, `input_log` (argv index 14), is a file path to which
//! every byte read from stdin is appended as it is read, before any
//! processing: what the child received, independent of what it paints.
//! A launch profile's argv admits no empty element, so `-` stands for "none"
//! in the `clear_scope`, `overlay` and `input_log` slots.
//!
//! The last argument, `paste_enter_window_ms` (argv index 15), models a CLI
//! that takes an Enter arriving within that many milliseconds of a bracketed
//! paste's end as a line break (`PASTE_ENTER_SUPPRESS_WINDOW` in
//! `openai/codex`): the time the daemon leaves between a body and its submit
//! key is then what decides whether the body is submitted. Omitted or `0`,
//! no window applies.
//!
//! One further argument, `receive_mode` (argv index 16), is a `;`-separated
//! list of how the receiver takes a submitted line. Omitted, empty or `-`,
//! nothing changes. `silent` makes a taken line print `TAKEN` where it printed
//! `SUBMITTED <body>`, so no detection rule keyed to `SUBMITTED` sees the
//! receiver react. `take-on|<path>` (it implies `silent`) takes a submit key
//! only once the file `<path>` exists: until then the composer keeps showing
//! the body in a frame that never finishes (the cursor stays hidden), and the
//! line is taken, the composer emptied and drawn finished, the moment the file
//! appears. `hold-probe|<path>` stops reading at the first typed byte after the
//! first bracketed paste until the file `<path>` exists, with the pasted body
//! drawn meanwhile: what a delivery writes behind its body stays unread, and
//! so unpainted, for as long as a test chooses.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

// The shared module's byte-accepting half is unused here: this fixture drives
// its own buffer through the state machine below.
#[allow(dead_code)]
#[path = "shared/composer_paint.rs"]
mod composer_paint;

use composer_paint::{ComposerStyle, Output, Overlay, OverlayComposer, Paint, spawn_reflector};

/// Lines of filler output, enough to push the marker line out of a
/// `bottom_line_count`-sized detection window: the only way this fixture can
/// move a window out of a ready status.
const FILLER_LINES: usize = 40;

/// `overlay`'s `key` trigger: draws the overlay.
const OVERLAY_SHOW: u8 = 0x01;
/// `overlay`'s `key` trigger: removes the overlay.
const OVERLAY_HIDE: u8 = 0x02;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// What `clear_scope` selects: how the `clear` byte (`0x15`) behaves. See
/// the module doc comment for what each named value models.
#[derive(Clone, Copy)]
enum ClearScope {
    /// `clear_scope` omitted or empty: the clear byte empties the buffer.
    Ordinary,
    /// `clear_scope == "hold-until-clear"`: see [`State::held`].
    HoldUntilClear,
    /// `clear_scope == "literal-clear"`: the clear byte types `^U` instead of
    /// clearing anything.
    LiteralClear,
    /// `clear_scope == "partial-clear"`: the clear byte erases only the
    /// buffer's last UTF-16 unit.
    PartialClear,
    /// `clear_scope == "line-clear"`: the clear byte erases the buffer's last
    /// painted display line.
    LineClear,
    /// `clear_scope == "ignore-clear"`: the clear byte does nothing.
    IgnoreClear,
}

impl ClearScope {
    /// Parses `clear_scope`'s positional argument, exiting the process on a
    /// value it cannot parse (a misspelled mode fails loudly instead of
    /// silently running as [`ClearScope::Ordinary`]).
    fn parse(value: &str) -> Self {
        match value {
            "" => ClearScope::Ordinary,
            "hold-until-clear" => ClearScope::HoldUntilClear,
            "literal-clear" => ClearScope::LiteralClear,
            "partial-clear" => ClearScope::PartialClear,
            "line-clear" => ClearScope::LineClear,
            "ignore-clear" => ClearScope::IgnoreClear,
            _ => {
                eprintln!("submit_child: cannot parse clear_scope {value:?}");
                std::process::exit(2);
            }
        }
    }
}

/// Removes the last UTF-16 unit from `buffer` (read as UTF-8 text),
/// re-encoding the remainder back into valid UTF-8. A no-op on an empty
/// buffer, used by [`ClearScope::PartialClear`].
fn take_back_one_unit(buffer: &mut Vec<u8>) {
    if buffer.is_empty() {
        return;
    }
    let mut units: Vec<u16> = String::from_utf8_lossy(buffer).encode_utf16().collect();
    units.pop();
    *buffer = String::from_utf16_lossy(&units).into_bytes();
}

/// What the `clear` byte does to `state`'s buffer under `settings.clear_scope`.
fn apply_clear(state: &mut State, settings: &Settings<'_>, chunk_had_append: &mut bool) {
    match settings.clear_scope {
        ClearScope::LiteralClear => {
            state.append(b'^', settings, chunk_had_append);
            state.append(b'U', settings, chunk_had_append);
        }
        ClearScope::PartialClear => take_back_one_unit(&mut state.buffer),
        ClearScope::LineClear => {
            state.buffer = composer_paint::without_last_painted_line(&state.buffer);
        }
        ClearScope::IgnoreClear => {}
        ClearScope::Ordinary | ClearScope::HoldUntilClear => {
            state.buffer.clear();
            state.pasted_len = 0;
        }
    }
}

/// What reading the bracketed-paste marker a `0x1b` began, one more byte at a
/// time, produced.
enum MarkerStep {
    /// Fewer bytes than a marker holds have arrived; wait for more.
    Matching,
    /// The marker just completed.
    Matched,
    /// The bytes are not the marker this state expects. Carries them.
    Unmodeled(Vec<u8>),
}

/// Mutable state carried across reads.
struct State {
    /// Bytes typed since the last submit, clear or filler burst.
    buffer: Vec<u8>,
    in_paste: bool,
    /// Bytes tentatively consumed while matching a bracketed-paste marker.
    pending: Vec<u8>,
    /// Submit keys still to be taken as line breaks since the last paste ended.
    paste_guard_left: u64,
    /// When the last bracketed paste ended.
    last_paste_end: Option<Instant>,
    /// Bracketed-paste completions seen so far (the `PASTE <n> buffered=<bytes>` counter).
    frames: u32,
    /// `buffer` holds a write awaiting release by `clear`
    /// (`clear_scope` "hold-until-clear" and "held:<scope>" only).
    held: bool,
    /// How many leading bytes of `buffer` arrived in a paste.
    pasted_len: usize,
    /// Bracketed pastes begun so far.
    pastes_begun: u32,
    /// The `hold-probe` stop has been made.
    probe_held: bool,
}

impl State {
    fn new() -> Self {
        State {
            buffer: Vec::new(),
            in_paste: false,
            pending: Vec::new(),
            paste_guard_left: 0,
            last_paste_end: None,
            frames: 0,
            held: false,
            pasted_len: 0,
            pastes_begun: 0,
            probe_held: false,
        }
    }

    fn append(&mut self, byte: u8, settings: &Settings<'_>, chunk_had_append: &mut bool) {
        if self.in_paste && self.pastes_begun <= settings.discard_pastes {
            return;
        }
        if settings.typing_takes_paste && !self.in_paste && self.pasted_len > 0 {
            self.buffer.clear();
            self.pasted_len = 0;
        }
        self.buffer.push(byte);
        if self.in_paste {
            self.pasted_len = self.buffer.len();
        }
        *chunk_had_append = true;
        self.held |= matches!(settings.clear_scope, ClearScope::HoldUntilClear)
            || (settings.held_paste && self.in_paste);
    }

    /// The marker this state is currently trying to match: the entry marker
    /// while outside a paste, the exit marker while inside one.
    fn active_marker(&self) -> &'static [u8] {
        if self.in_paste {
            PASTE_END
        } else {
            PASTE_START
        }
    }

    /// Every `0x1b` written to this fixture opens or closes a bracketed
    /// paste, so a marker-length sequence is read whole.
    fn feed_marker(&mut self, byte: u8) -> MarkerStep {
        self.pending.push(byte);
        let target = self.active_marker();
        if self.pending.len() < target.len() {
            return MarkerStep::Matching;
        }
        let sequence = std::mem::take(&mut self.pending);
        if sequence == target {
            MarkerStep::Matched
        } else {
            MarkerStep::Unmodeled(sequence)
        }
    }
}

/// Settings for one run, threaded through byte processing.
struct Settings<'a> {
    marker: &'a str,
    is_react: bool,
    paste_guard_keys: u64,
    /// A submit key this soon after a paste's end is a line break.
    paste_enter_window: Duration,
    clear_scope: ClearScope,
    /// `clear_scope` was given as `"held:<scope>"`.
    held_paste: bool,
    /// The bracketed pastes whose content is dropped: the first
    /// `discard_pastes` of them (`u32::MAX` for all).
    discard_pastes: u32,
    /// `clear_scope` was given with a `"keep-on-interrupt:"` prefix.
    interrupt_keeps_buffer: bool,
    /// `clear_scope` was given with a `"takes-paste-on-typing:"` prefix.
    typing_takes_paste: bool,
    /// A taken line prints `TAKEN` instead of `SUBMITTED <body>`.
    silent: bool,
    /// Submit keys are taken only once this file exists.
    take_on: Option<Arc<TakeOn>>,
    /// Reading stops at the first typed byte after a paste until this file
    /// exists.
    hold_probe: Option<std::path::PathBuf>,
}

/// The `take-on` receive mode: a submit key that arrives before the file
/// exists is held, and taken as soon as the file appears.
struct TakeOn {
    gone_when_exists: std::path::PathBuf,
    /// A submit key is held and not yet taken.
    held: Mutex<bool>,
    /// The held line was taken on another thread, so the buffer this thread
    /// owns still holds it.
    released: AtomicBool,
}

impl TakeOn {
    /// Whether a submit key arriving now is held rather than taken. Decided
    /// under the lock the releasing thread takes, so a key is never held
    /// after the release has looked.
    fn defer(&self) -> bool {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if self.gone_when_exists.exists() {
            return false;
        }
        *held = true;
        true
    }
}

/// The composer drawn the way a CLI caught mid-repaint draws it: the body is
/// there, the cursor is hidden and never shown again by this frame.
fn unfinished_composer() -> Overlay {
    Overlay {
        lines: Vec::new(),
        composer: OverlayComposer::Shown,
        paint: Paint::CursorHidden,
        closes_sync: false,
    }
}

/// Parsed `receive_mode` argument.
struct ReceiveMode {
    silent: bool,
    take_on: Option<Arc<TakeOn>>,
    hold_probe: Option<std::path::PathBuf>,
}

impl ReceiveMode {
    /// Exits the process on a value it cannot parse, so a misspelled mode
    /// fails loudly instead of running as the default receiver.
    fn from_args(index: usize) -> Self {
        let spec = std::env::args()
            .nth(index)
            .filter(|value| !value.is_empty() && value != "-")
            .unwrap_or_default();
        let mut mode = ReceiveMode {
            silent: false,
            take_on: None,
            hold_probe: None,
        };
        for part in spec.split(';').filter(|part| !part.is_empty()) {
            if part == "silent" {
                mode.silent = true;
            } else if let Some(path) = part.strip_prefix("take-on|") {
                mode.silent = true;
                mode.take_on = Some(Arc::new(TakeOn {
                    gone_when_exists: std::path::PathBuf::from(path),
                    held: Mutex::new(false),
                    released: AtomicBool::new(false),
                }));
            } else if let Some(path) = part.strip_prefix("hold-probe|") {
                mode.hold_probe = Some(std::path::PathBuf::from(path));
            } else {
                eprintln!("submit_child: cannot parse receive_mode {part:?}");
                std::process::exit(2);
            }
        }
        mode
    }
}

/// What draws the overlay (see the module doc comment).
#[derive(Clone, Copy, PartialEq, Eq)]
enum OverlayTrigger {
    Key,
    Body,
    Clear,
    /// The first read carrying this byte.
    Byte(u8),
    /// The first read carrying `DEL` once a bracketed paste has begun.
    Erase,
}

/// The parsed `overlay` / `overlay_heading` arguments.
struct OverlaySettings {
    trigger: OverlayTrigger,
    overlay: Overlay,
    /// Whether the overlay is a dialog that takes the bytes written to it.
    is_dialog: bool,
    /// A fired overlay is removed once the file at this path exists, as late
    /// as the test decides; `None` keeps it drawn.
    gone_when_exists: Option<std::path::PathBuf>,
    /// A fired overlay is removed by the first read of this byte, which is
    /// then handled as if no dialog had been drawn.
    gone_on_byte: Option<u8>,
    /// Paints the overlay in two writes (see the module doc comment).
    torn: Option<TornFrame>,
}

/// More rows than the PTY a test opens shows, so an `echo` shape reaches the
/// screen's top row.
const TORN_ECHO_ROWS: usize = 30;

/// The first of an overlay's two writes, and when the second follows.
struct TornFrame {
    first: Overlay,
    rest_when_exists: std::path::PathBuf,
    /// The first write opens a synchronized update the second one closes.
    sync: bool,
}

impl TornFrame {
    /// Parses `<shape>` and `<path>`, the composer's own marker, separator
    /// and continuation prefix taken from the positional arguments.
    fn parse(shape: &str, path: &str, sync: bool) -> Option<Self> {
        let arg = |index: usize| std::env::args().nth(index).unwrap_or_default();
        let lines = match shape {
            "erase" => Vec::new(),
            "option" => vec![format!("  {}{}1. Yes", arg(6), arg(7))],
            "marker" => vec![format!("{}{}", arg(6), arg(7))],
            other => {
                let text = other.strip_prefix("echo=")?;
                let prefix = arg(8);
                std::iter::repeat_n(format!("{prefix}{text}"), TORN_ECHO_ROWS)
                    .chain(std::iter::once(prefix))
                    .collect()
            }
        };
        Some(TornFrame {
            first: Overlay {
                lines,
                composer: OverlayComposer::Hidden,
                paint: if sync {
                    Paint::SyncOpen
                } else {
                    Paint::CursorHidden
                },
                closes_sync: false,
            },
            rest_when_exists: std::path::PathBuf::from(path),
            sync,
        })
    }
}

impl OverlayTrigger {
    /// The byte whose first read draws a `Clear` or `Byte` overlay.
    fn trigger_byte(self) -> Option<u8> {
        match self {
            OverlayTrigger::Clear => Some(0x15),
            OverlayTrigger::Byte(byte) => Some(byte),
            OverlayTrigger::Erase => Some(0x7f),
            OverlayTrigger::Key | OverlayTrigger::Body => None,
        }
    }
}

impl OverlaySettings {
    /// Whether reading `byte` fires this overlay, `pastes_begun` bracketed
    /// pastes having begun so far.
    fn fires_on(&self, byte: u8, pastes_begun: u32) -> bool {
        self.trigger.trigger_byte() == Some(byte)
            && (self.trigger != OverlayTrigger::Erase || pastes_begun > 0)
    }

    /// `None` when the arguments are absent or empty; exits the process on a
    /// value it cannot parse, so a misspelled mode fails loudly instead of
    /// running as the default fixture.
    fn from_args(first: usize) -> Option<Self> {
        let spec = std::env::args()
            .nth(first)
            .filter(|value| !value.is_empty() && value != "-")?;
        let heading = std::env::args().nth(first + 1).unwrap_or_default();
        let parsed = Self::parse(&spec, heading);
        if parsed.is_none() {
            eprintln!("submit_child: cannot parse overlay argument {spec:?}");
            std::process::exit(2);
        }
        parsed
    }

    fn parse(spec: &str, heading: String) -> Option<Self> {
        let mut parts = spec.splitn(3, ':');
        let trigger = match parts.next()? {
            "key" => OverlayTrigger::Key,
            "body" => OverlayTrigger::Body,
            "clear" => OverlayTrigger::Clear,
            "erase" => OverlayTrigger::Erase,
            other => {
                OverlayTrigger::Byte(u8::from_str_radix(other.strip_prefix("byte=")?, 16).ok()?)
            }
        };
        let (overlay, is_dialog) = match parts.next()? {
            "hidden" => (
                Overlay {
                    lines: vec![
                        heading,
                        "  1. Yes".to_string(),
                        "  2. No".to_string(),
                        "Esc to cancel".to_string(),
                    ],
                    composer: OverlayComposer::Hidden,
                    paint: Paint::CursorHidden,
                    closes_sync: false,
                },
                true,
            ),
            "option" => (
                Overlay {
                    lines: vec![heading],
                    composer: OverlayComposer::Replaced("1. Yes".to_string()),
                    paint: Paint::CursorHidden,
                    closes_sync: false,
                },
                true,
            ),
            "beside" => (
                Overlay {
                    lines: vec![heading],
                    composer: OverlayComposer::Shown,
                    paint: Paint::CursorHidden,
                    closes_sync: false,
                },
                true,
            ),
            "working" => (
                Overlay {
                    lines: vec![heading],
                    composer: OverlayComposer::Shown,
                    paint: Paint::Finished,
                    closes_sync: false,
                },
                false,
            ),
            _ => return None,
        };
        let (torn, end) = match parts.next() {
            Some(rest) => {
                let torn = match rest.strip_prefix("torn|") {
                    Some(torn) => Some((torn, false)),
                    None => rest.strip_prefix("torn-sync|").map(|torn| (torn, true)),
                };
                match torn {
                    Some((torn, sync)) => {
                        let (shape, after_shape) = torn.split_once('|')?;
                        let (path, end) = match after_shape.split_once('|') {
                            Some((path, end)) => (path, Some(end)),
                            None => (after_shape, None),
                        };
                        (Some(TornFrame::parse(shape, path, sync)?), end)
                    }
                    None => (None, Some(rest)),
                }
            }
            None => (None, None),
        };
        let (gone_when_exists, gone_on_byte) = match end {
            None => (None, None),
            Some(rest) => match rest.strip_prefix("until|") {
                Some(path) => (Some(std::path::PathBuf::from(path)), None),
                None => (
                    None,
                    Some(u8::from_str_radix(rest.strip_prefix("until-byte|")?, 16).ok()?),
                ),
            },
        };
        Some(OverlaySettings {
            trigger,
            overlay,
            is_dialog,
            gone_when_exists,
            gone_on_byte,
            torn,
        })
    }
}

/// Draws or removes the overlay, keeping `drawn` in step with the screen.
fn set_overlay(
    out: &Output,
    drawn: &AtomicBool,
    settings: &OverlaySettings,
    show: bool,
) -> io::Result<()> {
    drawn.store(show, Ordering::SeqCst);
    out.set_overlay(show.then(|| settings.overlay.clone()))
}

/// Draws the overlay, or for a torn one its first write, with the full
/// overlay following on a thread of its own once its file exists.
fn show_overlay(
    out: &Arc<Output>,
    drawn: &Arc<AtomicBool>,
    settings: &Arc<OverlaySettings>,
) -> io::Result<()> {
    let Some(torn) = &settings.torn else {
        return set_overlay(out, drawn, settings, true);
    };
    drawn.store(true, Ordering::SeqCst);
    out.set_overlay(Some(torn.first.clone()))?;
    let rest_when_exists = torn.rest_when_exists.clone();
    let rest = Overlay {
        closes_sync: torn.sync,
        ..settings.overlay.clone()
    };
    let (out, drawn) = (Arc::clone(out), Arc::clone(drawn));
    std::thread::spawn(move || {
        while !rest_when_exists.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        if drawn.load(Ordering::SeqCst) {
            let _ = out.set_overlay(Some(rest));
        }
    });
    Ok(())
}

/// Draws the overlay its trigger has just fired, and removes it again once
/// its end comes, on a thread of its own so input keeps being read meanwhile.
fn fire_overlay(
    out: &Arc<Output>,
    drawn: &Arc<AtomicBool>,
    settings: &Arc<OverlaySettings>,
) -> io::Result<()> {
    show_overlay(out, drawn, settings)?;
    let Some(gone) = settings.gone_when_exists.clone() else {
        return Ok(());
    };
    let (out, drawn, settings) = (Arc::clone(out), Arc::clone(drawn), Arc::clone(settings));
    std::thread::spawn(move || {
        while !gone.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = set_overlay(&out, &drawn, &settings, false);
    });
    Ok(())
}

/// Writes and flushes one output line.
fn emit(out: &Output, line: &str) -> io::Result<()> {
    out.line(line)
}

/// Pushes the marker line out of a `bottom_line_count`-sized detection window.
fn emit_filler(out: &Output) -> io::Result<()> {
    for _ in 0..FILLER_LINES {
        emit(out, ".")?;
    }
    Ok(())
}

/// Processes one byte read after the startup discard.
fn process_byte(
    state: &mut State,
    byte: u8,
    settings: &Settings<'_>,
    out: &Output,
    chunk_had_append: &mut bool,
) -> io::Result<()> {
    if !state.pending.is_empty() || byte == 0x1b {
        return match state.feed_marker(byte) {
            MarkerStep::Matching => Ok(()),
            MarkerStep::Matched => {
                if state.in_paste {
                    state.in_paste = false;
                    state.paste_guard_left = settings.paste_guard_keys;
                    state.last_paste_end = Some(Instant::now());
                    state.frames += 1;
                    emit(
                        out,
                        &format!("PASTE {} buffered={}", state.frames, state.buffer.len()),
                    )?;
                    if settings.is_react {
                        emit(out, "REACT")
                    } else {
                        emit(out, settings.marker)
                    }
                } else {
                    state.in_paste = true;
                    state.pastes_begun = state.pastes_begun.saturating_add(1);
                    Ok(())
                }
            }
            MarkerStep::Unmodeled(sequence) => {
                eprintln!("submit_child: cannot read escape sequence {sequence:?}");
                std::process::exit(2);
            }
        };
    }

    match byte {
        0x15 if settings.held_paste && state.held => {
            state.held = false;
            apply_clear(state, settings, chunk_had_append);
        }
        0x15 if state.held => {
            state.held = false;
            emit(out, "RELEASED")?;
        }
        0x15 => apply_clear(state, settings, chunk_had_append),
        0x7f | 0x08 => take_back_one_unit(&mut state.buffer),
        0x03 if settings.interrupt_keeps_buffer => {}
        0x03 => {
            state.buffer.clear();
            state.pasted_len = 0;
            state.held = false;
            // Sync before painting the filler lines so their composer
            // repaint shows the buffer already emptied, in the same frame,
            // rather than a stale body a later, separately timed repaint
            // would otherwise still be carrying.
            out.set_body("");
            emit_filler(out)?;
        }
        // Inside a paste a CR/LF is content, as in a real CLI's composer.
        b'\r' | b'\n' if !state.in_paste => {
            let inside_paste_guard = state.paste_guard_left > 0
                || state
                    .last_paste_end
                    .is_some_and(|ended| ended.elapsed() < settings.paste_enter_window);
            if state.paste_guard_left > 0 {
                state.paste_guard_left -= 1;
            }
            // A CR/LF read together with typed characters is line editing
            // inside that input, not a submit.
            let discarded_as_line_break = *chunk_had_append || inside_paste_guard;
            if !discarded_as_line_break && !state.buffer.is_empty() {
                if settings
                    .take_on
                    .as_ref()
                    .is_some_and(|take_on| take_on.defer())
                {
                    return out.set_overlay(Some(unfinished_composer()));
                }
                let body = String::from_utf8_lossy(&state.buffer).into_owned();
                state.held = false;
                state.buffer.clear();
                state.pasted_len = 0;
                // Sync before emitting the accepted line so it paints
                // alongside the emptied composer in the same frame, instead
                // of a later, separately timed repaint still showing the
                // submitted body as an unsent draft.
                out.set_body("");
                if settings.silent {
                    emit(out, "TAKEN")?;
                } else {
                    emit(out, &format!("SUBMITTED {body}"))?;
                }
            }
        }
        _ => state.append(byte, settings, chunk_had_append),
    }
    Ok(())
}

fn main() -> io::Result<()> {
    // A real vendor CLI raw-modes its own terminal on start; without that, the
    // platform's default line discipline echoes and reshapes the bytes written to
    // this process before the byte-wise state machine below ever sees them.
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("submit_child: cannot enter raw console input: {error}");
        std::process::exit(2);
    }

    let marker = std::env::args().nth(1).unwrap_or_default();
    let mut discard_reads: u64 = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let paste_guard_keys: u64 = std::env::args()
        .nth(3)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let paste_enter_window = Duration::from_millis(
        std::env::args()
            .nth(15)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    );
    let react_on_body = std::env::args().nth(4).unwrap_or_default();
    let ready_gate = std::env::args()
        .nth(5)
        .and_then(|arg| arg.strip_prefix("until|").map(std::path::PathBuf::from));
    let composer = ComposerStyle::from_args(6);
    // Positional, right after the (possibly empty) fold_placeholder slot
    // `ComposerStyle::from_args` reads; see the module doc comment.
    let clear_scope_full = std::env::args()
        .nth(11)
        .filter(|value| value != "-")
        .unwrap_or_default();
    let (stall_until, clear_scope_arg) = match clear_scope_full.strip_prefix("stall-until|") {
        Some(rest) => match rest.split_once('|') {
            Some((path, scope)) => (Some(std::path::PathBuf::from(path)), scope.to_string()),
            None => {
                eprintln!("submit_child: cannot parse clear_scope {clear_scope_full:?}");
                std::process::exit(2);
            }
        },
        None => (None, clear_scope_full.clone()),
    };
    let (frozen_from_paste, clear_scope_arg) = match clear_scope_arg.strip_prefix("frozen:") {
        Some(inner) => (true, inner.to_string()),
        None => (false, clear_scope_arg),
    };
    let (draining, clear_scope_arg) = match clear_scope_arg.strip_prefix("drain:") {
        Some(inner) => (true, inner.to_string()),
        None => (false, clear_scope_arg),
    };
    let (discard_pastes, clear_scope_arg) =
        if let Some(inner) = clear_scope_arg.strip_prefix("discard-paste:") {
            (u32::MAX, inner.to_string())
        } else if let Some(inner) = clear_scope_arg.strip_prefix("discard-first-paste:") {
            (1, inner.to_string())
        } else {
            (0, clear_scope_arg)
        };
    let (interrupt_keeps_buffer, clear_scope_arg) =
        match clear_scope_arg.strip_prefix("keep-on-interrupt:") {
            Some(inner) => (true, inner.to_string()),
            None => (false, clear_scope_arg),
        };
    let (typing_takes_paste, clear_scope_arg) =
        match clear_scope_arg.strip_prefix("takes-paste-on-typing:") {
            Some(inner) => (true, inner.to_string()),
            None => (false, clear_scope_arg),
        };
    let (held_paste, clear_scope) = match clear_scope_arg.strip_prefix("held:") {
        Some(inner) => (true, ClearScope::parse(inner)),
        None => (false, ClearScope::parse(&clear_scope_arg)),
    };
    let receive_mode = ReceiveMode::from_args(16);
    let mut input_log = std::env::args()
        .nth(14)
        .filter(|path| !path.is_empty() && path != "-")
        .map(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        })
        .transpose()?;
    let overlay = OverlaySettings::from_args(12).map(Arc::new);
    let overlay_drawn = Arc::new(AtomicBool::new(false));
    let mut overlay_fired = false;

    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let (style, reflect) = match composer {
        Some((style, reflect)) => (Some(style), Some(reflect)),
        None => (None, None),
    };
    let settings = Settings {
        marker: &marker,
        is_react: react_on_body == "1",
        paste_guard_keys,
        paste_enter_window,
        clear_scope,
        held_paste,
        discard_pastes,
        interrupt_keeps_buffer,
        typing_takes_paste,
        silent: receive_mode.silent,
        take_on: receive_mode.take_on,
        hold_probe: receive_mode.hold_probe,
    };
    let out = Arc::new(Output::new(style));
    let reflector = reflect.map(|reflect| spawn_reflector(Arc::clone(&out), reflect));
    if let Some(take_on) = settings.take_on.clone() {
        let out = Arc::clone(&out);
        std::thread::spawn(move || {
            while !take_on.gone_when_exists.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut held = take_on.held.lock().unwrap_or_else(PoisonError::into_inner);
            if std::mem::take(&mut *held) {
                take_on.released.store(true, Ordering::SeqCst);
                out.set_body("");
                let _ = out.set_overlay(None);
                let _ = out.line("TAKEN");
            }
        });
    }
    if let Some(path) = &ready_gate {
        while !path.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    emit(&out, &marker)?;

    let mut stall_until = stall_until;
    let mut frozen_from_paste = frozen_from_paste;
    let mut draining = draining;
    let mut state = State::new();
    let mut chunk = vec![0u8; 4096];

    loop {
        let n = handle.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if let Some(log) = &mut input_log {
            log.write_all(&chunk[..n])?;
        }

        if discard_reads > 0 {
            discard_reads -= 1;
            continue;
        }

        if settings
            .take_on
            .as_ref()
            .is_some_and(|take_on| take_on.released.swap(false, Ordering::SeqCst))
        {
            state.buffer.clear();
            state.pasted_len = 0;
            state.held = false;
        }
        let mut chunk_had_append = false;
        let mut dialog_took_submit = false;
        let mut dialog_took_input = false;
        let mut chunk_had_trigger_byte = false;
        let mut overlay_removal_pending = false;
        for &byte in &chunk[..n] {
            if draining {
                if byte != 0x03 {
                    continue;
                }
                draining = false;
            }
            if let Some(overlay) = &overlay {
                if overlay.trigger == OverlayTrigger::Key
                    && (byte == OVERLAY_SHOW || byte == OVERLAY_HIDE)
                {
                    if byte == OVERLAY_SHOW {
                        show_overlay(&out, &overlay_drawn, overlay)?;
                    } else {
                        set_overlay(&out, &overlay_drawn, overlay, false)?;
                    }
                    continue;
                }
                if overlay.is_dialog && overlay_drawn.load(Ordering::SeqCst) {
                    if overlay.gone_on_byte == Some(byte) {
                        // The dialog is gone from here on; its frame is drawn
                        // once this read's body has been synced.
                        overlay_drawn.store(false, Ordering::SeqCst);
                        overlay_removal_pending = true;
                    } else {
                        if byte == b'\r' {
                            dialog_took_submit = true;
                        } else {
                            dialog_took_input = true;
                        }
                        continue;
                    }
                }
            }
            chunk_had_trigger_byte |= overlay
                .as_ref()
                .is_some_and(|overlay| overlay.fires_on(byte, state.pastes_begun));
            if let Some(path) = &settings.hold_probe
                && !state.probe_held
                && state.pastes_begun > 0
                && !state.in_paste
                && state.pending.is_empty()
                && byte != 0x1b
            {
                state.probe_held = true;
                out.set_body_pasted_through(
                    &String::from_utf8_lossy(&state.buffer),
                    state.pasted_len.min(state.buffer.len()),
                );
                if let Some(reflector) = &reflector {
                    let _ = reflector.send(());
                }
                while !path.exists() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            let was_in_paste = state.in_paste;
            process_byte(&mut state, byte, &settings, &out, &mut chunk_had_append)?;
            if !was_in_paste && state.in_paste {
                if std::mem::take(&mut frozen_from_paste) {
                    out.freeze_body();
                }
                if let Some(path) = stall_until.take() {
                    emit(&out, "STALL-BEGAN")?;
                    while !path.exists() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    emit(&out, "STALL-ENDED")?;
                }
            }
        }
        if dialog_took_submit {
            emit(&out, "APPROVED")?;
        }
        if dialog_took_input {
            emit(&out, "DIALOG-INPUT")?;
        }
        if let Some(overlay) = &overlay
            && overlay.trigger == OverlayTrigger::Body
            && !overlay_fired
            && state.pasted_len > 0
        {
            overlay_fired = true;
            fire_overlay(&out, &overlay_drawn, overlay)?;
        }
        let reflects = reflector.is_some() && !state.held;
        if reflects {
            out.set_body_pasted_through(
                &String::from_utf8_lossy(&state.buffer),
                state.pasted_len.min(state.buffer.len()),
            );
        }
        if overlay_removal_pending && let Some(overlay) = &overlay {
            set_overlay(&out, &overlay_drawn, overlay, false)?;
        }
        // After the body sync above, so the dialog is drawn beside what the
        // trigger byte left in the composer, and before the reflector is woken,
        // so the reaction to the trigger byte reaches the screen as one frame
        // rather than a dialog-less repaint followed by the dialog.
        if let Some(overlay) = &overlay
            && overlay.trigger.trigger_byte().is_some()
            && !overlay_fired
            && chunk_had_trigger_byte
        {
            overlay_fired = true;
            fire_overlay(&out, &overlay_drawn, overlay)?;
        }
        if reflects && let Some(reflector) = &reflector {
            let _ = reflector.send(());
        }
    }

    Ok(())
}
