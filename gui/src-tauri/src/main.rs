//! Binary entry point for the yagura GUI.
//!
//! Everything beyond assembling the Tauri application lives in the
//! `yagura_gui` library.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use anyhow::Context;
use tauri::Manager;

use yagura_gui::connection_pool::ConnectionPool;
use yagura_gui::instance::{StartupContext, compose_window_title};
use yagura_gui::{commands, config_paths, daemon_supervisor, window_list_poll};

fn main() -> anyhow::Result<()> {
    // The pool is the only handle the commands get: a single `IpcClient` would
    // be one connection, and one connection serves at most one window.
    let pool = Arc::new(ConnectionPool::new());
    let poll_pool = Arc::clone(&pool);
    let supervisor_pool = Arc::clone(&pool);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(Arc::clone(pool.frames()))
        .manage(pool)
        .setup(move |app| {
            // The only place this process reads `YAGURA_INSTANCE` /
            // `YAGURA_STORE_DB` / the configuration base directory:
            // every other command and background
            // task reaches the result through the managed `StartupContext`
            // below instead of reading the environment a second time.
            let config_base = yagura_os::default_config_base_dir();
            let startup = StartupContext::resolve_from(
                std::env::var_os("YAGURA_INSTANCE").as_deref(),
                std::env::var_os("YAGURA_STORE_DB").as_deref(),
                config_base.clone(),
            );

            // Skipping these on a resolution failure is an optimization
            // against pointless writes and background work for an instance
            // this process must not act as, not the guarantee that a
            // failure stays inert -- that guarantee is `ensure_default_*_in`
            // requiring an `InstanceNamespace` it cannot construct on its
            // own.
            if let (Some(base), Ok(namespace)) =
                (config_base.as_deref(), startup.resolution().as_ref())
            {
                // Seeded before any daemon connect/spawn below, so a freshly
                // installed config root already has a playbook when the
                // daemon reads it.
                config_paths::ensure_default_playbook_in(base, namespace);
                tauri::async_runtime::spawn(window_list_poll::run_window_list_poll(
                    poll_pool,
                    app.handle().clone(),
                ));
                // A non-default instance gets a distinguishable title so the
                // taskbar and Alt+Tab tell the GUIs apart. The
                // stem is read at run time rather than hardcoded, and a
                // `title()` failure leaves the title as `tauri.conf.json`
                // set it (degrade and continue).
                if let Some(id) = namespace.id()
                    && let Some(window) = app.get_webview_window("main")
                    && let Ok(stem) = window.title()
                {
                    let _ = window.set_title(&compose_window_title(&stem, Some(id)));
                }
            }

            // The GUI owns every shortcut it handles; the browser's own keys
            // (print, find, ...) would open browser features inside the app.
            // A failure only leaves those keys working, so startup goes on.
            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                let applied = window.with_webview(|webview| {
                    if let Err(error) =
                        yagura_os::webview::turn_off_browser_accelerator_keys(&webview.controller())
                    {
                        eprintln!("{error}");
                    }
                });
                if let Err(error) = applied {
                    eprintln!(
                        "failed to reach the main webview to turn off its browser keys: {error}"
                    );
                }
            }

            daemon_supervisor::install(app.handle(), supervisor_pool, &startup);
            app.manage(startup);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::attach_window,
            commands::list_windows,
            commands::list_detections,
            commands::list_profiles,
            commands::list_restore_candidates,
            commands::get_daemon_health,
            commands::update_window_layout,
            commands::open_window,
            commands::close_window,
            commands::interrupt_window,
            commands::decide_approval,
            commands::sync_approvals,
            commands::rename_window,
            commands::send_chat,
            commands::broadcast_chat,
            commands::read_chat,
            commands::list_chat_rooms,
            commands::list_plans,
            commands::get_plan,
            commands::delete_plan,
            commands::set_task_assignment,
            commands::clear_task_assignment,
            commands::set_plan_owner,
            commands::approve_plan,
            commands::mark_plan_seen,
            commands::list_roles,
            commands::save_roles,
            commands::list_model_choices,
            commands::save_model_aliases,
            commands::set_profile_enabled,
            commands::get_profile_detail,
            commands::set_profile_command,
            commands::get_approval_settings,
            commands::set_auto_approve,
            commands::set_plan_approval_required,
            commands::list_mcp_registrations,
            commands::run_mcp_registration,
            commands::send_window_input,
            commands::next_frames,
            commands::set_window_palette,
            commands::set_window_visible,
            commands::copy_window_selection,
            commands::clear_window_selection,
            commands::resize,
            commands::load_keymap,
            commands::keymap_file_path,
            commands::save_keymap,
            commands::load_themes,
            commands::reconnect_daemon,
            commands::get_recovery_status
        ])
        .run(tauri::generate_context!())
        .context("failed to run the yagura GUI")
}
