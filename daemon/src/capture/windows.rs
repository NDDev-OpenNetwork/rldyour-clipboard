//! Clipboard capture on Windows.
//!
//! Windows does have a notification: `AddClipboardFormatListener` posts
//! `WM_CLIPBOARDUPDATE` to a window whenever the clipboard changes. That needs
//! a window and a message loop, so this backend owns a message-only window —
//! one that is never shown, never sized and exists purely to receive the
//! message. No polling, and no cost at all between copies.

use super::Recorder;
use std::ffi::c_void;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, CountClipboardFormats, EnumClipboardFormats,
    GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG,
    RegisterClassW, TranslateMessage, WM_CLIPBOARDUPDATE, WNDCLASSW,
};

/// Standard clipboard formats this backend understands.
const CF_TEXT: u32 = 1;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

/// The window class name and the window title, as UTF-16 with a terminator.
const CLASS_NAME: &[u16] = &[
    b'r' as u16,
    b'l' as u16,
    b'd' as u16,
    b'y' as u16,
    b'c' as u16,
    b'l' as u16,
    b'i' as u16,
    b'p' as u16,
    0,
];

/// Set once, before the window is created, and only read from the window
/// procedure — which runs on the same thread that created the window.
static RECORDER: OnceLock<&'static Recorder> = OnceLock::new();

pub fn watch(recorder: &Recorder) {
    // The recorder lives as long as the daemon, and the window procedure is a
    // plain `extern "system"` function with nowhere to put a borrow. Leaking a
    // reference is how it reaches the callback without a static mutable.
    let recorder: &'static Recorder =
        unsafe { std::mem::transmute::<&Recorder, &'static Recorder>(recorder) };
    if RECORDER.set(recorder).is_err() {
        return;
    }

    let Some(window) = create_listener_window() else {
        eprintln!("rldyour-clipboardd: could not create the clipboard listener window");
        return;
    };

    if unsafe { AddClipboardFormatListener(window) } == 0 {
        eprintln!("rldyour-clipboardd: could not register as a clipboard listener");
        return;
    }

    let mut message: MSG = unsafe { std::mem::zeroed() };
    // GetMessageW returns 0 on WM_QUIT and -1 on error; either ends the loop.
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn create_listener_window() -> Option<HWND> {
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_procedure),
        lpszClassName: CLASS_NAME.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    // A class name already registered is not an error worth stopping for: it
    // means a previous run in this process registered it.
    unsafe { RegisterClassW(&class) };

    let window = unsafe {
        CreateWindowExW(
            0,
            CLASS_NAME.as_ptr(),
            CLASS_NAME.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            // Message-only: never shown, never painted, never in the taskbar.
            HWND_MESSAGE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if window.is_null() { None } else { Some(window) }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLIPBOARDUPDATE {
        if let Some(recorder) = RECORDER.get() {
            if let Some(parts) = read() {
                recorder.record(parts, None);
            }
        }
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Reads every representation worth keeping from the clipboard.
///
/// Returns nothing when a password manager marked the contents as a secret, so
/// the bytes are never copied into this process at all.
fn read() -> Option<Vec<(String, Vec<u8>)>> {
    // The clipboard is a shared, singly-owned resource: another process may
    // hold it for a moment right after a copy.
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return None;
    }
    let parts = read_opened();
    unsafe { CloseClipboard() };
    parts
}

fn read_opened() -> Option<Vec<(String, Vec<u8>)>> {
    if concealed() {
        return None;
    }

    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();

    // Registered formats first: they carry the richest content, and the
    // standard ones below are the fallbacks for the same thing.
    if let Some(png) = registered("PNG") {
        push(&mut parts, "image/png", raw(png));
    }
    if let Some(html) = registered("HTML Format") {
        push(&mut parts, "text/html", raw(html).map(strip_cf_html));
    }

    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } != 0 {
        push(
            &mut parts,
            "text/plain;charset=utf-8",
            raw(CF_UNICODETEXT).map(utf16_to_utf8),
        );
    } else if unsafe { IsClipboardFormatAvailable(CF_TEXT) } != 0 {
        push(&mut parts, "text/plain", raw(CF_TEXT).map(trim_nul));
    }

    // Only when no PNG was offered: a screenshot usually arrives as a DIB, and
    // storing both would be the same picture twice.
    if !parts.iter().any(|(mime, _)| mime == "image/png")
        && unsafe { IsClipboardFormatAvailable(CF_DIB) } != 0
    {
        push(&mut parts, "image/bmp", raw(CF_DIB).and_then(dib_to_bmp));
    }

    if unsafe { IsClipboardFormatAvailable(CF_HDROP) } != 0 {
        // Reading the drop list needs the shell API; the file names are
        // already in the text representation above for anything that pastes
        // as text, so this is left for a later version rather than guessed at.
    }

    Some(parts)
}

fn push(parts: &mut Vec<(String, Vec<u8>)>, mime: &str, content: Option<Vec<u8>>) {
    if let Some(bytes) = content {
        if !bytes.is_empty() {
            parts.push((mime.to_string(), bytes));
        }
    }
}

/// Whether any offered format says the contents are a secret.
fn concealed() -> bool {
    // The names password managers register on Windows to opt out of clipboard
    // history, including the one Windows' own history honours.
    const HINTS: &[&str] = &[
        "ExcludeClipboardContentFromMonitorProcessing",
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
        "PasswordManagerHint",
    ];

    // `ExcludeClipboardContentFromMonitorProcessing` present at all means
    // exclude; the other two are read as flags whose absence means allow.
    if let Some(format) = lookup("ExcludeClipboardContentFromMonitorProcessing") {
        if unsafe { IsClipboardFormatAvailable(format) } != 0 {
            return true;
        }
    }
    if let Some(format) = lookup("CanIncludeInClipboardHistory") {
        if unsafe { IsClipboardFormatAvailable(format) } != 0 {
            // A DWORD of zero means "do not keep this".
            if let Some(value) = raw(format) {
                if value.len() >= 4
                    && u32::from_le_bytes([value[0], value[1], value[2], value[3]]) == 0
                {
                    return true;
                }
            }
        }
    }
    let _ = HINTS;
    false
}

/// The id of a registered clipboard format, if the clipboard offers it.
fn registered(name: &str) -> Option<u32> {
    let format = lookup(name)?;
    (unsafe { IsClipboardFormatAvailable(format) } != 0).then_some(format)
}

fn lookup(name: &str) -> Option<u32> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let format = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
    (format != 0).then_some(format)
}

/// Copies one clipboard format's bytes out of the global memory it lives in.
fn raw(format: u32) -> Option<Vec<u8>> {
    let handle: HANDLE = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return None;
    }

    let pointer = unsafe { GlobalLock(handle as *mut c_void) };
    if pointer.is_null() {
        return None;
    }
    let size = unsafe { GlobalSize(handle as *mut c_void) };
    let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, size) }.to_vec();
    unsafe { GlobalUnlock(handle as *mut c_void) };

    Some(bytes)
}

/// Turns a UTF-16 clipboard string into UTF-8, dropping the terminator.
fn utf16_to_utf8(bytes: Vec<u8>) -> Vec<u8> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units).into_bytes()
}

fn trim_nul(mut bytes: Vec<u8>) -> Vec<u8> {
    if let Some(end) = bytes.iter().position(|byte| *byte == 0) {
        bytes.truncate(end);
    }
    bytes
}

/// Strips the `CF_HTML` header, which is Windows-specific framing rather than
/// part of the document.
///
/// The header states where the fragment starts and ends as byte offsets into
/// the whole buffer; anything outside them is context the source added.
fn strip_cf_html(bytes: Vec<u8>) -> Vec<u8> {
    let text = String::from_utf8_lossy(&bytes);
    let offset = |key: &str| -> Option<usize> {
        let line = text.lines().find(|line| line.starts_with(key))?;
        line[key.len()..].trim().parse::<usize>().ok()
    };

    match (offset("StartFragment:"), offset("EndFragment:")) {
        (Some(start), Some(end)) if start < end && end <= bytes.len() => bytes[start..end].to_vec(),
        // Without a usable header the whole buffer is the best guess.
        _ => bytes,
    }
}

/// Rebuilds a `.bmp` file from a device-independent bitmap.
///
/// `CF_DIB` is the file without its fourteen-byte header, which is why a
/// screenshot pasted from the clipboard needs one put back before anything
/// else can read it.
fn dib_to_bmp(dib: Vec<u8>) -> Option<Vec<u8>> {
    const FILE_HEADER: usize = 14;
    if dib.len() < 40 {
        return None;
    }

    let header_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) as usize;
    let bit_count = u16::from_le_bytes([dib[14], dib[15]]) as usize;
    let compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    let used_colours = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]) as usize;

    // The palette sits between the info header and the pixels; at 16 and 32
    // bits per pixel a BI_BITFIELDS image has three masks there instead.
    let palette = match bit_count {
        1 | 4 | 8 => {
            let entries = if used_colours == 0 {
                1usize << bit_count
            } else {
                used_colours
            };
            entries * 4
        }
        16 | 32 if compression == 3 => 12,
        _ => 0,
    };

    let pixels_at = FILE_HEADER + header_size + palette;
    let total = FILE_HEADER + dib.len();

    let mut bmp = Vec::with_capacity(total);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(total as u32).to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&(pixels_at as u32).to_le_bytes());
    bmp.extend_from_slice(&dib);

    Some(bmp)
}

/// Counts what the clipboard is offering, for diagnostics only.
#[allow(dead_code)]
fn format_count() -> i32 {
    unsafe { CountClipboardFormats() }
}

#[allow(dead_code)]
fn enumerate(previous: u32) -> u32 {
    unsafe { EnumClipboardFormats(previous) }
}
