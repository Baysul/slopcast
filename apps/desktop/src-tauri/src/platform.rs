//! Platform introspection: Wayland detection and the dlopen'd EGL GPU
//! probe (D5). The probe is Linux-only: it opens a DRM render node and
//! dlopens `libEGL.so.1`; other platforms report no GPU information (the
//! renderer treats a null result as "unavailable").

use std::ffi::c_void;

#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString, c_char};
#[cfg(target_os = "linux")]
use std::os::raw::c_int;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    pub platform: String,
    pub is_wayland: bool,
    /// Whether a real video capture route exists: Wayland on Linux, WGC on
    /// Windows. X11/macOS have none — the share degrades to audio-only.
    pub video_capture_available: bool,
}

#[must_use]
pub fn is_wayland() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
        || std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty())
}

#[must_use]
pub fn video_capture_available() -> bool {
    cfg!(target_os = "windows") || is_wayland()
}

/// Returns the platform identifier and whether the app runs on Wayland.
#[must_use]
#[tauri::command]
pub fn get_platform_info() -> PlatformInfo {
    PlatformInfo {
        platform: std::env::consts::OS.into(),
        is_wayland: is_wayland(),
        video_capture_available: video_capture_available(),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub egl_vendor: Option<String>,
    pub gl_renderer: Option<String>,
    pub gl_version: Option<String>,
    /// `true` when the GL renderer is a software rasterizer
    /// (`llvmpipe`/`softpipe`/`SwiftShader`).
    pub software_rasterizer: bool,
}

#[cfg(target_os = "linux")]
const EGL_PLATFORM_SURFACELESS_MESA: c_int = 0x31DD;
#[cfg(target_os = "linux")]
const EGL_PLATFORM_X11_KHR: c_int = 0x31D5;
#[cfg(target_os = "linux")]
const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();
#[cfg(target_os = "linux")]
const EGL_VENDOR: c_int = 0x3053;
#[cfg(target_os = "linux")]
const EGL_VERSION: c_int = 0x3054;
#[cfg(target_os = "linux")]
const GL_RENDERER: u32 = 0x1F01;
#[cfg(target_os = "linux")]
const GL_VERSION: u32 = 0x1F02;

#[cfg(target_os = "linux")]
type EglGetPlatformDisplay =
    unsafe extern "C" fn(*const c_void, *mut c_void, *const c_int) -> *mut c_void;
#[cfg(target_os = "linux")]
type EglInitialize = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;
#[cfg(target_os = "linux")]
type EglQueryString = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char;
#[cfg(target_os = "linux")]
type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
#[cfg(target_os = "linux")]
type GlGetString = unsafe extern "C" fn(u32) -> *const c_char;

/// The first `/dev/dri/renderD*` node, if any.
#[cfg(target_os = "linux")]
fn first_render_node() -> Option<PathBuf> {
    std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .min()
}

/// RAII guard for the dlopen'd `libEGL.so.1` handle: every error path closes
/// it exactly once via `Drop`.
#[cfg(target_os = "linux")]
struct EglLib(*mut c_void);

#[cfg(target_os = "linux")]
impl Drop for EglLib {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is the valid handle from dlopen; it is closed
            // exactly once here because the handle is never copied out.
            unsafe { libc::dlclose(self.0) };
        }
    }
}

/// The EGL entry points resolved from the dlopen'd library.
#[cfg(target_os = "linux")]
struct EglFunctions {
    get_platform_display: EglGetPlatformDisplay,
    initialize: EglInitialize,
    query_string: EglQueryString,
    get_proc_address: EglGetProcAddress,
}

/// Opens the first `/dev/dri/renderD*` node read-write and closes it again,
/// proving hardware-accelerated rendering is reachable — mirrors libwebrtc's
/// own render-node acquisition.
#[cfg(target_os = "linux")]
fn open_render_node() -> Result<(), String> {
    let render_node = first_render_node().ok_or_else(|| {
        "no /dev/dri/renderD* render node found — hardware acceleration unavailable".to_string()
    })?;
    let node_cstr = CString::new(render_node.to_string_lossy().as_bytes())
        .map_err(|_| "render node path contains a NUL byte".to_string())?;
    // SAFETY: `node_cstr` is a valid NUL-terminated path; the returned fd (if
    // non-negative) is closed immediately below.
    let fd = unsafe { libc::open(node_cstr.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "failed to open render node {}: {}",
            render_node.display(),
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` is a valid open file descriptor from the open above.
    unsafe { libc::close(fd) };
    Ok(())
}

/// dlopens `libEGL.so.1` (the same pattern libwebrtc uses — no new crate) and
/// resolves the four EGL entry points.
#[cfg(target_os = "linux")]
fn dlopen_egl() -> Result<(EglLib, EglFunctions), String> {
    let lib_cstr = CString::new("libEGL.so.1").map_err(|_| "NUL in library name".to_string())?;
    // SAFETY: dlopen with a valid NUL-terminated path; the handle is owned by
    // the EglLib guard, which closes it exactly once on drop.
    let handle = unsafe { libc::dlopen(lib_cstr.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err("failed to dlopen libEGL.so.1".into());
    }

    let symbol = |name: &str| -> *mut c_void {
        let Ok(sym_cstr) = CString::new(name) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `handle` is the valid dlopen handle above and `sym_cstr` a
        // valid NUL-terminated symbol name.
        unsafe { libc::dlsym(handle, sym_cstr.as_ptr()) }
    };

    let get_platform_display_ptr = symbol("eglGetPlatformDisplay");
    let initialize_ptr = symbol("eglInitialize");
    let query_string_ptr = symbol("eglQueryString");
    let get_proc_address_ptr = symbol("eglGetProcAddress");
    if get_platform_display_ptr.is_null()
        || initialize_ptr.is_null()
        || query_string_ptr.is_null()
        || get_proc_address_ptr.is_null()
    {
        return Err("libEGL.so.1 is missing required symbols".into());
    }
    // SAFETY: the four pointers are non-null dlsym results for the real EGL
    // functions in the dlopen'd library; casting them to their ABI types is
    // the standard dlopen pattern.
    let functions = unsafe {
        EglFunctions {
            get_platform_display: std::mem::transmute::<*mut c_void, EglGetPlatformDisplay>(
                get_platform_display_ptr,
            ),
            initialize: std::mem::transmute::<*mut c_void, EglInitialize>(initialize_ptr),
            query_string: std::mem::transmute::<*mut c_void, EglQueryString>(query_string_ptr),
            get_proc_address: std::mem::transmute::<*mut c_void, EglGetProcAddress>(
                get_proc_address_ptr,
            ),
        }
    };
    Ok((EglLib(handle), functions))
}

/// Creates and initializes a display: surfaceless-Mesa first, X11 as
/// fallback.
#[cfg(target_os = "linux")]
fn create_egl_display(functions: &EglFunctions) -> Result<*mut c_void, String> {
    // SAFETY: `functions.get_platform_display` is a valid EGL function
    // pointer; the display is checked for null below.
    let display = unsafe {
        (functions.get_platform_display)(
            EGL_PLATFORM_SURFACELESS_MESA as *const c_void,
            EGL_DEFAULT_DISPLAY,
            std::ptr::null(),
        )
    };
    let display = if display.is_null() {
        // SAFETY: same contract as the surfaceless attempt above.
        unsafe {
            (functions.get_platform_display)(
                EGL_PLATFORM_X11_KHR as *const c_void,
                EGL_DEFAULT_DISPLAY,
                std::ptr::null(),
            )
        }
    } else {
        display
    };
    if display.is_null() {
        return Err("eglGetPlatformDisplay returned no display".into());
    }
    // SAFETY: `display` is a valid EGL display; the out-pointers may be null
    // (EGL allows it), which the native function tolerates.
    let initialized =
        unsafe { (functions.initialize)(display, std::ptr::null_mut(), std::ptr::null_mut()) };
    if initialized == 0 {
        return Err("eglInitialize failed".into());
    }
    Ok(display)
}

#[cfg(target_os = "linux")]
fn query_string(functions: &EglFunctions, display: *mut c_void, name: c_int) -> Option<String> {
    // SAFETY: `display` is an initialized EGL display; `name` is a valid EGL
    // string attribute.
    let ptr = unsafe { (functions.query_string)(display, name) };
    if ptr.is_null() {
        None
    } else {
        // SAFETY: `ptr` points to a NUL-terminated string owned by EGL.
        Some(
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(target_os = "linux")]
fn gl_string(functions: &EglFunctions, name: u32) -> Option<String> {
    // SAFETY: `functions.get_proc_address` is a valid EGL function pointer
    // and the argument a NUL-terminated C string.
    let get_string_ptr = unsafe { (functions.get_proc_address)(c"glGetString".as_ptr()) };
    if get_string_ptr.is_null() {
        return None;
    }
    // SAFETY: `get_string_ptr` is a non-null EGL-client function pointer.
    let get_string = unsafe { std::mem::transmute::<*mut c_void, GlGetString>(get_string_ptr) };
    // SAFETY: `get_string` is a valid GL function pointer from
    // eglGetProcAddress; calling it with a standard enum is well-defined.
    let ptr = unsafe { get_string(name) };
    if ptr.is_null() {
        None
    } else {
        // SAFETY: `ptr` points to a NUL-terminated string owned by GL.
        Some(
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Mirrors `app.getGPUInfo('complete')` (D5): dlopens `libEGL.so.1`, opens a
/// DRM render node, initializes a surfaceless display and reports
/// vendor/renderer/version plus a software-rasterizer flag.
///
/// # Errors
///
/// Returns an error when no DRM render node exists, `libEGL.so.1` cannot be
/// loaded, or EGL fails to initialize.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn probe_gpu_info() -> Result<GpuInfo, String> {
    open_render_node()?;
    let (_lib, functions) = dlopen_egl()?;
    let display = create_egl_display(&functions)?;

    let egl_vendor = query_string(&functions, display, EGL_VENDOR);
    let _egl_version = query_string(&functions, display, EGL_VERSION);

    let gl_renderer = gl_string(&functions, GL_RENDERER);
    let gl_version = gl_string(&functions, GL_VERSION);

    let software_rasterizer = gl_renderer.as_deref().is_some_and(|renderer| {
        let lower = renderer.to_lowercase();
        lower.contains("llvmpipe") || lower.contains("softpipe") || lower.contains("swiftshader")
    });

    Ok(GpuInfo {
        egl_vendor,
        gl_renderer,
        gl_version,
        software_rasterizer,
    })
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub fn probe_gpu_info() -> Result<GpuInfo, String> {
    Ok(GpuInfo {
        egl_vendor: None,
        gl_renderer: None,
        gl_version: None,
        software_rasterizer: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual GPU probe mirroring `app.getGPUInfo('complete')`. Run with:
    ///
    /// ```sh
    /// cargo test -p slopcast gpu_probe -- --ignored --nocapture
    /// ```
    ///
    /// Linux-only: the probe is a stub elsewhere.
    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "manual diagnostic: requires a DRM render node"]
    fn gpu_probe() {
        match probe_gpu_info() {
            Ok(info) => eprintln!(
                "[probe] vendor={:?} renderer={:?} version={:?} software={}",
                info.egl_vendor, info.gl_renderer, info.gl_version, info.software_rasterizer
            ),
            Err(e) => eprintln!("[probe] failed: {e}"),
        }
    }
}
