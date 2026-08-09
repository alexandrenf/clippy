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
        .name("clippy-capture".into())
        .spawn(move || {
            while let Ok(request) = rx.recv() {
                let result = match request {
                    CaptureRequest::Selection => capture_selected_text(&app),
                    CaptureRequest::Clipboard => capture_clipboard_text(&app),
                };
                if let Err(error) = result {
                    eprintln!("clippy: capture failed: {error}");
                    notify_error(&app, &error);
                }
            }
        });
}

fn enqueue(request: CaptureRequest) {
    let Some(tx) = CAPTURE_TX.get() else {
        eprintln!("clippy: capture worker is not ready");
        return;
    };
    match tx.try_send(request) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => eprintln!("clippy: capture worker stopped"),
    }
}

/// Global low-level keyboard listener: Left Shift x2 captures the current
/// selection, Right Shift x2 toggles the panel. Runs on its own thread; if the
/// OS denies the hook, the standard fallback shortcuts below still work.
#[cfg(not(target_os = "macos"))]
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
            eprintln!("clippy: global keyboard listener unavailable ({e:?}); double-shift shortcuts disabled, fallback hotkeys still active");
        }
    });
}

/// macOS 26 traps when rdev asks Carbon to translate a key into text from its
/// event-tap thread. Clippy only needs physical shift keycodes, so use a small
/// keycode-only event tap and avoid the keyboard-layout APIs entirely.
#[cfg(target_os = "macos")]
pub fn start_double_shift_listener(app: AppHandle) {
    macos_key_tap::start(app);
}

#[cfg(target_os = "macos")]
mod macos_key_tap {
    use super::{capture_selection, panel, AppHandle, Side, HOLD_LIMIT, TAP_WINDOW};
    use std::ffi::c_void;
    use std::ptr;
    use std::time::{Duration, Instant};

    type CgEventRef = *mut c_void;
    type CgEventTapProxy = *mut c_void;
    type CfMachPortRef = *mut c_void;
    type CfRunLoopSourceRef = *mut c_void;
    type CfRunLoopRef = *mut c_void;
    type CfRunLoopMode = *const c_void;

    const KEY_DOWN: u32 = 10;
    const FLAGS_CHANGED: u32 = 12;
    const TAP_DISABLED_BY_TIMEOUT: u32 = u32::MAX - 1;
    const TAP_DISABLED_BY_USER_INPUT: u32 = u32::MAX;
    const KEYBOARD_EVENT_KEYCODE: u32 = 9;
    const SHIFT_LEFT: i64 = 56;
    const SHIFT_RIGHT: i64 = 60;
    const EVENT_MASK: u64 = (1 << KEY_DOWN) | (1 << FLAGS_CHANGED);

    struct ListenerState {
        app: AppHandle,
        tap: CfMachPortRef,
        left_down: bool,
        right_down: bool,
        pressed: Option<(Side, Instant)>,
        dirty: bool,
        last_tap: Option<(Side, Instant)>,
    }

    impl ListenerState {
        fn shift_changed(&mut self, side: Side) {
            let down = match side {
                Side::Left => &mut self.left_down,
                Side::Right => &mut self.right_down,
            };
            if !*down {
                *down = true;
                if self.pressed.is_none() {
                    self.pressed = Some((side, Instant::now()));
                    self.dirty = false;
                } else {
                    self.dirty = true;
                    self.last_tap = None;
                }
                return;
            }

            *down = false;
            let tap_ok = matches!(
                self.pressed,
                Some((pressed_side, started))
                    if pressed_side == side && !self.dirty && started.elapsed() < HOLD_LIMIT
            );
            self.pressed = None;
            if !tap_ok {
                self.last_tap = None;
                return;
            }

            if let Some((last_side, last_time)) = self.last_tap {
                if last_side == side && last_time.elapsed() < TAP_WINDOW {
                    self.last_tap = None;
                    match side {
                        Side::Left => capture_selection(&self.app),
                        Side::Right => {
                            let app = self.app.clone();
                            let scheduler = app.clone();
                            if let Err(error) =
                                scheduler.run_on_main_thread(move || panel::toggle(&app))
                            {
                                eprintln!("clippy: could not toggle panel: {error}");
                            }
                        }
                    }
                    return;
                }
            }
            self.last_tap = Some((side, Instant::now()));
        }
    }

    unsafe extern "C" fn event_callback(
        _proxy: CgEventTapProxy,
        event_type: u32,
        event: CgEventRef,
        user_info: *mut c_void,
    ) -> CgEventRef {
        if user_info.is_null() {
            return event;
        }
        let state = &mut *user_info.cast::<ListenerState>();
        if event_type == TAP_DISABLED_BY_TIMEOUT || event_type == TAP_DISABLED_BY_USER_INPUT {
            if !state.tap.is_null() {
                CGEventTapEnable(state.tap, true);
            }
            return event;
        }
        if event.is_null() {
            return event;
        }
        if event_type == KEY_DOWN {
            state.dirty = true;
            state.last_tap = None;
        } else if event_type == FLAGS_CHANGED {
            match CGEventGetIntegerValueField(event, KEYBOARD_EVENT_KEYCODE) {
                SHIFT_LEFT => state.shift_changed(Side::Left),
                SHIFT_RIGHT => state.shift_changed(Side::Right),
                _ => {}
            }
        }
        event
    }

    pub fn start(app: AppHandle) {
        let _ = std::thread::Builder::new()
            .name("clippy-key-listener".into())
            .spawn(move || unsafe {
                loop {
                    // An ad-hoc rebuild changes the app's code identity and can
                    // invalidate its previous Input Monitoring grant. Wait for
                    // the user to re-authorize it instead of permanently
                    // abandoning Double Shift during this launch.
                    while !CGPreflightListenEventAccess() {
                        std::thread::sleep(Duration::from_secs(1));
                    }

                    let state = Box::new(ListenerState {
                        app: app.clone(),
                        tap: ptr::null_mut(),
                        left_down: false,
                        right_down: false,
                        pressed: None,
                        dirty: false,
                        last_tap: None,
                    });
                    let state = Box::into_raw(state);
                    let tap = CGEventTapCreate(
                        0,
                        0,
                        1,
                        EVENT_MASK,
                        event_callback,
                        state.cast::<c_void>(),
                    );
                    if tap.is_null() {
                        drop(Box::from_raw(state));
                        eprintln!("clippy: global keyboard listener unavailable; retrying");
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    (*state).tap = tap;
                    let source = CFMachPortCreateRunLoopSource(ptr::null(), tap, 0);
                    if source.is_null() {
                        CFRelease(tap.cast_const());
                        drop(Box::from_raw(state));
                        eprintln!(
                            "clippy: could not create the macOS keyboard event source; retrying"
                        );
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    let run_loop = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
                    CGEventTapEnable(tap, true);
                    CFRunLoopRun();
                    CFRelease(source.cast_const());
                    CFRelease(tap.cast_const());
                    drop(Box::from_raw(state));
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: unsafe extern "C" fn(
                CgEventTapProxy,
                u32,
                CgEventRef,
                *mut c_void,
            ) -> CgEventRef,
            user_info: *mut c_void,
        ) -> CfMachPortRef;
        fn CGEventTapEnable(tap: CfMachPortRef, enable: bool);
        fn CGPreflightListenEventAccess() -> bool;
        fn CGEventGetIntegerValueField(event: CgEventRef, field: u32) -> i64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: CfMachPortRef,
            order: isize,
        ) -> CfRunLoopSourceRef;
        fn CFRunLoopGetCurrent() -> CfRunLoopRef;
        fn CFRunLoopAddSource(
            run_loop: CfRunLoopRef,
            source: CfRunLoopSourceRef,
            mode: CfRunLoopMode,
        );
        fn CFRunLoopRun();
        fn CFRelease(value: *const c_void);
        static kCFRunLoopCommonModes: CfRunLoopMode;
    }
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
        eprintln!("clippy: could not register CmdOrCtrl+Shift+Space: {e}");
    }
    if let Err(e) = gs.on_shortcut("CmdOrCtrl+Alt+C", |app, _shortcut, event| {
        if event.state() == ShortcutState::Released {
            capture_selection(app);
        }
    }) {
        eprintln!("clippy: could not register CmdOrCtrl+Alt+C: {e}");
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
            return Err("Clippy never captures text from secure fields".into())
        }
        crate::macos::Selection::PermissionDenied => {
            panel::show(app);
            return Err(
                "Allow Clippy in System Settings → Privacy & Security → Accessibility".into(),
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
