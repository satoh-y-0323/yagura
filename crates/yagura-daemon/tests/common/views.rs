//! Fixtures for tests that watch what the daemon's view layer sends: a window
//! driven by `term_script`, a read of the model's screen to compare the
//! applied grid with, and the wait that holds until they agree.

use std::time::Duration;

use tokio::time::Instant;
use yagura_daemon::detector::VtScreen;
use yagura_test_support::frame::Grid;
use yagura_test_support::view::ViewClient;

use super::Harness;

/// What `term_script` prints once it reads stdin.
pub const SCRIPT_READY: &[u8] = b"SCRIPT-READY";

/// The longest a test waits for the daemon to catch up with an event.
pub const CATCH_UP: Duration = Duration::from_secs(20);

/// The argv of a `term_script` window in script mode.
pub fn script_argv() -> Vec<String> {
    vec![
        yagura_test_support::process::term_script_path()
            .to_str()
            .expect("term_script path must be valid UTF-8")
            .to_string(),
    ]
}

/// `bytes` as a `term_script` command: `<` + lowercase hex + `>`.
pub fn script_command(bytes: &[u8]) -> Vec<u8> {
    let mut command = vec![b'<'];
    command.extend(super::hex_of(bytes).into_bytes());
    command.push(b'>');
    command
}

/// Opens a `term_script` window and waits until it is reading commands.
pub async fn open_script_window(h: &Harness) -> String {
    let window_id = h.open(script_argv(), None).await;
    super::wait_until_model_shows(h, &window_id, SCRIPT_READY, CATCH_UP).await;
    window_id
}

/// Makes the window's child write `bytes` to its terminal.
pub async fn script_write(h: &Harness, window_id: &str, bytes: &[u8]) {
    h.write_input(window_id, &script_command(bytes)).await;
}

/// Makes the child write `bytes`, then waits until the window's model shows
/// `marker`, which `bytes` must end with.
pub async fn script_write_and_wait(h: &Harness, window_id: &str, bytes: &[u8], marker: &str) {
    script_write(h, window_id, bytes).await;
    super::wait_until_model_shows(h, window_id, marker.as_bytes(), CATCH_UP).await;
}

/// The window's model as a reader of the daemon sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelScreen {
    pub cols: u16,
    pub rows: Vec<String>,
    pub cursor_row: u16,
    pub cursor_hidden: bool,
    pub alt_screen: bool,
    pub mouse_reporting: bool,
}

/// Reads the model's visible screen row by row.
pub async fn model_screen(h: &Harness, window_id: &str) -> ModelScreen {
    let session = h
        .registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .clone();
    session
        .read_screen(model_screen_of)
        .await
        .expect("the window's model is readable")
}

fn model_screen_of(screen: &VtScreen) -> ModelScreen {
    let (cols, rows) = screen.size();
    let modes = screen.input_modes();
    ModelScreen {
        cols,
        rows: screen
            .bottom_lines(usize::from(rows))
            .into_iter()
            .map(|line| line.trim_end_matches(' ').to_string())
            .collect(),
        cursor_row: screen.cursor_row(),
        cursor_hidden: screen.cursor_hidden(),
        alt_screen: modes.alt_screen,
        mouse_reporting: modes.mouse != yagura_daemon::input_encode::MouseTracking::Off,
    }
}

/// Why `grid` is not the picture of `model`, or `None` when it is. Compares
/// the size, every row's text, the cursor's row and visibility, and the two
/// mode flags the frame carries. Cell attributes are the subject of tests that
/// know what the child painted.
pub fn grid_differs_from_model(grid: &Grid, model: &ModelScreen) -> Option<String> {
    if grid.cols != model.cols || usize::from(grid.rows) != model.rows.len() {
        return Some(format!(
            "size: grid {}x{}, model {}x{}",
            grid.cols,
            grid.rows,
            model.cols,
            model.rows.len()
        ));
    }
    for (row, expected) in model.rows.iter().enumerate() {
        let shown = grid.row_text(u16::try_from(row).unwrap());
        if &shown != expected {
            return Some(format!("row {row}: grid {shown:?}, model {expected:?}"));
        }
    }
    if grid.view.display_offset == 0 {
        if grid.cursor.row != model.cursor_row {
            return Some(format!(
                "cursor row: grid {}, model {}",
                grid.cursor.row, model.cursor_row
            ));
        }
        if grid.cursor.visible == model.cursor_hidden {
            return Some(format!(
                "cursor visibility: grid visible={}, model hidden={}",
                grid.cursor.visible, model.cursor_hidden
            ));
        }
    }
    if grid.view.alt_screen != model.alt_screen {
        return Some(format!(
            "alt screen: grid {}, model {}",
            grid.view.alt_screen, model.alt_screen
        ));
    }
    if grid.view.mouse_reporting != model.mouse_reporting {
        return Some(format!(
            "mouse reporting: grid {}, model {}",
            grid.view.mouse_reporting, model.mouse_reporting
        ));
    }
    None
}

/// Applies (and acknowledges) frames until the grid is the picture of the
/// model as it reads at that moment, failing with the last difference when it
/// does not within `timeout`. The model is read again after every frame, so a
/// child that is still writing is followed, not raced.
pub async fn assert_converges_to_model(
    view: &mut ViewClient,
    h: &Harness,
    window_id: &str,
    what: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let model = model_screen(h, window_id).await;
        let Some(difference) = grid_differs_from_model(&view.grid, &model) else {
            return;
        };
        assert!(
            Instant::now() < deadline,
            "{what}: the grid never became the picture of the model; last difference: {difference}\n\
             grid:\n{}\nmodel: {model:?}",
            view.grid.dump()
        );
        if let Some(parts) = view
            .try_next_complete_frame(Duration::from_millis(250))
            .await
            && let Some(last) = parts.last()
        {
            view.ack(last.seq).await;
        }
    }
}

/// `n` numbered lines, `LINE-0001\r\n`, `LINE-0002\r\n`, ... counting from `from`.
pub fn numbered_lines(from: usize, n: usize) -> Vec<u8> {
    (from..from + n)
        .flat_map(|number| format!("LINE-{number:04}\r\n").into_bytes())
        .collect()
}

/// A burst of 320 lines of 511 `0` ended by carriage returns, then `tag` and a
/// carriage return -- written to a window whose child reads in cooked mode.
/// A bare line feed costs a bundled ConPTY about 11 ms each there, which would
/// turn the burst into a slow trickle.
pub fn cr_burst_payload(tag: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    for _ in 0..320 {
        payload.extend(std::iter::repeat_n(b'0', 511));
        payload.push(b'\r');
    }
    payload.extend_from_slice(tag.as_bytes());
    payload.push(b'\r');
    payload
}

/// Opens a script window, attaches a view of `size` to it and makes the child
/// print `lines` numbered lines ended by `HIST-FILL`, so the window has
/// history to scroll back into. Returns the window and the view showing the
/// filled screen at the bottom.
pub async fn open_view_with_history(
    h: &Harness,
    size: yagura_protocol::TerminalSize,
    lines: usize,
) -> (String, ViewClient) {
    let window_id = open_script_window(h).await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(size)).await;
    let mut bytes = b"\x1b[H\x1b[2J".to_vec();
    bytes.extend(numbered_lines(1, lines));
    bytes.extend_from_slice(b"HIST-FILL");
    script_write_and_wait(h, &window_id, &bytes, "HIST-FILL").await;
    view.pump_until("the filled screen", CATCH_UP, |grid| {
        grid.contains_text("HIST-FILL")
    })
    .await;
    (window_id, view)
}
