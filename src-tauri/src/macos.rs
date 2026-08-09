//! Small, focused wrappers around macOS Accessibility and pasteboard APIs.
//! The Accessibility path reads a selection without touching the clipboard.

use objc2_app_kit::NSPasteboard;
use std::ffi::{c_char, c_void, CStr};
use std::ptr;

type AXUIElementRef = *const c_void;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;

const AX_SUCCESS: i32 = 0;
const UTF8: u32 = 0x0800_0100;

#[derive(Debug)]
pub enum Selection {
    Text(String),
    Empty,
    Unsupported,
    PermissionDenied,
    Protected,
}

struct OwnedCf(CFTypeRef);

impl Drop for OwnedCf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> i32;

    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: CFTypeRef);
    fn CFEqual(left: CFTypeRef, right: CFTypeRef) -> u8;
    fn CFGetTypeID(value: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(value: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        value: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        value: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    static kCFBooleanTrue: CFTypeRef;
}

unsafe fn ax_name(value: &'static [u8]) -> Option<OwnedCf> {
    let value = CStr::from_bytes_with_nul_unchecked(value);
    let string = CFStringCreateWithCString(ptr::null(), value.as_ptr(), UTF8);
    (!string.is_null()).then_some(OwnedCf(string))
}

unsafe fn copy_attribute(element: AXUIElementRef, attribute: CFStringRef) -> Option<OwnedCf> {
    let mut value = ptr::null();
    let result = AXUIElementCopyAttributeValue(element, attribute, &mut value);
    (result == AX_SUCCESS && !value.is_null()).then_some(OwnedCf(value))
}

unsafe fn cf_string(value: CFTypeRef) -> Option<String> {
    if CFGetTypeID(value) != CFStringGetTypeID() {
        return None;
    }
    let length = CFStringGetLength(value);
    let capacity = CFStringGetMaximumSizeForEncoding(length, UTF8).checked_add(1)?;
    if capacity <= 0 {
        return Some(String::new());
    }
    let mut bytes = vec![0_u8; capacity as usize];
    if CFStringGetCString(value, bytes.as_mut_ptr().cast(), capacity, UTF8) == 0 {
        return None;
    }
    Some(
        CStr::from_ptr(bytes.as_ptr().cast())
            .to_string_lossy()
            .into_owned(),
    )
}

pub fn selected_text() -> Selection {
    unsafe {
        if !accessibility_trusted() {
            return Selection::PermissionDenied;
        }

        // These Accessibility names are CFSTR header constants rather than
        // exported linker symbols on current macOS SDKs, so construct their
        // documented string values once for this short capture operation.
        let Some(focused_attribute) = ax_name(b"AXFocusedUIElement\0") else {
            return Selection::Unsupported;
        };
        let Some(parent_attribute) = ax_name(b"AXParent\0") else {
            return Selection::Unsupported;
        };
        let Some(selected_text_attribute) = ax_name(b"AXSelectedText\0") else {
            return Selection::Unsupported;
        };
        let Some(subrole_attribute) = ax_name(b"AXSubrole\0") else {
            return Selection::Unsupported;
        };
        let Some(secure_text_subrole) = ax_name(b"AXSecureTextField\0") else {
            return Selection::Unsupported;
        };

        let system = OwnedCf(AXUIElementCreateSystemWide());
        if system.0.is_null() {
            return Selection::Unsupported;
        }
        let Some(mut element) = copy_attribute(system.0, focused_attribute.0) else {
            return Selection::Unsupported;
        };

        for _ in 0..=6 {
            let _ = AXUIElementSetMessagingTimeout(element.0, 0.2);

            if let Some(subrole) = copy_attribute(element.0, subrole_attribute.0) {
                if CFEqual(subrole.0, secure_text_subrole.0) != 0 {
                    return Selection::Protected;
                }
            }

            if let Some(selected) = copy_attribute(element.0, selected_text_attribute.0) {
                if let Some(text) = cf_string(selected.0) {
                    return if text.is_empty() {
                        Selection::Empty
                    } else {
                        Selection::Text(text)
                    };
                }
            }

            let Some(parent) = copy_attribute(element.0, parent_attribute.0) else {
                return Selection::Unsupported;
            };
            element = parent;
        }
    }
    Selection::Unsupported
}

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

pub fn input_monitoring_trusted() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}

pub fn capture_permissions_granted() -> bool {
    accessibility_trusted() && input_monitoring_trusted()
}

/// Ask macOS to explain the Accessibility requirement. The return value is the
/// current status; permission changes after the system prompt are asynchronous.
pub fn request_accessibility_permission() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt.cast::<c_void>()];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            ptr::null(),
            ptr::null(),
        );
        if options.is_null() {
            let listening = CGRequestListenEventAccess();
            return accessibility_trusted() && listening;
        }
        let trusted = AXIsProcessTrustedWithOptions(options) != 0;
        CFRelease(options);
        let listening = CGRequestListenEventAccess();
        trusted && listening
    }
}

pub fn pasteboard_generation() -> i64 {
    NSPasteboard::generalPasteboard().changeCount() as i64
}
