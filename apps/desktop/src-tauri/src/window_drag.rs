//! Interactive window moves and resizes handed to the window manager.
//!
//! The frameless titlebar lives inside the web content, so the toolkit never sees the press that
//! would start a native move. Sending `_NET_WM_MOVERESIZE` ourselves lets the window manager apply
//! its own snapping, edge tiling and panel handling instead of a renderer-driven `setPosition`.

use std::os::raw::{c_int, c_long, c_ulong};
use std::ptr;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::Deserialize;
use x11_dl::xlib::{self, Xlib};

#[derive(Clone, Copy, Deserialize)]
pub(crate) enum DragKind {
    Move,
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
}

impl DragKind {
    /// Direction codes from the EWMH `_NET_WM_MOVERESIZE` specification.
    fn direction(self) -> c_long {
        match self {
            Self::NorthWest => 0,
            Self::North => 1,
            Self::NorthEast => 2,
            Self::East => 3,
            Self::SouthEast => 4,
            Self::South => 5,
            Self::SouthWest => 6,
            Self::West => 7,
            Self::Move => 8,
        }
    }
}

const LEFT_BUTTON: c_long = 1;
const SOURCE_APPLICATION: c_long = 1;

fn toplevel_xid(
    window: &tauri::WebviewWindow<tauri_runtime_cef::CefRuntime>,
) -> Result<c_ulong, String> {
    let handle = window.window_handle().map_err(|error| error.to_string())?;

    match handle.as_raw() {
        RawWindowHandle::Xlib(handle) => Ok(handle.window),
        RawWindowHandle::Xcb(handle) => Ok(c_ulong::from(handle.window.get())),
        other => Err(format!("Expected an X11 window handle, got {other:?}")),
    }
}

fn send_move_resize(xid: c_ulong, kind: DragKind) -> Result<(), String> {
    let xlib = Xlib::open().map_err(|error| error.to_string())?;
    // SAFETY: a null name selects the DISPLAY environment variable; the result is checked below.
    let display = unsafe { (xlib.XOpenDisplay)(ptr::null()) };
    if display.is_null() {
        return Err("Cannot open the X11 display".into());
    }

    // SAFETY: `display` is a live connection owned by this function until `XCloseDisplay`, and
    // every out-pointer passed to Xlib refers to a local that outlives the call.
    unsafe {
        let root = (xlib.XDefaultRootWindow)(display);
        let (mut root_x, mut root_y, mut unused): (c_int, c_int, c_int) = (0, 0, 0);
        let (mut unused_window, mut unused_mask) = (0, 0);
        (xlib.XQueryPointer)(
            display,
            root,
            &raw mut unused_window,
            &raw mut unused_window,
            &raw mut root_x,
            &raw mut root_y,
            &raw mut unused,
            &raw mut unused,
            &raw mut unused_mask,
        );
        (xlib.XUngrabPointer)(display, xlib::CurrentTime);

        let message_type = (xlib.XInternAtom)(display, c"_NET_WM_MOVERESIZE".as_ptr(), xlib::False);
        let mut data = xlib::ClientMessageData::new();
        data.set_long(0, c_long::from(root_x));
        data.set_long(1, c_long::from(root_y));
        data.set_long(2, kind.direction());
        data.set_long(3, LEFT_BUTTON);
        data.set_long(4, SOURCE_APPLICATION);
        let mut event = xlib::XEvent {
            client_message: xlib::XClientMessageEvent {
                type_: xlib::ClientMessage,
                serial: 0,
                send_event: xlib::True,
                display,
                window: xid,
                message_type,
                format: 32,
                data,
            },
        };
        (xlib.XSendEvent)(
            display,
            root,
            xlib::False,
            xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask,
            &raw mut event,
        );
        (xlib.XCloseDisplay)(display);
    }

    Ok(())
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned window"
)]
pub(crate) fn begin_window_drag(
    window: tauri::WebviewWindow<tauri_runtime_cef::CefRuntime>,
    kind: DragKind,
) -> Result<(), String> {
    let xid = toplevel_xid(&window)?;

    send_move_resize(xid, kind)
}
