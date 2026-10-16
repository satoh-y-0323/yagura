//! Composer-draw mode: how a delivery-receiving fixture paints the bytes
//! written to it as one marked region a screen reader can recover.
//!
//! Included by each fixture with `#[path]` rather than lifted into the
//! library: a `[[bin]]` target cannot use the crate's dev-dependencies, and
//! the library ships in the daemon, where fixture drawing has no place. This
//! directory holds no `main.rs`, so cargo builds no binary from it.

use std::io::{self, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Longest run of body characters painted on one display line. Kept well
/// under the PTY's width so a painted line never reaches the terminal's own
/// wrap point, which would leave a line without the continuation prefix a
/// reader walks the region by.
const COMPOSER_WRAP_CHARS: usize = 32;

/// How composer-draw mode paints unsubmitted input: the marker naming the
/// composer line, the separator between marker and body, the prefix each
/// wrapped line carries, and what stands on screen in place of a non-empty
/// body when the composer folds it. Taken from argv so a fixture and the
/// detection definition that reads its screen never hold two copies of the
/// same values.
pub(crate) struct ComposerStyle {
    marker: String,
    separator: String,
    continuation_prefix: String,
    fold_placeholder: Option<String>,
}

impl ComposerStyle {
    /// The style and the reflection timing read from the positional arguments
    /// starting at `first`, or `None` when the fixture was launched without
    /// them and keeps its default, non-drawing behavior.
    ///
    /// The timing argument is a delay in milliseconds, or `pause|<path>`: no
    /// delay, but nothing is painted while the file `<path>` exists, so a
    /// test decides when what was read shows. `pause|<path>|<ms>` also delays
    /// every paint by `<ms>`, the first one after the pause ends included, so
    /// a test holds the screen at a stale reading for a margin that does not
    /// depend on the machine's speed. One further argument may follow
    /// it: the placeholder a folding composer paints instead of the body.
    /// Absent or empty, the body is painted as typed.
    pub(crate) fn from_args(first: usize) -> Option<(Self, Reflect)> {
        let mut args = std::env::args().skip(first);
        let marker = args.next()?;
        let separator = args.next()?;
        let continuation_prefix = args.next()?;
        let timing = args.next()?;
        let reflect = match timing.strip_prefix("pause|") {
            Some(spec) => {
                let (path, delay) = match spec.rsplit_once('|') {
                    Some((path, ms)) => (path, ms.parse().ok().map(Duration::from_millis)?),
                    None => (spec, Duration::ZERO),
                };
                Reflect {
                    delay,
                    paused_while: Some(std::path::PathBuf::from(path)),
                }
            }
            None => Reflect {
                delay: timing.parse().ok().map(Duration::from_millis)?,
                paused_while: None,
            },
        };
        let style = ComposerStyle {
            marker,
            separator,
            continuation_prefix,
            // `-` stands for "none" where an empty argument is not allowed
            // (a launch profile's argv).
            fold_placeholder: args
                .next()
                .filter(|value| !value.is_empty() && value != "-"),
        };
        Some((style, reflect))
    }
}

/// What stands in the composer's place while an [`Overlay`] is drawn.
// Only `submit_child` draws an overlay; the other fixtures sharing this file
// never construct one.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) enum OverlayComposer {
    /// The composer is painted as usual below the overlay's lines.
    Shown,
    /// Nothing follows the overlay's lines, so the cursor rests on a line
    /// that is neither a marker line nor a continuation line.
    Hidden,
    /// A line shaped like the composer's marker line, carrying fixed text
    /// instead of the body -- a dialog's selected option drawn with the same
    /// glyph a composer uses.
    Replaced(String),
}

/// Lines a CLI draws on top of its prompt area (a dialog, a status line),
/// painted where the composer region starts.
// Only `submit_child` draws an overlay; see [`OverlayComposer`].
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct Overlay {
    pub(crate) lines: Vec<String>,
    pub(crate) composer: OverlayComposer,
    pub(crate) paint: Paint,
    /// The frame also ends a synchronized update an earlier frame left open.
    pub(crate) closes_sync: bool,
}

/// How a frame brackets what it draws, the way a real CLI marks a repaint in
/// progress: a screen reader tells a finished paint from one still under way
/// (or a dialog) by these brackets alone.
// Only `submit_child` draws an overlay; see [`OverlayComposer`].
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum Paint {
    /// `ESC[?25l` ... `ESC[?25h`: the paint is finished and the cursor shows
    /// where input goes.
    Finished,
    /// `ESC[?25l` with the cursor left hidden: a dialog, or a paint cut short.
    CursorHidden,
    /// `ESC[?2026h` left open with the cursor never hidden: a synchronized
    /// paint cut short.
    SyncOpen,
}

impl Paint {
    fn opening(self) -> &'static [u8] {
        match self {
            Paint::Finished | Paint::CursorHidden => b"\x1b[?25l",
            Paint::SyncOpen => b"\x1b[?2026h",
        }
    }

    fn closing(self) -> &'static [u8] {
        match self {
            Paint::Finished => b"\x1b[?25h",
            Paint::CursorHidden | Paint::SyncOpen => b"",
        }
    }
}

/// The fixture's output together with the composer region painted below it,
/// when composer-draw mode is on.
struct Screen {
    /// One frame being assembled; [`Screen::commit`] writes it out whole.
    frame: Vec<u8>,
    style: Option<ComposerStyle>,
    body: String,
    /// How many leading bytes of `body` arrived in a paste: the part a
    /// folding composer paints as its placeholder.
    pasted_len: usize,
    /// Set once the composer stops showing what it reads: `body` keeps what
    /// it held at that moment.
    frozen: bool,
    /// Drawn in the composer region's place while set.
    overlay: Option<Overlay>,
    /// Rows painted below the marker line, so the region is erased by walking
    /// the cursor back up exactly as far as painting pushed it down.
    extra_rows: usize,
}

impl Screen {
    fn erase(&mut self) -> io::Result<()> {
        if self.style.is_none() {
            return Ok(());
        }
        if self.extra_rows > 0 {
            write!(self.frame, "\x1b[{}A", self.extra_rows)?;
        }
        self.extra_rows = 0;
        write!(self.frame, "\r\x1b[0J")
    }

    /// Paints marker, separator and body, leaving the cursor at the end of
    /// the last painted row: a reader takes the region from the cursor
    /// upwards, so the cursor must not end up outside it.
    fn paint(&mut self) -> io::Result<()> {
        let Some(style) = self.style.as_ref() else {
            return Ok(());
        };
        let composer = match self.overlay.clone() {
            Some(overlay) => {
                for (index, line) in overlay.lines.iter().enumerate() {
                    if index > 0 {
                        write!(self.frame, "\r\n")?;
                        self.extra_rows += 1;
                    }
                    write!(self.frame, "{line}")?;
                }
                if !overlay.lines.is_empty() && !matches!(overlay.composer, OverlayComposer::Hidden)
                {
                    write!(self.frame, "\r\n")?;
                    self.extra_rows += 1;
                }
                overlay.composer
            }
            None => OverlayComposer::Shown,
        };
        match composer {
            OverlayComposer::Shown => {}
            OverlayComposer::Hidden => return Ok(()),
            OverlayComposer::Replaced(text) => {
                return write!(self.frame, "{}{}{text}", style.marker, style.separator);
            }
        }
        write!(self.frame, "{}", style.marker)?;
        // A folding composer paints a placeholder where the body would stand:
        // the region stays observable while what it holds cannot be read back.
        let (pasted, typed) = self.body.split_at(self.pasted_len.min(self.body.len()));
        let painted_source = match &style.fold_placeholder {
            Some(placeholder) if pasted.chars().any(|c| !c.is_control()) => {
                format!("{placeholder}{typed}")
            }
            _ => self.body.clone(),
        };
        // Control bytes occupy no column; a real composer does not paint them.
        let drawable: Vec<char> = painted_source.chars().filter(|c| !c.is_control()).collect();
        if !drawable.is_empty() {
            write!(self.frame, "{}", style.separator)?;
            let mut start = 0;
            while start < drawable.len() {
                if start > 0 {
                    write!(self.frame, "\r\n{}", style.continuation_prefix)?;
                    self.extra_rows += 1;
                }
                let end = wrap_end(&drawable, start);
                let painted: String = drawable
                    .get(start..end)
                    .unwrap_or_default()
                    .iter()
                    .collect();
                write!(self.frame, "{painted}")?;
                start = end;
            }
        }
        Ok(())
    }

    /// Writes the assembled frame, bracketed as the overlay's [`Paint`] says
    /// (a finished paint without one), with a single write. Rust's stdout is
    /// line-buffered and would split a frame at its last newline, letting a
    /// reader see the region erased but not yet repainted; stderr is
    /// unbuffered and, on a PTY, feeds the same terminal stream.
    fn commit(&mut self) -> io::Result<()> {
        let (paint, closes_sync) = self
            .overlay
            .as_ref()
            .map_or((Paint::Finished, false), |overlay| {
                (overlay.paint, overlay.closes_sync)
            });
        let drawn = std::mem::take(&mut self.frame);
        let mut frame = Vec::with_capacity(drawn.len() + 24);
        frame.extend_from_slice(paint.opening());
        frame.extend_from_slice(&drawn);
        frame.extend_from_slice(paint.closing());
        if closes_sync {
            frame.extend_from_slice(b"\x1b[?2026l");
        }
        let mut stderr = io::stderr().lock();
        stderr.write_all(&frame)?;
        stderr.flush()
    }
}

/// Where the painted line starting at `start` ends: at most
/// [`COMPOSER_WRAP_CHARS`] characters, and never on a space, which a reader
/// trims off the end of each painted line and would lose from the body.
fn wrap_end(drawable: &[char], start: usize) -> usize {
    let mut end = drawable.len().min(start + COMPOSER_WRAP_CHARS);
    while end > start + 1 && drawable.get(end - 1) == Some(&' ') {
        end -= 1;
    }
    end
}

/// `buffer` (read as UTF-8 text) with the last display line it paints
/// removed, the way a CLI whose clear key erases one visual line at a time
/// leaves a body that wraps over several lines.
// Only `submit_child` models that clear key; the other fixtures sharing this
// file never call it.
#[allow(dead_code)]
pub(crate) fn without_last_painted_line(buffer: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(buffer);
    let drawable: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
    let mut start = 0;
    let mut last_start = 0;
    while start < drawable.len() {
        last_start = start;
        start = wrap_end(&drawable, start);
    }
    drawable
        .get(..last_start)
        .unwrap_or_default()
        .iter()
        .collect::<String>()
        .into_bytes()
}

/// A [`Screen`] shared between the thread reading input and the thread that
/// repaints the body after a delay. `Screen.body` is the one place a
/// composer's contents live: the reading thread writes every update to it
/// through [`Output::set_body`] the moment the update happens, so whichever
/// thread paints next -- [`Output::line`] drawing an accepted line, or the
/// delayed repaint -- always paints what the composer currently holds,
/// never a value carried separately and applied out of order.
pub(crate) struct Output {
    screen: Mutex<Screen>,
}

impl Output {
    pub(crate) fn new(style: Option<ComposerStyle>) -> Self {
        Output {
            screen: Mutex::new(Screen {
                frame: Vec::new(),
                style,
                body: String::new(),
                pasted_len: 0,
                frozen: false,
                overlay: None,
                extra_rows: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Screen> {
        self.screen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes one output line above the composer and repaints the region
    /// below it, so output and composer never overwrite each other.
    pub(crate) fn line(&self, text: &str) -> io::Result<()> {
        let mut screen = self.lock();
        screen.erase()?;
        if screen.style.is_some() {
            write!(screen.frame, "{text}\r\n")?;
        } else {
            writeln!(screen.frame, "{text}")?;
        }
        screen.paint()?;
        screen.commit()
    }

    /// Draws `overlay` in the composer region's place, or restores the
    /// composer when `None`. The body keeps updating underneath either way.
    // Only `submit_child` draws an overlay; see [`OverlayComposer`].
    #[allow(dead_code)]
    pub(crate) fn set_overlay(&self, overlay: Option<Overlay>) -> io::Result<()> {
        let mut screen = self.lock();
        if screen.style.is_none() {
            return Ok(());
        }
        screen.erase()?;
        screen.overlay = overlay;
        screen.paint()?;
        screen.commit()
    }

    /// Records `body` as the composer's contents, without painting it. The
    /// caller applies this the moment its own local buffer changes, so
    /// `Screen.body` is never behind: a caller that goes on to paint in the
    /// same step (see [`Output::line`]) paints what it just set, not a value
    /// still in flight on the delayed-repaint channel.
    pub(crate) fn set_body(&self, body: &str) {
        self.set_body_pasted_through(body, body.len());
    }

    /// [`Output::set_body`], where only the first `pasted_len` bytes of
    /// `body` arrived in a paste: a folding composer, like Claude Code's
    /// `[Pasted text #N]`, folds that part alone and paints what was typed
    /// after it as typed.
    pub(crate) fn set_body_pasted_through(&self, body: &str, pasted_len: usize) {
        let mut screen = self.lock();
        if screen.frozen {
            return;
        }
        screen.body.clear();
        screen.body.push_str(body);
        screen.pasted_len = pasted_len;
    }

    /// Keeps the composer showing what it holds now, whatever is read later.
    // Only `submit_child` freezes its composer; the other fixtures sharing
    // this file never do.
    #[allow(dead_code)]
    pub(crate) fn freeze_body(&self) {
        self.lock().frozen = true;
    }

    /// Repaints the region with whatever body currently stands.
    pub(crate) fn repaint(&self) -> io::Result<()> {
        let mut screen = self.lock();
        if screen.style.is_none() {
            return Ok(());
        }
        screen.erase()?;
        screen.paint()?;
        screen.commit()
    }
}

/// When a [`spawn_reflector`] thread paints what was read; see
/// [`ComposerStyle::from_args`].
pub(crate) struct Reflect {
    delay: Duration,
    paused_while: Option<std::path::PathBuf>,
}

/// Starts the thread that repaints [`Output`]'s current body after the
/// reflection delay following a signal that it changed, waiting out a pause
/// first when one is set. The deadline is set by the first signal after a
/// quiet moment; what gets painted is whatever body `Output` holds when that
/// deadline arrives (already applied there by [`Output::set_body`]), not a
/// value carried by the signal itself, so the updates that led there are not
/// replayed one by one.
pub(crate) fn spawn_reflector(output: Arc<Output>, reflect: Reflect) -> Sender<()> {
    let Reflect {
        delay,
        paused_while,
    } = reflect;
    let (sender, receiver) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut pending: Option<Instant> = None;
        loop {
            let received = match pending {
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(remaining) if !remaining.is_zero() => receiver.recv_timeout(remaining),
                    _ => Err(RecvTimeoutError::Timeout),
                },
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(()) => {
                    if pending.is_none() {
                        let deadline = Instant::now().checked_add(delay);
                        pending = Some(deadline.unwrap_or_else(Instant::now));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if pending.take().is_some() {
                        let mut was_paused = false;
                        while paused_while.as_ref().is_some_and(|path| path.exists()) {
                            was_paused = true;
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        if was_paused {
                            std::thread::sleep(delay);
                        }
                        if output.repaint().is_err() {
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    sender
}

/// Where byte scanning stands inside an escape sequence: composer-draw mode
/// drops sequences such as a bracketed paste's markers instead of taking them
/// for body text.
pub(crate) enum Scan {
    Text,
    Escape,
    Sequence,
}

/// Feeds one byte into the composer buffer, returning the bytes to echo when
/// that byte submits what was typed.
///
/// The fixtures that keep a state machine of their own drive their own buffer
/// instead and never call this.
pub(crate) fn accept_byte(buffer: &mut Vec<u8>, scan: &mut Scan, byte: u8) -> Option<Vec<u8>> {
    match scan {
        Scan::Escape => {
            *scan = if byte == b'[' {
                Scan::Sequence
            } else {
                Scan::Text
            };
            None
        }
        Scan::Sequence => {
            if (0x40..=0x7e).contains(&byte) {
                *scan = Scan::Text;
            }
            None
        }
        Scan::Text => match byte {
            0x1b => {
                *scan = Scan::Escape;
                None
            }
            b'\r' | b'\n' => (!buffer.is_empty()).then(|| std::mem::take(buffer)),
            // Ctrl-U (the clear sequence a definition carries) and Ctrl-C both
            // leave a real CLI's composer empty.
            0x15 | 0x03 => {
                buffer.clear();
                None
            }
            // DEL and BS erase the last character, as the erase key that takes
            // a delivery's probe back out does in a real CLI's composer.
            0x7f | 0x08 => {
                erase_last_char(buffer);
                None
            }
            _ => {
                buffer.push(byte);
                None
            }
        },
    }
}

/// Drops the last UTF-8 character of `buffer`: its continuation bytes, then
/// the byte that starts it.
fn erase_last_char(buffer: &mut Vec<u8>) {
    while let Some(byte) = buffer.pop() {
        if byte & 0xC0 != 0x80 {
            break;
        }
    }
}
