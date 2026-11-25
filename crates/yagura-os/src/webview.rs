//! Settings the GUI applies to its own WebView2 webview and the window that
//! hosts it (Windows only).

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Controller, ICoreWebView2Settings3,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EndMenu, MF_SYSMENU, PostMessageW, RegisterWindowMessageW, SC_KEYMENU, WM_SYSCOMMAND,
    WM_UNINITMENUPOPUP,
};
use windows::core::w;
use windows_core::Interface;

/// Failure to change a setting of the webview.
#[derive(Debug, thiserror::Error)]
#[error("failed to turn off the webview's browser accelerator keys: {0}")]
pub struct WebviewSettingError(#[from] windows_core::Error);

/// Turns off the browser accelerator keys of the webview `controller` hosts:
/// print, find, zoom, reload, history navigation and the other keys a browser
/// handles itself. Keys that edit or move through text keep working.
///
/// Call it on the webview's own UI thread, as Tauri's `with_webview` does.
///
/// # Errors
///
/// Returns [`WebviewSettingError`] when the webview has already been closed,
/// when the installed WebView2 runtime predates the setting
/// (`ICoreWebView2Settings3`), or when WebView2 refuses the change.
pub fn turn_off_browser_accelerator_keys(
    controller: &ICoreWebView2Controller,
) -> Result<(), WebviewSettingError> {
    // SAFETY: each call goes through a COM interface pointer that its Rust
    // wrapper keeps referenced until the wrapper drops, and no pointer
    // outlives this block; failures come back as HRESULT errors.
    unsafe {
        controller
            .CoreWebView2()?
            .Settings()?
            .cast::<ICoreWebView2Settings3>()?
            .SetAreBrowserAcceleratorKeysEnabled(false)?;
    }
    Ok(())
}

/// Failure to keep a lone Alt out of the host window's menu mode.
#[derive(Debug, thiserror::Error)]
#[error("failed to keep a lone Alt from opening the window's menu mode")]
pub struct MenuModeError(#[source] windows::core::Error);

/// Id of this module's subclass of the host window, unique among its subclasses.
const LONE_ALT_SUBCLASS_ID: usize = 0x7961_6c74;

/// Keeps the window `hwnd` out of its keyboard menu mode, where its thread
/// runs the system's menu loop and answers its webview nothing until the next
/// key or click. The window has no menu bar, so the mode only freezes it: a
/// lone Alt or F10 no longer enters it, and closing the window menu (Alt+Space)
/// leaves it instead of returning to it. Alt with another key is left to the
/// system.
///
/// Call it on the thread that owns `hwnd`, as Tauri's setup hook runs.
///
/// # Errors
///
/// Returns [`MenuModeError`] when no message can be registered to end the menu
/// loop with, or when the window cannot be subclassed (it is gone, or belongs
/// to another thread).
pub fn keep_lone_alt_out_of_menu_mode(hwnd: isize) -> Result<(), MenuModeError> {
    // A registered message cannot collide with the ones tao, wry or WebView2
    // post to the same window.
    // SAFETY: the argument is a static NUL-terminated UTF-16 string.
    let end_menu_loop = unsafe { RegisterWindowMessageW(w!("yagura.end-menu-loop")) };
    if end_menu_loop == 0 {
        return Err(MenuModeError(windows::core::Error::from_win32()));
    }
    // SAFETY: SetWindowSubclass only records the procedure and its data for
    // `hwnd`, and fails for a window that is gone or owned by another thread.
    let subclassed = unsafe {
        SetWindowSubclass(
            HWND(hwnd),
            Some(lone_alt_proc),
            LONE_ALT_SUBCLASS_ID,
            end_menu_loop as usize,
        )
    };
    if subclassed.as_bool() {
        Ok(())
    } else {
        Err(MenuModeError(windows::core::Error::from_win32()))
    }
}

/// WM_SYSCOMMAND carries the key pressed with Alt in `lparam`, 0 when Alt (or
/// F10) came alone; the low four bits of `wparam` are the system's own.
/// `end_menu_loop` is the message registered for ending the menu loop.
unsafe extern "system" fn lone_alt_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    end_menu_loop: usize,
) -> LRESULT {
    if msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_KEYMENU as usize && lparam.0 == 0 {
        return LRESULT(0);
    }
    // Esc on the window menu returns to the menu mode rather than leaving it,
    // and the return happens after this message, so the end is posted; the
    // high word of `lparam` flags the window menu.
    if msg == WM_UNINITMENUPOPUP && ((lparam.0 as u32) >> 16) & MF_SYSMENU.0 != 0 {
        // SAFETY: posts a message with no payload to the window being processed.
        let posted = unsafe { PostMessageW(hwnd, end_menu_loop as u32, WPARAM(0), LPARAM(0)) };
        if let Err(error) = posted {
            tracing::warn!("failed to post the end of the window menu's loop: {error}");
        }
    }
    if msg as usize == end_menu_loop {
        // SAFETY: EndMenu only asks this thread's active menu loop, if any, to end.
        let _ = unsafe { EndMenu() };
        return LRESULT(0);
    }
    // SAFETY: hands the message on, unchanged, to the next procedure of the
    // window that is processing it.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
