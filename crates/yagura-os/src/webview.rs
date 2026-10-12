//! Settings the GUI applies to its own WebView2 webview (Windows only).

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Controller, ICoreWebView2Settings3,
};
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
