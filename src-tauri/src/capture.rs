use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use crate::{db, panel};

const TAP_WINDOW: Duration = Duration::from_millis(400);
const HOLD_LIMIT: Duration = Duration::from_millis(500);
const COPY_DEADLINE: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy)]
enum CaptureRequest {
    Selection,
    Clipboard,
}

static CAPTURE_TX: OnceLock<SyncSender<CaptureRequest>> = OnceLock::new();

/// One bounded worker serializes Accessibility, pasteboard, and database work.
/// A burst can queue one extra request without spawning unbounded threads.
pub fn start_capture_worker(app: AppHandle) {
    let (tx, rx) = sync_channel(1);
    if CAPTURE_TX.set(tx).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("cooper-capture".into())
        .spawn(move || {
            while let Ok(request) = rx.recv() {
                let result = match request {
                    CaptureRequest::Selection => capture_selected_text(&app),
                    CaptureRequest::Clipboard => capture_clipboard_text(&app),
                };
                if let Err(error) = result {
                    eprintln!("cooper: capture failed: {error}");
                    notify_error(&app, &error);
                }
            }
        });
}

fn enqueue(request: CaptureRequest) {
    let Some(tx) = CAPTURE_TX.get() else {
        eprintln!("cooper: capture worker is not ready");
        return;
    };
    match tx.try_send(request) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => eprintln!("cooper: capture worker stopped"),
    }
}

/// Global low-level keyboard listener: Left Shift x2 captures the current
/// selection, Right Shift x2 toggles the panel. Runs on its own thread; if the
/// OS denies the hook, the standard fallback shortcuts below still work.
pub fn start_double_shift_listener(app: AppHandle) {
    std::thread::spawn(move || {
        let mut pressed: Option<(Side, Instant)> = None;
        let mut dirty = false;
        let mut last_tap: Option<(Side, Instant)> = None;

        let result = rdev::listen(move |event| {
            use rdev::{EventType, Key};
            match event.event_type {
                EventType::KeyPress(Key::ShiftLeft) => {
                    if pressed.is_none() {
                        pressed = Some((Side::Left, Instant::now()));
                        dirty = false;
                    }
                }
                EventType::KeyPress(Key::ShiftRight) => {
                    if pressed.is_none() {
                        pressed = Some((Side::Right, Instant::now()));
                        dirty = false;
                    }
                }
                EventType::KeyPress(_) => {
                    dirty = true;
                    last_tap = None;
                }
                EventType::KeyRelease(Key::ShiftLeft) | EventType::KeyRelease(Key::ShiftRight) => {
                    let side = if matches!(event.event_type, EventType::KeyRelease(Key::ShiftLeft))
                    {
                        Side::Left
                    } else {
                        Side::Right
                    };
                    let tap_ok = matches!(
                        pressed,
                        Some((s, t)) if s == side && !dirty && t.elapsed() < HOLD_LIMIT
                    );
                    pressed = None;
                    if !tap_ok {
                        last_tap = None;
                        return;
                    }
                    if let Some((s, t)) = last_tap {
                        if s == side && t.elapsed() < TAP_WINDOW {
                            last_tap = None;
                            match side {
                                Side::Left => capture_selection(&app),
                                Side::Right => panel::toggle(&app),
                            }
                            return;
                        }
                    }
                    last_tap = Some((side, Instant::now()));
                }
                _ => {}
            }
        });
        if let Err(e) = result {
            eprintln!("cooper: global keyboard listener unavailable ({e:?}); double-shift shortcuts disabled, fallback hotkeys still active");
        }
    });
}

/// Standard hotkeys remain available when the raw modifier listener is denied.
/// Capture on release so the synthetic fallback never races held modifiers.
pub fn register_fallback_shortcuts(app: &AppHandle) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let gs = app.global_shortcut();
    if let Err(e) = gs.on_shortcut("CmdOrCtrl+Shift+Space", |app, _shortcut, event| {
        if event.state() == ShortcutState::Released {
            panel::toggle(app);
        }
    }) {
        eprintln!("cooper: could not register CmdOrCtrl+Shift+Space: {e}");
    }
    if let Err(e) = gs.on_shortcut("CmdOrCtrl+Alt+C", |app, _shortcut, event| {
        if event.state() == ShortcutState::Released {
            capture_selection(app);
        }
    }) {
        eprintln!("cooper: could not register CmdOrCtrl+Alt+C: {e}");
    }
}

pub fn capture_selection(_app: &AppHandle) {
    enqueue(CaptureRequest::Selection);
}

pub fn capture_clipboard(_app: &AppHandle) {
    enqueue(CaptureRequest::Clipboard);
}

fn capture_selected_text(app: &AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    match crate::macos::selected_text() {
        crate::macos::Selection::Text(text) => return add_text_item(app, &text),
        crate::macos::Selection::Empty => {
            let _ = app.emit("capture-empty", ());
            return Ok(());
        }
        crate::macos::Selection::Protected => {
            return Err("Cooper never captures text from secure fields".into())
        }
        crate::macos::Selection::PermissionDenied => {
            panel::show(app);
            return Err(
                "Allow Cooper in System Settings → Privacy & Security → Accessibility".into(),
            );
        }
        crate::macos::Selection::Unsupported => {}
    }

    capture_via_copy(app)
}

/// Read the clipboard only after an explicit in-app action.
fn capture_clipboard_text(app: &AppHandle) -> Result<(), String> {
    let text = arboard::Clipboard::new()
        .map_err(|e| e.to_string())?
        .get_text()
        .map_err(|_| "The clipboard does not contain text".to_string())?;
    add_text_item(app, &text)
}

/// Compatibility path for apps that do not expose selected text through macOS
/// Accessibility. It never writes a sentinel, so an empty selection cannot
/// destroy images, files, rich text, or custom pasteboard representations.
fn capture_via_copy(app: &AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let generation = crate::macos::pasteboard_generation();

    #[cfg(not(target_os = "macos"))]
    let previous = arboard::Clipboard::new()
        .ok()
        .and_then(|mut clipboard| clipboard.get_text().ok());

    send_copy()?;
    let started = Instant::now();
    let mut delay = Duration::from_millis(5);

    while started.elapsed() < COPY_DEADLINE {
        std::thread::sleep(delay);

        #[cfg(target_os = "macos")]
        let changed = crate::macos::pasteboard_generation() != generation;

        #[cfg(not(target_os = "macos"))]
        let changed = arboard::Clipboard::new()
            .ok()
            .and_then(|mut clipboard| clipboard.get_text().ok())
            .as_ref()
            != previous.as_ref();

        if changed {
            let text = arboard::Clipboard::new()
                .map_err(|e| e.to_string())?
                .get_text()
                .map_err(|_| "The selection does not contain text".to_string())?;
            return add_text_item(app, &text);
        }
        delay = (delay * 2).min(Duration::from_millis(25));
    }

    let _ = app.emit("capture-empty", ());
    Ok(())
}

fn add_text_item(app: &AppHandle, text: &str) -> Result<(), String> {
    if !text.chars().any(|c| !c.is_whitespace()) {
        let _ = app.emit("capture-empty", ());
        return Ok(());
    }
    if text.chars().count() > db::MAX_ITEM_CHARS {
        return Err(format!(
            "Selection is longer than {} characters",
            db::MAX_ITEM_CHARS
        ));
    }
    let database = app.state::<db::Db>();
    let conn = database.0.lock().map_err(|e| e.to_string())?;
    let active = db::get_active_section(&conn);
    if db::is_recent_duplicate(&conn, text, active, db::now_ms() - 1_500)
        .map_err(|e| e.to_string())?
    {
        drop(conn);
        let _ = app.emit("capture-duplicate", ());
        return Ok(());
    }
    db::insert_item(&conn, text, active).map_err(|e| e.to_string())?;
    drop(conn);
    let _ = app.emit("refresh", ());
    let _ = app.emit("captured", ());
    Ok(())
}

fn notify_error(app: &AppHandle, message: &str) {
    let _ = app.emit("capture-error", message);
}

fn send_copy() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;
    #[cfg(target_os = "macos")]
    let copy_key = Key::Unicode('c');
    // Ctrl+C can send SIGINT to a terminal when no text is selected. The
    // standard Ctrl+Insert copy chord avoids that destructive side effect.
    #[cfg(not(target_os = "macos"))]
    let copy_key = Key::Insert;

    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| e.to_string())?;
    let clicked = enigo
        .key(copy_key, Direction::Click)
        .map_err(|e| e.to_string());
    let released = enigo
        .key(modifier, Direction::Release)
        .map_err(|e| e.to_string());
    clicked.and(released)
}
