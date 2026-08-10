use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{capture, db, glass, panel, sync};

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn refresh(app: &AppHandle) {
    sync::wake(app);
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
        return Err("A list name cannot be empty".into());
    }
    if name.chars().count() > db::MAX_SECTION_CHARS {
        return Err(format!(
            "A list name cannot be longer than {} characters",
            db::MAX_SECTION_CHARS
        ));
    }
    if name.contains('\n') || name.contains('\r') {
        return Err("A list name must fit on one line".into());
    }
    Ok(name)
}

fn entry_list_name(content: &str) -> Option<&str> {
    content
        .strip_prefix("## ")
        .or_else(|| content.strip_prefix("# "))
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

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCandidate {
    pub path: String,
    pub name: String,
    pub media_type: String,
    pub size: i64,
    pub preview: Option<String>,
    pub temporary: bool,
}

fn attachment_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "ts" | "tsx" => "text/typescript",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

fn attachment_preview(path: &Path, media_type: &str, size: u64) -> Option<String> {
    const MAX_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;
    if !media_type.starts_with("image/") || size > MAX_PREVIEW_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some(format!("data:{media_type};base64,{}", BASE64.encode(bytes)))
}

fn inspect_attachment_path(raw_path: &str) -> CmdResult<AttachmentCandidate> {
    let path = PathBuf::from(raw_path)
        .canonicalize()
        .map_err(|error| format!("Couldn’t read attachment: {error}"))?;
    let metadata = fs::metadata(&path).map_err(err)?;
    if !metadata.is_file() {
        return Err("Folders cannot be attached".into());
    }
    if metadata.len() > db::MAX_ATTACHMENT_BYTES {
        return Err("Attachments must be 250 MB or smaller".into());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "That attachment has no usable file name".to_string())?
        .to_string();
    let media_type = attachment_media_type(&path).to_string();
    let preview = attachment_preview(&path, &media_type, metadata.len());
    Ok(AttachmentCandidate {
        path: path.to_string_lossy().into_owned(),
        name,
        media_type,
        size: metadata.len() as i64,
        preview,
        temporary: false,
    })
}

fn inspect_attachment_paths(paths: Vec<String>) -> CmdResult<Vec<AttachmentCandidate>> {
    if paths.len() > db::MAX_ATTACHMENTS_PER_ITEM {
        return Err(format!(
            "Attach up to {} files to one prompt",
            db::MAX_ATTACHMENTS_PER_ITEM
        ));
    }
    let mut seen = HashSet::new();
    let mut attachments = Vec::with_capacity(paths.len());
    for raw_path in paths {
        let attachment = inspect_attachment_path(&raw_path)?;
        if seen.insert(attachment.path.clone()) {
            attachments.push(attachment);
        }
    }
    Ok(attachments)
}

fn stored_attachment_path(
    attachments_dir: &Path,
    item_id: i64,
    index: usize,
    source: &Path,
) -> PathBuf {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .take(12)
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|extension| !extension.is_empty());
    let stem = format!("{item_id}-{}-{index}", db::now_ms());
    match extension {
        Some(extension) => attachments_dir.join(format!("{stem}.{extension}")),
        None => attachments_dir.join(stem),
    }
}

fn remove_stored_files(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "clippy: could not remove attachment {}: {error}",
                    path.display()
                );
            }
        }
    }
}

fn open_path(path: &Path) -> CmdResult<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };

    command.arg(path).spawn().map_err(err)?;
    Ok(())
}

static PASTE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn paste_drafts_dir(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app.path().app_data_dir().map_err(err)?.join("paste-drafts"))
}

fn is_paste_draft(app: &AppHandle, path: &Path) -> bool {
    let Ok(root) = paste_drafts_dir(app).and_then(|root| root.canonicalize().map_err(err)) else {
        return false;
    };
    path.canonicalize()
        .map(|path| path.starts_with(root))
        .unwrap_or(false)
}

pub fn clear_paste_drafts(app: &AppHandle) {
    let Ok(root) = paste_drafts_dir(app) else {
        return;
    };
    if let Err(error) = fs::remove_dir_all(&root) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("clippy: could not clear pasted-image drafts: {error}");
        }
    }
}

fn bundled_companion_executable() -> CmdResult<PathBuf> {
    let executable = std::env::current_exe().map_err(err)?;
    let companion = executable
        .parent()
        .ok_or_else(|| "Clippy’s install location is unavailable".to_string())?
        .join("clippy-mcp");
    if companion.is_file() {
        Ok(companion)
    } else {
        Err("This Clippy build does not include the agent companion".into())
    }
}

#[tauri::command]
pub async fn agent_companion_status() -> CmdResult<bool> {
    let companion = bundled_companion_executable()?;
    tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new(companion)
            .arg("doctor")
            .output()
            .map(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("codex_mcp: configured")
                    && String::from_utf8_lossy(&output.stdout)
                        .contains("companion_skill: installed")
            })
            .map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn install_agent_companion() -> CmdResult<()> {
    let companion = bundled_companion_executable()?;
    tauri::async_runtime::spawn_blocking(move || {
        let status = std::process::Command::new(companion)
            .arg("install-codex")
            .status()
            .map_err(err)?;
        if status.success() {
            Ok(())
        } else {
            Err("Codex could not enable the Clippy companion".into())
        }
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub fn paste_clipboard_image(app: AppHandle) -> CmdResult<AttachmentCandidate> {
    const MAX_PASTED_IMAGE_BYTES: usize = 64 * 1024 * 1024;

    let mut clipboard = arboard::Clipboard::new().map_err(err)?;
    let image = clipboard
        .get_image()
        .map_err(|_| "The clipboard does not contain an image".to_string())?;
    let expected = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "That image is too large to paste".to_string())?;
    if expected == 0 || expected > MAX_PASTED_IMAGE_BYTES || image.bytes.len() != expected {
        return Err("That image is too large to paste".into());
    }

    let drafts_dir = paste_drafts_dir(&app)?;
    fs::create_dir_all(&drafts_dir).map_err(err)?;
    let sequence = PASTE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = drafts_dir.join(format!("pasted-image-{}-{sequence}.png", db::now_ms()));
    image::save_buffer_with_format(
        &path,
        &image.bytes,
        image.width as u32,
        image.height as u32,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(err)?;

    let mut attachment = inspect_attachment_path(
        path.to_str()
            .ok_or_else(|| "The pasted image path is not usable".to_string())?,
    )?;
    attachment.name = "Pasted image.png".into();
    attachment.temporary = true;
    Ok(attachment)
}

#[tauri::command]
pub fn discard_pasted_image(app: AppHandle, path: String) -> CmdResult<()> {
    let path = PathBuf::from(path);
    if !is_paste_draft(&app, &path) {
        return Err("Only unsaved pasted images can be discarded".into());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(err(error)),
    }
}

#[tauri::command]
pub fn get_state(db: State<db::Db>) -> CmdResult<db::AppState> {
    let conn = db.0.lock().map_err(err)?;
    db::get_state(&conn).map_err(err)
}

#[tauri::command]
pub fn set_shortcuts(
    app: AppHandle,
    db: State<db::Db>,
    show_shortcut: String,
    capture_shortcut: String,
) -> CmdResult<()> {
    let show_shortcut = show_shortcut.trim();
    let capture_shortcut = capture_shortcut.trim();
    capture::validate_fallback_shortcuts(show_shortcut, capture_shortcut)?;

    let (old_show, old_capture) = {
        let conn = db.0.lock().map_err(err)?;
        (
            db::get_setting(&conn, "show_shortcut")
                .unwrap_or_else(|| capture::DEFAULT_SHOW_SHORTCUT.into()),
            db::get_setting(&conn, "capture_shortcut")
                .unwrap_or_else(|| capture::DEFAULT_CAPTURE_SHORTCUT.into()),
        )
    };
    if old_show == show_shortcut && old_capture == capture_shortcut {
        return Ok(());
    }

    capture::replace_fallback_shortcuts(
        &app,
        &old_show,
        &old_capture,
        show_shortcut,
        capture_shortcut,
    )?;

    let save_result = (|| -> rusqlite::Result<()> {
        let mut conn = db.0.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = conn.transaction()?;
        db::set_setting(&tx, "show_shortcut", show_shortcut)?;
        db::set_setting(&tx, "capture_shortcut", capture_shortcut)?;
        tx.commit()
    })();
    if let Err(error) = save_result {
        let _ = capture::replace_fallback_shortcuts(
            &app,
            show_shortcut,
            capture_shortcut,
            &old_show,
            &old_capture,
        );
        return Err(err(error));
    }
    refresh(&app);
    Ok(())
}

/// Add a note/prompt from the input. A line of the form "## Name" creates (or
/// switches to) a list instead of adding an item. "# Name" remains supported
/// for compatibility with the original Cooper shortcut.
#[tauri::command]
pub fn add_entry(
    app: AppHandle,
    db: State<db::Db>,
    content: String,
    attachment_paths: Vec<String>,
) -> CmdResult<()> {
    let text = item_content(&content)?;
    let list_name = entry_list_name(text);
    if let Some(name) = list_name {
        if !attachment_paths.is_empty() {
            return Err("Remove attachments before creating a list".into());
        }
        let name = section_name(name)?;
        let conn = db.0.lock().map_err(err)?;
        let id = db::find_or_create_section(&conn, name).map_err(err)?;
        db::set_active_section(&conn, Some(id)).map_err(err)?;
    } else {
        let attachments = inspect_attachment_paths(attachment_paths)?;
        let attachments_dir = app.path().app_data_dir().map_err(err)?.join("attachments");
        fs::create_dir_all(&attachments_dir).map_err(err)?;

        let mut conn = db.0.lock().map_err(err)?;
        let active = db::get_active_section(&conn);
        let tx = conn.transaction().map_err(err)?;
        let item_id = db::insert_item(&tx, text, active).map_err(err)?;
        let mut copied_paths = Vec::with_capacity(attachments.len());
        let pasted_sources = attachments
            .iter()
            .filter(|attachment| is_paste_draft(&app, Path::new(&attachment.path)))
            .map(|attachment| PathBuf::from(&attachment.path))
            .collect::<Vec<_>>();
        for (index, attachment) in attachments.iter().enumerate() {
            let source = PathBuf::from(&attachment.path);
            let target = stored_attachment_path(&attachments_dir, item_id, index, &source);
            if let Err(error) = fs::copy(&source, &target) {
                remove_stored_files(&copied_paths);
                return Err(format!("Couldn’t copy {}: {error}", attachment.name));
            }
            copied_paths.push(target.clone());
            if let Err(error) = tx.execute(
                "INSERT INTO attachments(item_id, name, stored_path, media_type, size, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    item_id,
                    attachment.name,
                    target.to_string_lossy(),
                    attachment.media_type,
                    attachment.size,
                    db::now_ms(),
                ],
            ) {
                remove_stored_files(&copied_paths);
                return Err(err(error));
            }
        }
        if let Err(error) = tx.commit() {
            remove_stored_files(&copied_paths);
            return Err(err(error));
        }
        remove_stored_files(&pasted_sources);
    }
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn inspect_attachments(paths: Vec<String>) -> CmdResult<Vec<AttachmentCandidate>> {
    inspect_attachment_paths(paths)
}

#[tauri::command]
pub fn get_attachment_preview(db: State<db::Db>, id: i64) -> CmdResult<Option<String>> {
    let conn = db.0.lock().map_err(err)?;
    let (path, media_type, size): (String, String, i64) = conn
        .query_row(
            "SELECT stored_path, media_type, size FROM attachments WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| "That attachment no longer exists".to_string())?;
    drop(conn);
    Ok(attachment_preview(
        Path::new(&path),
        &media_type,
        size.max(0) as u64,
    ))
}

#[tauri::command]
pub fn open_attachment(db: State<db::Db>, id: i64) -> CmdResult<()> {
    let conn = db.0.lock().map_err(err)?;
    let path: String = conn
        .query_row(
            "SELECT stored_path FROM attachments WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .map_err(|_| "That attachment no longer exists".to_string())?;
    drop(conn);
    open_path(Path::new(&path))
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
    let mut paths = Vec::new();
    {
        let mut statement = conn
            .prepare("SELECT stored_path FROM attachments WHERE item_id = ?1")
            .map_err(err)?;
        for id in &ids {
            let rows = statement
                .query_map(rusqlite::params![id], |row| row.get::<_, String>(0))
                .map_err(err)?;
            for row in rows {
                paths.push(PathBuf::from(row.map_err(err)?));
            }
        }
    }
    let tx = conn.transaction().map_err(err)?;
    for id in ids {
        tx.execute("DELETE FROM items WHERE id = ?1", rusqlite::params![id])
            .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    drop(conn);
    remove_stored_files(&paths);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn merge_items(app: AppHandle, db: State<db::Db>, ids: Vec<i64>) -> CmdResult<()> {
    let ids = item_ids(ids)?;
    if ids.len() < 2 {
        return Err("Select at least two prompts to merge".into());
    }

    let mut conn = db.0.lock().map_err(err)?;
    let tx = conn.transaction().map_err(err)?;
    let mut contents = Vec::with_capacity(ids.len());
    for id in &ids {
        let content: String = tx
            .query_row(
                "SELECT content FROM items WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(|_| "One of those prompts no longer exists".to_string())?;
        contents.push(content);
    }
    let merged = contents.join("\n\n");
    if merged.chars().count() > db::MAX_ITEM_CHARS {
        return Err("Those prompts are too long to merge".into());
    }

    let target = ids[0];
    tx.execute(
        "UPDATE items SET content = ?2, done = 0, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![target, merged, db::now_ms()],
    )
    .map_err(err)?;
    for id in ids.iter().skip(1) {
        tx.execute(
            "UPDATE attachments SET item_id = ?1 WHERE item_id = ?2",
            rusqlite::params![target, id],
        )
        .map_err(err)?;
        tx.execute("DELETE FROM items WHERE id = ?1", rusqlite::params![id])
            .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn move_items(
    app: AppHandle,
    db: State<db::Db>,
    ids: Vec<i64>,
    section_id: Option<i64>,
) -> CmdResult<()> {
    let ids = item_ids(ids)?;
    if ids.is_empty() {
        return Ok(());
    }
    let mut conn = db.0.lock().map_err(err)?;
    if let Some(id) = section_id {
        if !db::section_exists(&conn, id).map_err(err)? {
            return Err("That list no longer exists".into());
        }
    }
    let tx = conn.transaction().map_err(err)?;
    for id in ids {
        let updated = tx
            .execute(
                "UPDATE items SET section_id = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, section_id, db::now_ms()],
            )
            .map_err(err)?;
        if updated == 0 {
            return Err("One of those prompts no longer exists".into());
        }
    }
    tx.commit().map_err(err)?;
    drop(conn);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn clear_completed(app: AppHandle, db: State<db::Db>) -> CmdResult<()> {
    let conn = db.0.lock().map_err(err)?;
    let mut paths = Vec::new();
    {
        let mut statement = conn
            .prepare(
                "SELECT attachments.stored_path
                 FROM attachments
                 JOIN items ON items.id = attachments.item_id
                 WHERE items.done = 1",
            )
            .map_err(err)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(err)?;
        for row in rows {
            paths.push(PathBuf::from(row.map_err(err)?));
        }
    }
    conn.execute("DELETE FROM items WHERE done = 1", [])
        .map_err(err)?;
    drop(conn);
    remove_stored_files(&paths);
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn set_active_section(app: AppHandle, db: State<db::Db>, id: Option<i64>) -> CmdResult<()> {
    let conn = db.0.lock().map_err(err)?;
    if let Some(id) = id {
        if !db::section_exists(&conn, id).map_err(err)? {
            return Err("That list no longer exists".into());
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
        return Err("A list with that name already exists".into());
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

/// Delete a list. Prompts either fall back to Inbox or are deleted with their
/// stored attachment files, depending on the explicit user choice.
#[tauri::command]
pub fn delete_section(
    app: AppHandle,
    db: State<db::Db>,
    id: i64,
    delete_items: bool,
) -> CmdResult<()> {
    if id <= 0 {
        return Err("Invalid list id".into());
    }

    let mut conn = db.0.lock().map_err(err)?;
    let was_active = db::get_active_section(&conn) == Some(id);
    let mut paths = Vec::new();
    if delete_items {
        let mut statement = conn
            .prepare(
                "SELECT attachments.stored_path
                 FROM attachments
                 INNER JOIN items ON items.id = attachments.item_id
                 WHERE items.section_id = ?1",
            )
            .map_err(err)?;
        let rows = statement
            .query_map(rusqlite::params![id], |row| row.get::<_, String>(0))
            .map_err(err)?;
        for row in rows {
            paths.push(PathBuf::from(row.map_err(err)?));
        }
    }

    let tx = conn.transaction().map_err(err)?;
    if delete_items {
        tx.execute(
            "DELETE FROM items WHERE section_id = ?1",
            rusqlite::params![id],
        )
        .map_err(err)?;
    } else {
        tx.execute(
            "UPDATE items SET section_id = NULL WHERE section_id = ?1",
            rusqlite::params![id],
        )
        .map_err(err)?;
    }
    let removed = tx
        .execute("DELETE FROM sections WHERE id = ?1", rusqlite::params![id])
        .map_err(err)?;
    if removed == 0 {
        return Err("That list no longer exists".into());
    }
    if was_active {
        db::set_active_section(&tx, None).map_err(err)?;
    }
    tx.commit().map_err(err)?;
    drop(conn);
    if delete_items {
        remove_stored_files(&paths);
    }
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
pub fn set_keep_on_top(app: AppHandle, db: State<db::Db>, enabled: bool) -> CmdResult<()> {
    {
        let conn = db.0.lock().map_err(err)?;
        db::set_setting(&conn, "keep_on_top", if enabled { "true" } else { "false" })
            .map_err(err)?;
    }
    for window in app.webview_windows().into_values() {
        window.set_always_on_top(enabled).map_err(err)?;
    }
    refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn copy_text(text: String, paths: Vec<String>) -> CmdResult<()> {
    if paths.len() > db::MAX_BATCH_ITEMS {
        return Err("Too many attachments to copy".into());
    }
    let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();

    #[cfg(target_os = "macos")]
    if !paths.is_empty() {
        return crate::macos::copy_text_and_files(&text, &paths);
    }

    #[cfg(not(target_os = "macos"))]
    let text = if paths.is_empty() {
        text
    } else {
        let attachments = paths
            .iter()
            .map(|path| format!("- {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{text}\n\nAttachments:\n{attachments}")
    };

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

    let mut md = String::from("# Clippy export\n\n");
    let write_items = |md: &mut String, section_id: Option<i64>| {
        for item in state.items.iter().filter(|i| i.section_id == section_id) {
            let mark = if item.done { "x" } else { " " };
            let content = item.content.replace('\n', "\n  ");
            md.push_str(&format!("- [{mark}] {content}\n"));
            for attachment in &item.attachments {
                md.push_str(&format!("  - Attachment: {}\n", attachment.path));
            }
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
            "clippy-export.md".to_string()
        } else {
            format!("clippy-export-{suffix}.md")
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
    Err("Too many Clippy exports in Documents".into())
}

#[tauri::command]
pub fn reveal_notes(app: AppHandle) -> CmdResult<()> {
    let path = app.path().app_data_dir().map_err(err)?.join("clippy.db");

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R");
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg("/select,");
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(&path));
        command.spawn().map_err(err)?;
        return Ok(());
    };

    command.arg(path).spawn().map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn check_for_updates() -> CmdResult<()> {
    open_path(Path::new(
        "https://github.com/alexandrenf/clippy/releases/latest",
    ))
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
    .title("Edit — Clippy")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_new_and_legacy_inline_list_commands() {
        assert_eq!(entry_list_name("## Docs review"), Some("Docs review"));
        assert_eq!(entry_list_name("# Docs review"), Some("Docs review"));
        assert_eq!(entry_list_name("Review ## headings"), None);
    }

    #[test]
    fn inspects_image_attachments_for_composer_previews() {
        let path = std::env::temp_dir().join(format!(
            "clippy-attachment-test-{}-{}.png",
            std::process::id(),
            db::now_ms()
        ));
        fs::write(&path, b"not-a-real-image-but-safe-to-preview").unwrap();

        let attachment = inspect_attachment_path(path.to_str().unwrap()).unwrap();
        assert_eq!(attachment.name, path.file_name().unwrap().to_string_lossy());
        assert_eq!(attachment.media_type, "image/png");
        assert!(attachment
            .preview
            .unwrap()
            .starts_with("data:image/png;base64,"));

        fs::remove_file(path).unwrap();
    }
}
