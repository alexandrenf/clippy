#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod commands;
mod db;
mod glass;
#[cfg(target_os = "macos")]
mod macos;
mod panel;
mod tray;

use std::{path::Path, sync::Mutex, time::Duration};
use tauri::Manager;

fn migrate_legacy_database(data_dir: &Path) -> rusqlite::Result<()> {
    let target = data_dir.join("clippy.db");
    if target.exists() {
        return Ok(());
    }

    let Some(app_data_root) = data_dir.parent() else {
        return Ok(());
    };
    let source = app_data_root.join("app.cooper.desktop").join("cooper.db");
    if !source.exists() {
        return Ok(());
    }

    let source_conn =
        rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut target_conn = rusqlite::Connection::open(&target)?;
    let backup = rusqlite::backup::Backup::new(&source_conn, &mut target_conn)?;
    backup.run_to_completion(64, Duration::from_millis(10), None)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            panel::show(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::add_entry,
            commands::set_items_done,
            commands::update_item,
            commands::delete_items,
            commands::clear_completed,
            commands::set_active_section,
            commands::create_section,
            commands::rename_section,
            commands::delete_section,
            commands::set_theme,
            commands::copy_text,
            commands::export_markdown,
            commands::capture_now,
            commands::hide_panel,
            commands::open_editor,
            commands::accessibility_status,
            commands::request_accessibility_permission,
            commands::open_accessibility_settings,
        ])
        .setup(|app| {
            let launch_hidden = std::env::args_os().any(|arg| arg == "--hidden");
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            if let Err(error) = migrate_legacy_database(&data_dir) {
                let _ = std::fs::remove_file(data_dir.join("clippy.db"));
                eprintln!("clippy: could not import the Cooper database: {error}");
            }
            let conn = db::init(&data_dir.join("clippy.db"))?;
            let glass_on = db::get_setting(&conn, "theme").as_deref() == Some("glass");
            app.manage(db::Db(Mutex::new(conn)));

            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            tray::create(app.handle())?;
            capture::start_capture_worker(app.handle().clone());
            capture::register_fallback_shortcuts(app.handle());
            capture::start_double_shift_listener(app.handle().clone());

            if let Some(win) = app.get_webview_window("main") {
                panel::position(&win);
                if glass_on {
                    glass::apply(&win, true);
                }
                if !launch_hidden {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the panel hides it; the app lives in the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Clippy");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_the_legacy_cooper_database() {
        let root = std::env::temp_dir().join(format!(
            "clippy-migration-test-{}-{}",
            std::process::id(),
            db::now_ms()
        ));
        let legacy_dir = root.join("app.cooper.desktop");
        let clippy_dir = root.join("app.clippy.desktop");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::create_dir_all(&clippy_dir).unwrap();

        let legacy = db::init(&legacy_dir.join("cooper.db")).unwrap();
        legacy
            .execute(
                "INSERT INTO sections(name, created_at) VALUES('Imported', 1)",
                [],
            )
            .unwrap();
        drop(legacy);

        migrate_legacy_database(&clippy_dir).unwrap();
        let imported = db::init(&clippy_dir.join("clippy.db")).unwrap();
        let name: String = imported
            .query_row("SELECT name FROM sections", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "Imported");
        drop(imported);

        std::fs::remove_dir_all(root).unwrap();
    }
}
