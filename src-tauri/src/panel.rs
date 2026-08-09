use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

static FEEDBACK_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureFeedbackPayload {
    kind: String,
    preview: String,
}

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
        // Clicking the tray removes focus from the panel before this callback
        // runs. Visibility, rather than focus, is therefore the reliable
        // source of truth for an explicit Show / Hide action.
        if visible {
            let _ = win.hide();
        } else {
            show(app);
        }
    }
}

fn position_capture_feedback(win: &WebviewWindow) {
    let monitor = win
        .cursor_position()
        .ok()
        .and_then(|point| win.monitor_from_point(point.x, point.y).ok().flatten())
        .or_else(|| win.current_monitor().ok().flatten())
        .or_else(|| win.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let work = monitor.work_area();
    let Ok(size) = win.outer_size() else {
        return;
    };
    let x = work.position.x + (work.size.width as i32 - size.width as i32) / 2;
    let y = work.position.y + (24.0 * monitor.scale_factor()) as i32;
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

pub fn capture_feedback(app: &AppHandle, kind: &str, preview: impl Into<String>) {
    if app
        .get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
    {
        return;
    }
    let Some(window) = app.get_webview_window("capture-toast") else {
        return;
    };

    let generation = FEEDBACK_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    position_capture_feedback(&window);
    let _ = window.set_focusable(false);
    let _ = window.set_ignore_cursor_events(true);
    let _ = window.show();
    let _ = app.emit_to(
        "capture-toast",
        "capture-feedback",
        CaptureFeedbackPayload {
            kind: kind.into(),
            preview: preview.into(),
        },
    );

    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2_050));
        if FEEDBACK_GENERATION.load(Ordering::Relaxed) == generation {
            if let Some(window) = app.get_webview_window("capture-toast") {
                let _ = window.hide();
            }
        }
    });
}
