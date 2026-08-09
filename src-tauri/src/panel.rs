use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

/// Dock to the usable right edge of the screen under the pointer. This makes
/// the global shortcut follow the active display and avoids the menu bar/Dock.
pub fn position(win: &WebviewWindow) {
    let monitor = match win
        .cursor_position()
        .ok()
        .and_then(|p| win.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| win.current_monitor().ok().flatten())
    {
        Some(m) => m,
        None => match win.primary_monitor() {
            Ok(Some(m)) => m,
            _ => return,
        },
    };
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    let wsize = match win.outer_size() {
        Ok(s) => s,
        Err(_) => return,
    };
    let margin = (16.0 * scale) as i32;
    let x = work.position.x + work.size.width as i32 - wsize.width as i32 - margin;
    let available_height = work.size.height as i32;
    let y = work.position.y + ((available_height - wsize.height as i32) / 2).max(margin);
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

pub fn show(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        position(&win);
        let _ = win.show();
        let _ = win.set_focus();
        let _ = app.emit("panel-shown", ());
    }
}

pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

pub fn toggle(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let visible = win.is_visible().unwrap_or(false);
        let focused = win.is_focused().unwrap_or(false);
        if visible && focused {
            let _ = win.hide();
        } else if visible {
            let _ = win.set_focus();
            let _ = app.emit("panel-shown", ());
        } else {
            show(app);
        }
    }
}
