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
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> i32;

    static kAXFocusedUIElementAttribute: CFStringRef;
    static kAXParentAttribute: CFStringRef;
    static kAXSelectedTextAttribute: CFStringRef;
    static kAXSubroleAttribute: CFStringRef;
    static kAXSecureTextFieldSubrole: CFStringRef;
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

        let system = OwnedCf(AXUIElementCreateSystemWide());
        if system.0.is_null() {
            return Selection::Unsupported;
        }
        let Some(mut element) = copy_attribute(system.0, kAXFocusedUIElementAttribute) else {
            return Selection::Unsupported;
        };

        for _ in 0..=6 {
            let _ = AXUIElementSetMessagingTimeout(element.0, 0.2);

            if let Some(subrole) = copy_attribute(element.0, kAXSubroleAttribute) {
                if CFEqual(subrole.0, kAXSecureTextFieldSubrole) != 0 {
                    return Selection::Protected;
                }
            }

            if let Some(selected) = copy_attribute(element.0, kAXSelectedTextAttribute) {
                if let Some(text) = cf_string(selected.0) {
                    return if text.is_empty() {
                        Selection::Empty
                    } else {
                        Selection::Text(text)
                    };
                }
            }

            let Some(parent) = copy_attribute(element.0, kAXParentAttribute) else {
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
            return accessibility_trusted();
        }
        let trusted = AXIsProcessTrustedWithOptions(options) != 0;
        CFRelease(options);
        trusted
    }
}

pub fn pasteboard_generation() -> i64 {
    NSPasteboard::generalPasteboard().changeCount() as i64
}
