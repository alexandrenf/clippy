use tauri::{AppHandle, Emitter, Manager, State};

use crate::{capture, db, glass, panel};

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn refresh(app: &AppHandle) {
    let _ = app.emit("refresh", ());
}

fn item_content(content: &str) -> CmdResult<&str> {
    let content = content.trim();
    if content.is_empty() {
        return Err("A note cannot be empty".into());
    }
    if content.chars().count() > db::MAX_ITEM_CHARS {
        return Err(format!(
            "A note cannot be longer than {} characters",
            db::MAX_ITEM_CHARS
        ));
    }
    Ok(content)
}

fn section_name(name: &str) -> CmdResult<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A section name cannot be empty".into());
    }
    if name.chars().count() > db::MAX_SECTION_CHARS {
        return Err(format!(
            "A section name cannot be longer than {} characters",
            db::MAX_SECTION_CHARS
        ));
    }
    if name.contains('\n') || name.contains('\r') {
        return Err("A section name must fit on one line".into());
    }
    Ok(name)
}

fn item_ids(mut ids: Vec<i64>) -> CmdResult<Vec<i64>> {
    if ids.is_empty() {
        return Ok(ids);
    }
    if ids.len() > db::MAX_BATCH_ITEMS {
        return Err("Too many items in one operation".into());
    }
    if ids.iter().any(|id| *id <= 0) {
        return Err("Invalid item id".into());
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

#[tauri::command]
pub fn get_state(db: State<db::Db>) -> CmdResult<db::AppState> {
    let conn = db.0.lock().map_err(err)?;
    db::get_state(&conn).map_err(err)
}

/// Add a note/prompt from the input. A line of the form "# Name" creates (or
/// switches to) a section instead of adding an item.
#[tauri::command]
pub fn add_entry(app: AppHandle, db: State<db::Db>, content: String) -> CmdResult<()> {
    let text = item_content(&content)?;
    let conn = db.0.lock().map_err(err)?;
    if let Some(name) = text.strip_prefix("# ") {
        let name = section_name(name)?;
        let id = db::find_or_create_section(&conn, name).map_err(err)?;
        db::set_active_section(&conn, Some(id)).map_err(err)?;
    } else {
        let active = db::get_active_section(&conn);
        db::insert_item(&conn, text, active).map_err(err)?;
    }
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn set_items_done(
    app: AppHandle,
    db: State<db::Db>,
    ids: Vec<i64>,
    done: bool,
) -> CmdResult<()> {
    let ids = item_ids(ids)?;
    if ids.is_empty() {
        return Ok(());
    }
    let mut conn = db.0.lock().map_err(err)?;
    let tx = conn.transaction().map_err(err)?;
    let now = db::now_ms();
    for id in ids {
        tx.execute(
            "UPDATE items SET done = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, done, now],
        )
        .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn update_item(app: AppHandle, db: State<db::Db>, id: i64, content: String) -> CmdResult<()> {
    let content = item_content(&content)?;
    let conn = db.0.lock().map_err(err)?;
    let updated = conn
        .execute(
            "UPDATE items SET content = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, content, db::now_ms()],
        )
        .map_err(err)?;
    if updated == 0 {
        return Err("That note no longer exists".into());
    }
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn delete_items(app: AppHandle, db: State<db::Db>, ids: Vec<i64>) -> CmdResult<()> {
    let ids = item_ids(ids)?;
    if ids.is_empty() {
        return Ok(());
    }
    let mut conn = db.0.lock().map_err(err)?;
    let tx = conn.transaction().map_err(err)?;
    for id in ids {
        tx.execute("DELETE FROM items WHERE id = ?1", rusqlite::params![id])
            .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn clear_completed(app: AppHandle, db: State<db::Db>) -> CmdResult<()> {
    let conn = db.0.lock().map_err(err)?;
    conn.execute("DELETE FROM items WHERE done = 1", [])
        .map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn set_active_section(app: AppHandle, db: State<db::Db>, id: Option<i64>) -> CmdResult<()> {
    let conn = db.0.lock().map_err(err)?;
    if let Some(id) = id {
        if !db::section_exists(&conn, id).map_err(err)? {
            return Err("That section no longer exists".into());
        }
    }
    db::set_active_section(&conn, id).map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn create_section(app: AppHandle, db: State<db::Db>, name: String) -> CmdResult<i64> {
    let name = section_name(&name)?;
    let conn = db.0.lock().map_err(err)?;
    let id = db::find_or_create_section(&conn, name).map_err(err)?;
    db::set_active_section(&conn, Some(id)).map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(id)
}

#[tauri::command]
pub fn rename_section(app: AppHandle, db: State<db::Db>, id: i64, name: String) -> CmdResult<()> {
    let name = section_name(&name)?;
    let conn = db.0.lock().map_err(err)?;
    if db::section_name_exists(&conn, name, Some(id)).map_err(err)? {
        return Err("A section with that name already exists".into());
    }
    conn.execute(
        "UPDATE sections SET name = ?2 WHERE id = ?1",
        rusqlite::params![id, name],
    )
    .map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(())
}

/// Delete a section; its items fall back to the unfiled group.
#[tauri::command]
pub fn delete_section(app: AppHandle, db: State<db::Db>, id: i64) -> CmdResult<()> {
    let conn = db.0.lock().map_err(err)?;
    conn.execute(
        "UPDATE items SET section_id = NULL WHERE section_id = ?1",
        rusqlite::params![id],
    )
    .map_err(err)?;
    conn.execute("DELETE FROM sections WHERE id = ?1", rusqlite::params![id])
        .map_err(err)?;
    if db::get_active_section(&conn) == Some(id) {
        db::set_active_section(&conn, None).map_err(err)?;
    }
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn set_theme(app: AppHandle, db: State<db::Db>, theme: String) -> CmdResult<()> {
    if !matches!(theme.as_str(), "system" | "light" | "dark" | "glass") {
        return Err("Unknown appearance".into());
    }
    {
        let conn = db.0.lock().map_err(err)?;
        db::set_setting(&conn, "theme", &theme).map_err(err)?;
    }
    glass::apply_all(&app, theme == "glass");
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn copy_text(text: String) -> CmdResult<()> {
    let mut clip = arboard::Clipboard::new().map_err(err)?;
    clip.set_text(text).map_err(err)
}

/// Write all sections/items to a Markdown file in Documents and return its path.
#[tauri::command]
pub fn export_markdown(app: AppHandle, db: State<db::Db>) -> CmdResult<String> {
    use tauri::path::BaseDirectory;
    let conn = db.0.lock().map_err(err)?;
    let state = db::get_state(&conn).map_err(err)?;
    drop(conn);

    let mut md = String::from("# Cooper export\n\n");
    let write_items = |md: &mut String, section_id: Option<i64>| {
        for item in state.items.iter().filter(|i| i.section_id == section_id) {
            let mark = if item.done { "x" } else { " " };
            let content = item.content.replace('\n', "\n  ");
            md.push_str(&format!("- [{mark}] {content}\n"));
        }
    };
    if state.items.iter().any(|i| i.section_id.is_none()) {
        write_items(&mut md, None);
        md.push('\n');
    }
    for section in &state.sections {
        md.push_str(&format!("## {}\n\n", section.name));
        write_items(&mut md, Some(section.id));
        md.push('\n');
    }

    let dir = app
        .path()
        .resolve("", BaseDirectory::Document)
        .map_err(err)?;
    for suffix in 0..10_000 {
        let name = if suffix == 0 {
            "cooper-export.md".to_string()
        } else {
            format!("cooper-export-{suffix}.md")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(md.as_bytes()).map_err(err)?;
                return Ok(path.to_string_lossy().into_owned());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(err(e)),
        }
    }
    Err("Too many Cooper exports in Documents".into())
}

#[tauri::command]
pub fn capture_now(app: AppHandle) {
    capture::capture_clipboard(&app);
}

#[tauri::command]
pub fn hide_panel(app: AppHandle) {
    panel::hide(&app);
}

#[tauri::command]
pub fn open_editor(app: AppHandle, db: State<db::Db>, id: i64) -> CmdResult<()> {
    let label = format!("editor-{id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.set_focus();
        return Ok(());
    }
    let win = tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App(format!("index.html#/edit/{id}").into()),
    )
    .title("Edit — Cooper")
    .inner_size(460.0, 360.0)
    .min_inner_size(320.0, 240.0)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .center()
    .build()
    .map_err(err)?;

    let glass_on = {
        let conn = db.0.lock().map_err(err)?;
        db::get_setting(&conn, "theme").as_deref() == Some("glass")
    };
    if glass_on {
        glass::apply(&win, true);
    }
    Ok(())
}

#[tauri::command]
pub fn open_accessibility_settings() -> CmdResult<()> {
    #[cfg(target_os = "macos")]
    {
        let pane = if crate::macos::accessibility_trusted() {
            "Privacy_ListenEvent"
        } else {
            "Privacy_Accessibility"
        };
        std::process::Command::new("open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{pane}"
            ))
            .spawn()
            .map_err(err)?;
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    Err("Accessibility Settings is only available on macOS".into())
}

#[tauri::command]
pub fn accessibility_status() -> bool {
    #[cfg(target_os = "macos")]
    {
        return crate::macos::capture_permissions_granted();
    }

    #[cfg(not(target_os = "macos"))]
    true
}

#[tauri::command]
pub fn request_accessibility_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        return crate::macos::request_accessibility_permission();
    }

    #[cfg(not(target_os = "macos"))]
    true
}
