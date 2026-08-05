//! Phase C: dlopen-based EGL/DMA-BUF import + GL readback.
//!
//! Port of libwebrtc's `egl_dmabuf.cc` (`m144_release`): the same dlopen
//! loading order (libEGL, then libGL, then gbm for the display fallback),
//! the same client-extension gates (`EGL_EXT_platform_base`,
//! `EGL_MESA_platform_gbm`, `EGL_KHR_platform_gbm`), the same display
//! fallback chain (Wayland display first, GBM render node second), the same
//! fourcc/GL-format mappings, and the same synchronous per-frame
//! import/readback sequence (`eglCreateImageKHR` → texture → FBO →
//! `glReadPixels`).
//!
//! The EGL context is bound to the thread that constructed `EglDmaBuf` (the
//! capture thread) and every `read_dmabuf` re-arms `eglMakeCurrent` on it,
//! so the instance is never shared across threads.
//!
//! Deviations from the `.cc`: the GBM render node is found by scanning
//! `/dev/dri/renderD*` instead of libdrm's `drmGetDevices2` — the crate has
//! no libdrm dependency and the render nodes are a stable, documented
//! interface. Also, no `dlclose` ever (kept identical to the `.cc`, which
//! documents `crbug.com/1290566`: unloading `libEGL` on `NVidia` crashes the
//! process). And `read_dmabuf` always reads the full negotiated rectangle
//! (`glReadPixels(0, 0, w, h)`): the `.cc`'s sub-rectangle crop parameters are
//! not ported, since the capture engine reads the whole frame every time.

use pipewire::spa::param::video::VideoFormat;
use std::cell::Cell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr;

/// DRM `DRM_FORMAT_MOD_INVALID` from `drm_fourcc.h` — `fourcc_mod_code(NONE,
/// DRM_FORMAT_RESERVED)` = `(1 << 56) - 1`. "No explicit modifier"; buffers
/// carrying it must be imported without modifier attributes.
pub(crate) const DRM_FORMAT_MOD_INVALID: u64 = (1 << 56) - 1;

/// One DMA-BUF plane handed to `read_dmabuf`. `fd` is owned by the caller
/// (duplicated with `F_DUPFD_CLOEXEC` by the capture engine) and must never
/// be closed by `EglDmaBuf`.
pub(crate) struct DmabufPlane {
    pub fd: i32,
    pub offset: u32,
    pub stride: i32,
}

// --- EGL constants (values from mesa's `EGL/eglext.h` + `EGL/egl.h`) ---

const EGL_EXTENSIONS: c_int = 0x3055;
const EGL_HEIGHT: c_int = 0x3056;
const EGL_WIDTH: c_int = 0x3057;
const EGL_NONE: c_int = 0x3038;
const EGL_OPENGL_API: c_uint = 0x30A2;
const EGL_FALSE: c_uint = 0;
const EGL_PLATFORM_GBM_KHR: c_uint = 0x31D7;
const EGL_PLATFORM_WAYLAND_KHR: c_uint = 0x31D8;
const EGL_LINUX_DMA_BUF_EXT: c_uint = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: c_int = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: c_int = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: c_int = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: c_int = 0x3274;
const EGL_DMA_BUF_PLANE1_FD_EXT: c_int = 0x3275;
const EGL_DMA_BUF_PLANE1_OFFSET_EXT: c_int = 0x3276;
const EGL_DMA_BUF_PLANE1_PITCH_EXT: c_int = 0x3277;
const EGL_DMA_BUF_PLANE2_FD_EXT: c_int = 0x3278;
const EGL_DMA_BUF_PLANE2_OFFSET_EXT: c_int = 0x3279;
const EGL_DMA_BUF_PLANE2_PITCH_EXT: c_int = 0x327A;
const EGL_DMA_BUF_PLANE3_FD_EXT: c_int = 0x3440;
const EGL_DMA_BUF_PLANE3_OFFSET_EXT: c_int = 0x3441;
const EGL_DMA_BUF_PLANE3_PITCH_EXT: c_int = 0x3442;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: c_int = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: c_int = 0x3444;
const EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT: c_int = 0x3445;
const EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT: c_int = 0x3446;
const EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT: c_int = 0x3447;
const EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT: c_int = 0x3448;
const EGL_DMA_BUF_PLANE3_MODIFIER_LO_EXT: c_int = 0x3449;
const EGL_DMA_BUF_PLANE3_MODIFIER_HI_EXT: c_int = 0x344A;

// --- GL constants (values from `GLES2/gl2.h` / `GL/gl.h`) ---

const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_NO_ERROR: c_uint = 0;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_RGBA: c_uint = 0x1908;
const GL_NEAREST: c_int = 0x2600;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_WRAP_S: c_uint = 0x2802;
const GL_TEXTURE_WRAP_T: c_uint = 0x2803;
const GL_CLAMP_TO_EDGE: c_int = 0x812F;
const GL_FRAMEBUFFER: c_uint = 0x8D40;
const GL_COLOR_ATTACHMENT0: c_uint = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: c_uint = 0x8CD5;
const GL_BGRA: c_uint = 0x80E1;

// --- DRM fourcc values (from `drm_fourcc.h`) ---

const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241;
const DRM_FORMAT_XBGR8888: u32 = 0x3432_4258;
const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258;

// --- dlopen plumbing ---

/// `eglGetProcAddress` / `glXGetProcAddressARB` — the only two symbols
/// resolved with `dlsym`; every other entry point goes through one of them.
type ResolverFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GetProcAddressFn = ResolverFn;

type BindApiFn = unsafe extern "C" fn(c_uint) -> c_uint;
type CreateContextFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_int) -> *mut c_void;
type DestroyContextFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
type TerminateFn = unsafe extern "C" fn(*mut c_void) -> c_uint;
type CreateImageFn = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    c_uint,
    *mut c_void,
    *const c_int,
) -> *mut c_void;
type DestroyImageFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
type GetErrorFn = unsafe extern "C" fn() -> c_int;
type GetPlatformDisplayFn = unsafe extern "C" fn(c_uint, *mut c_void, *const isize) -> *mut c_void;
type GetPlatformDisplayExtFn =
    unsafe extern "C" fn(c_uint, *mut c_void, *const c_int) -> *mut c_void;
type InitializeFn = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_uint;
type MakeCurrentFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> c_uint;
type QueryStringFn = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char;
type ImageTargetFn = unsafe extern "C" fn(c_uint, *mut c_void);
type QueryFormatsFn = unsafe extern "C" fn(*mut c_void, c_int, *mut c_int, *mut c_int) -> c_uint;
type QueryModifiersFn =
    unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut u64, *mut c_uint, *mut c_int) -> c_uint;

type BindTextureFn = unsafe extern "C" fn(c_uint, c_uint);
type DeleteTexturesFn = unsafe extern "C" fn(c_int, *const c_uint);
type GenTexturesFn = unsafe extern "C" fn(c_int, *mut c_uint);
type GlGetErrorFn = unsafe extern "C" fn() -> c_uint;
type ReadPixelsFn = unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_uint, c_uint, *mut c_void);
type GenFramebuffersFn = unsafe extern "C" fn(c_int, *mut c_uint);
type DeleteFramebuffersFn = unsafe extern "C" fn(c_int, *const c_uint);
type BindFramebufferFn = unsafe extern "C" fn(c_uint, c_uint);
type FramebufferTexture2DFn = unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint, c_int);
type CheckFramebufferStatusFn = unsafe extern "C" fn(c_uint) -> c_uint;
type TexParameteriFn = unsafe extern "C" fn(c_uint, c_uint, c_int);

/// All EGL entry points, resolved through `eglGetProcAddress`. The optional
/// `query` pair exists only when both dma-buf import extensions are present
/// (libwebrtc resolves the two query functions as one unit).
#[derive(Clone, Copy)]
struct EglFns {
    get_proc_address: GetProcAddressFn,
    bind_api: BindApiFn,
    create_context: CreateContextFn,
    destroy_context: DestroyContextFn,
    terminate: TerminateFn,
    create_image: CreateImageFn,
    destroy_image: DestroyImageFn,
    get_error: GetErrorFn,
    get_platform_display: GetPlatformDisplayFn,
    get_platform_display_ext: GetPlatformDisplayExtFn,
    initialize: InitializeFn,
    make_current: MakeCurrentFn,
    query_string: QueryStringFn,
    image_target_texture2d: ImageTargetFn,
}

/// All GL entry points, resolved through `glXGetProcAddressARB`.
#[derive(Clone, Copy)]
struct GlFns {
    bind_texture: BindTextureFn,
    delete_textures: DeleteTexturesFn,
    gen_textures: GenTexturesFn,
    get_error: GlGetErrorFn,
    read_pixels: ReadPixelsFn,
    gen_framebuffers: GenFramebuffersFn,
    delete_framebuffers: DeleteFramebuffersFn,
    bind_framebuffer: BindFramebufferFn,
    framebuffer_texture2d: FramebufferTexture2DFn,
    check_framebuffer_status: CheckFramebufferStatusFn,
    tex_parameteri: TexParameteriFn,
}

/// A GBM device on an opened render node. `gbm_create_device` dups the fd
/// internally; `Drop` runs `gbm_device_destroy` (releasing the internal
/// reference) and then closes the original fd — mirroring the `.cc`
/// destructor (`gbm_device_destroy` + `close(drm_fd_)`).
struct GbmDevice {
    device: *mut c_void,
    destroy: DestroyDeviceFn,
    fd: c_int,
}

impl Drop for GbmDevice {
    fn drop(&mut self) {
        // SAFETY: both the device pointer and the fd are owned by this
        // struct; libgbm stays loaded for the process lifetime (no dlclose).
        unsafe {
            (self.destroy)(self.device);
            libc::close(self.fd);
        }
    }
}

type CreateDeviceFn = unsafe extern "C" fn(c_int) -> *mut c_void;
type DestroyDeviceFn = unsafe extern "C" fn(*mut c_void);

/// Resolves `name` with `dlsym` and transmutes the raw symbol pointer to `T`.
///
/// # Safety
/// `library` must be a valid `dlopen` handle; `name` must be a
/// NUL-terminated symbol name; the resolved symbol must have signature `T`.
unsafe fn dlsym_fn<T: Sized>(library: *mut c_void, name: &CStr) -> Option<T> {
    // SAFETY: `library` is a valid dlopen handle and `name` is
    // NUL-terminated, per the caller's contract.
    let sym = unsafe { libc::dlsym(library, name.as_ptr()) };
    if sym.is_null() {
        None
    } else {
        // SAFETY: the caller guarantees the resolved symbol has signature
        // `T`; fn pointers are pointer-sized with the same alignment, so
        // reading `T` from the raw pointer's bits is sound.
        Some(unsafe { std::ptr::read(std::ptr::from_ref(&sym).cast::<T>()) })
    }
}

/// Resolves `name` through `resolver` (`eglGetProcAddress` or
/// `glXGetProcAddressARB`) and transmutes the result to `T`.
///
/// # Safety
/// `resolver` must be a valid entry-point resolver; `name` must be a
/// NUL-terminated symbol name; the resolved symbol must have signature `T`.
unsafe fn resolve_fn<T: Sized>(resolver: ResolverFn, name: &CStr) -> Option<T> {
    // SAFETY: `resolver` is a valid entry-point resolver and `name` is
    // NUL-terminated, per the caller's contract.
    let sym = unsafe { resolver(name.as_ptr()) };
    if sym.is_null() {
        None
    } else {
        // SAFETY: the caller guarantees the resolved symbol has signature
        // `T`; fn pointers are pointer-sized with the same alignment, so
        // reading `T` from the raw pointer's bits is sound.
        Some(unsafe { std::ptr::read(std::ptr::from_ref(&sym).cast::<T>()) })
    }
}

/// Like `resolve_fn`, but with a readable error instead of `Option`.
///
/// # Safety
/// See `resolve_fn`.
unsafe fn resolve_required<T: Sized>(resolver: ResolverFn, name: &CStr) -> Result<T, String> {
    // SAFETY: forwarded to `resolve_fn`.
    unsafe { resolve_fn::<T>(resolver, name) }
        .ok_or_else(|| format!("{} not found", name.to_string_lossy()))
}

fn dlopen_error() -> String {
    // SAFETY: `dlerror` returns a pointer to a static buffer, or null when
    // there is nothing to report; both are handled.
    let raw = unsafe { libc::dlerror() };
    if raw.is_null() {
        "unknown dynamic linker error".to_string()
    } else {
        // SAFETY: `raw` is non-null and NUL-terminated per `dlerror`'s contract.
        unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned()
    }
}

fn load_egl_fns() -> Result<EglFns, String> {
    // SAFETY: `dlopen` is thread-safe; the handle is intentionally never
    // closed (the .cc documents that unloading libEGL crashes NVidia).
    let lib = unsafe { libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if lib.is_null() {
        return Err(format!("dlopen libEGL.so.1: {}", dlopen_error()));
    }
    // SAFETY: `eglGetProcAddress` is a required EGL entry point.
    let get_proc_address = unsafe { dlsym_fn::<GetProcAddressFn>(lib, c"eglGetProcAddress") }
        .ok_or_else(|| "eglGetProcAddress not found in libEGL.so.1".to_string())?;
    // SAFETY: every resolved symbol is a core EGL entry point whose signature
    // matches the typed alias above.
    let fns = unsafe {
        EglFns {
            get_proc_address,
            bind_api: resolve_required(get_proc_address, c"eglBindAPI")?,
            create_context: resolve_required(get_proc_address, c"eglCreateContext")?,
            destroy_context: resolve_required(get_proc_address, c"eglDestroyContext")?,
            terminate: resolve_required(get_proc_address, c"eglTerminate")?,
            create_image: resolve_required(get_proc_address, c"eglCreateImageKHR")?,
            destroy_image: resolve_required(get_proc_address, c"eglDestroyImageKHR")?,
            get_error: resolve_required(get_proc_address, c"eglGetError")?,
            get_platform_display: resolve_required(get_proc_address, c"eglGetPlatformDisplay")?,
            get_platform_display_ext: resolve_required(
                get_proc_address,
                c"eglGetPlatformDisplayEXT",
            )?,
            initialize: resolve_required(get_proc_address, c"eglInitialize")?,
            make_current: resolve_required(get_proc_address, c"eglMakeCurrent")?,
            query_string: resolve_required(get_proc_address, c"eglQueryString")?,
            image_target_texture2d: resolve_required(
                get_proc_address,
                c"glEGLImageTargetTexture2DOES",
            )?,
        }
    };
    Ok(fns)
}

fn load_gl_fns() -> Result<GlFns, String> {
    // libwebrtc tries libGL.so.1 first, then plain libGL.so.
    let mut loaded = None;
    for name in [c"libGL.so.1", c"libGL.so"] {
        // SAFETY: `dlopen` is thread-safe; the handle is never closed.
        let lib = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if !lib.is_null() {
            loaded = Some(lib);
            break;
        }
    }
    let Some(lib) = loaded else {
        return Err(format!("dlopen libGL.so.1: {}", dlopen_error()));
    };
    // SAFETY: `glXGetProcAddressARB` is the standard GL extension resolver.
    let glx = unsafe { dlsym_fn::<ResolverFn>(lib, c"glXGetProcAddressARB") }
        .ok_or_else(|| "glXGetProcAddressARB not found in libGL.so.1".to_string())?;
    // SAFETY: every resolved symbol is a core GL entry point whose signature
    // matches the typed alias above.
    unsafe {
        Ok(GlFns {
            bind_texture: resolve_required(glx, c"glBindTexture")?,
            delete_textures: resolve_required(glx, c"glDeleteTextures")?,
            gen_textures: resolve_required(glx, c"glGenTextures")?,
            get_error: resolve_required(glx, c"glGetError")?,
            read_pixels: resolve_required(glx, c"glReadPixels")?,
            gen_framebuffers: resolve_required(glx, c"glGenFramebuffers")?,
            delete_framebuffers: resolve_required(glx, c"glDeleteFramebuffers")?,
            bind_framebuffer: resolve_required(glx, c"glBindFramebuffer")?,
            framebuffer_texture2d: resolve_required(glx, c"glFramebufferTexture2D")?,
            check_framebuffer_status: resolve_required(glx, c"glCheckFramebufferStatus")?,
            tex_parameteri: resolve_required(glx, c"glTexParameteri")?,
        })
    }
}

/// Opens the first `/dev/dri/renderD*` node and creates a GBM device on it.
/// Returns `Ok(None)` when no render node exists (the caller then reports the
/// Wayland display failure as fatal).
///
/// Deviation from the .cc (which uses libdrm's `drmGetDevices2`): the render
/// nodes are scanned with `read_dir` and sorted, since `read_dir` order is
/// unspecified and the first node is the documented convention.
fn open_gbm_device() -> Result<Option<GbmDevice>, String> {
    // SAFETY: `dlopen` is thread-safe; the handle is never closed (libgbm
    // stays loaded for the process lifetime, like libEGL/libGL).
    let lib = unsafe { libc::dlopen(c"libgbm.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if lib.is_null() {
        return Err(format!("dlopen libgbm.so.1: {}", dlopen_error()));
    }
    // SAFETY: `gbm_create_device` is the documented libgbm device factory.
    let create = unsafe { dlsym_fn::<CreateDeviceFn>(lib, c"gbm_create_device") }
        .ok_or_else(|| "gbm_create_device not found in libgbm.so.1".to_string())?;
    // SAFETY: `gbm_device_destroy` is the documented libgbm teardown API.
    let destroy = unsafe { dlsym_fn::<DestroyDeviceFn>(lib, c"gbm_device_destroy") }
        .ok_or_else(|| "gbm_device_destroy not found in libgbm.so.1".to_string())?;

    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .map_err(|e| format!("list /dev/dri: {e}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("renderD"))
        .collect();
    nodes.sort();
    let Some(node) = nodes.first() else {
        return Ok(None);
    };
    let path = format!("/dev/dri/{node}\0");
    let path =
        CStr::from_bytes_with_nul(path.as_bytes()).map_err(|_| "invalid render node path")?;
    // SAFETY: `O_RDWR` open of a render node; the mode argument is omitted
    // (creating files is not the intent).
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        return Err(format!(
            "open {path:?}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` is a valid descriptor; `gbm_create_device` takes ownership
    // of it (the device keeps the fd alive until `gbm_device_destroy`).
    let device = unsafe { create(fd) };
    if device.is_null() {
        // SAFETY: ownership did not transfer, so the fd is still ours.
        unsafe { libc::close(fd) };
        return Err(format!("gbm_create_device({path:?}) returned null"));
    }
    Ok(Some(GbmDevice {
        device,
        destroy,
        fd,
    }))
}

/// Splits an `eglQueryString` extension list on spaces.
///
/// # Safety
/// `extensions` must be a valid, NUL-terminated string returned by EGL (or
/// null, which yields an empty list).
unsafe fn extension_list(extensions: *const c_char) -> Vec<String> {
    if extensions.is_null() {
        return Vec::new();
    }
    // SAFETY: guarded by the null check above; the string is NUL-terminated.
    unsafe { CStr::from_ptr(extensions) }
        .to_bytes()
        .split(|b| *b == b' ')
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

fn contains_extension(extensions: &[String], name: &str) -> bool {
    extensions.iter().any(|ext| ext == name)
}

/// `SpaPixelFormatToDrmFormat` from the .cc: SPA BGRA/RGBA/BGRx/RGBx map to
/// the DRM formats with the *reverse* channel order (EGL exposes channels in
/// DRM order).
fn spa_to_drm_fourcc(format: VideoFormat) -> Option<u32> {
    match format {
        VideoFormat::RGBA => Some(DRM_FORMAT_ABGR8888),
        VideoFormat::RGBx => Some(DRM_FORMAT_XBGR8888),
        VideoFormat::BGRA => Some(DRM_FORMAT_ARGB8888),
        VideoFormat::BGRx => Some(DRM_FORMAT_XRGB8888),
        _ => None,
    }
}

/// DRM fourcc values are opaque bit patterns, but EGL attributes are `EGLint`
/// — the driver reinterprets the two's-complement value as `u32`.
#[allow(
    clippy::cast_possible_wrap,
    reason = "drm fourccs are opaque bit patterns; EGL reinterprets the EGLint as u32"
)]
fn fourcc_to_egl_int(fourcc: u32) -> c_int {
    fourcc as c_int
}

/// The GL read format for `glReadPixels`: BGRA-family formats read as
/// `GL_BGRA`, RGBA-family as `GL_RGBA` (the .cc's switch with BGRA default).
fn gl_read_format(format: VideoFormat) -> c_uint {
    match format {
        VideoFormat::RGBA | VideoFormat::RGBx => GL_RGBA,
        _ => GL_BGRA,
    }
}

/// Modifier attributes are 32-bit halves by the extension spec; the driver
/// recombines the bit patterns.
#[allow(
    clippy::cast_possible_truncation,
    reason = "EGL modifier attributes are 32-bit halves by spec; the driver recombines the bit patterns"
)]
fn modifier_halves(modifier: u64) -> (c_int, c_int) {
    ((modifier & 0xFFFF_FFFF) as c_int, (modifier >> 32) as c_int)
}

/// Builds the `eglCreateImageKHR` attribute list for `EGL_LINUX_DMA_BUF_EXT`
/// (the `.cc`'s `ImageFromDmaBuf` attribute sequence, minus the EGL calls so it
/// is unit-testable): width, height, fourcc, then per-plane FD/OFFSET/PITCH
/// plus `MODIFIER_LO/HI` when the modifier is explicit, terminated by `EGL_NONE`.
fn build_image_attrs(
    width: c_int,
    height: c_int,
    fourcc: u32,
    planes: &[DmabufPlane],
    modifier: u64,
) -> Result<Vec<c_int>, String> {
    if planes.is_empty() {
        return Err("no planes to import".into());
    }
    let mut attrs = Vec::with_capacity(3 + 5 * planes.len() + 1);
    attrs.push(EGL_WIDTH);
    attrs.push(width);
    attrs.push(EGL_HEIGHT);
    attrs.push(height);
    attrs.push(EGL_LINUX_DRM_FOURCC_EXT);
    attrs.push(fourcc_to_egl_int(fourcc));
    // EGL supports at most four planes; the .cc hard-codes the same limit.
    let (lo, hi) = if modifier == DRM_FORMAT_MOD_INVALID {
        (0, 0)
    } else {
        modifier_halves(modifier)
    };
    for (index, plane) in planes.iter().take(4).enumerate() {
        let base = match index {
            0 => (
                EGL_DMA_BUF_PLANE0_FD_EXT,
                EGL_DMA_BUF_PLANE0_OFFSET_EXT,
                EGL_DMA_BUF_PLANE0_PITCH_EXT,
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            ),
            1 => (
                EGL_DMA_BUF_PLANE1_FD_EXT,
                EGL_DMA_BUF_PLANE1_OFFSET_EXT,
                EGL_DMA_BUF_PLANE1_PITCH_EXT,
                EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT,
            ),
            2 => (
                EGL_DMA_BUF_PLANE2_FD_EXT,
                EGL_DMA_BUF_PLANE2_OFFSET_EXT,
                EGL_DMA_BUF_PLANE2_PITCH_EXT,
                EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT,
            ),
            _ => (
                EGL_DMA_BUF_PLANE3_FD_EXT,
                EGL_DMA_BUF_PLANE3_OFFSET_EXT,
                EGL_DMA_BUF_PLANE3_PITCH_EXT,
                EGL_DMA_BUF_PLANE3_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE3_MODIFIER_HI_EXT,
            ),
        };
        attrs.push(base.0);
        attrs.push(plane.fd);
        attrs.push(base.1);
        attrs.push(c_int::try_from(plane.offset).map_err(|_| "plane offset does not fit EGLint")?);
        attrs.push(base.2);
        attrs.push(plane.stride);
        if modifier != DRM_FORMAT_MOD_INVALID {
            attrs.push(base.3);
            attrs.push(lo);
            attrs.push(base.4);
            attrs.push(hi);
        }
    }
    attrs.push(EGL_NONE);
    Ok(attrs)
}

fn egl_error_name(error: c_int) -> &'static str {
    match error {
        0x3000 => "EGL_SUCCESS",
        0x3001 => "EGL_NOT_INITIALIZED",
        0x3002 => "EGL_BAD_ACCESS",
        0x3003 => "EGL_BAD_ALLOC",
        0x3004 => "EGL_BAD_ATTRIBUTE",
        0x3005 => "EGL_BAD_CONFIG",
        0x3006 => "EGL_BAD_CONTEXT",
        0x3007 => "EGL_BAD_CURRENT_SURFACE",
        0x3008 => "EGL_BAD_DISPLAY",
        0x3009 => "EGL_BAD_SURFACE",
        0x300A => "EGL_BAD_MATCH",
        0x300B => "EGL_BAD_PARAMETER",
        0x300C => "EGL_BAD_NATIVE_PIXMAP",
        0x300D => "EGL_BAD_NATIVE_WINDOW",
        0x300E => "EGL_CONTEXT_LOST",
        _ => "unknown EGL error",
    }
}

fn gl_error_name(error: c_uint) -> &'static str {
    match error {
        0 => "GL_NO_ERROR",
        0x0500 => "GL_INVALID_ENUM",
        0x0501 => "GL_INVALID_VALUE",
        0x0502 => "GL_INVALID_OPERATION",
        0x0503 => "GL_STACK_OVERFLOW",
        0x0504 => "GL_STACK_UNDERFLOW",
        0x0505 => "GL_OUT_OF_MEMORY",
        _ => "unknown GL error",
    }
}

/// dlopen-based EGL/DMA-BUF importer + GL readback (Phase C).
///
/// Thread affinity: `new()` must run on the thread that will call
/// `read_dmabuf`; the EGL context is bound to that thread for the lifetime
/// of the instance. Never shared across threads.
pub(crate) struct EglDmaBuf {
    display: *mut c_void,
    context: *mut c_void,
    /// Lazily created on the first read; reused for the instance lifetime.
    /// `Cell` because `read_dmabuf` takes `&self` — the capture engine hands
    /// the instance out as a plain shared field, and the single-thread
    /// contract makes interior mutability safe.
    texture: Cell<c_uint>,
    /// Lazily created on the first read; reused for the instance lifetime.
    fbo: Cell<c_uint>,
    /// Whether `EGL_EXT_image_dma_buf_import` was advertised; drives the
    /// modifier-query fallback list.
    import_ext: bool,
    /// The resolved format/modifier query functions, present only when both
    /// dma-buf import extensions were advertised.
    query: Option<(QueryFormatsFn, QueryModifiersFn)>,
    egl: EglFns,
    gl: GlFns,
    /// Render-node fallback device (only when the Wayland display failed).
    gbm: Option<GbmDevice>,
}

impl EglDmaBuf {
    /// Loads `libEGL.so.1`/`libGL.so.1` via `dlopen`, resolves entry points,
    /// gates client extensions, creates the Wayland display (GBM fallback),
    /// initializes, creates a surfaceless context, and resolves the
    /// `eglQueryDmaBufFormatsEXT`/`ModifiersEXT` entry points.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the stack cannot be brought up
    /// (missing libraries, missing extensions, display/context failure).
    pub(crate) fn new() -> Result<Self, String> {
        let egl = load_egl_fns()?;
        let gl = load_gl_fns()?;

        // SAFETY: `EGL_NO_DISPLAY` (null) is the valid target for the client
        // extension query.
        let client_extensions =
            unsafe { extension_list((egl.query_string)(ptr::null_mut(), EGL_EXTENSIONS)) };
        for required in [
            "EGL_EXT_platform_base",
            "EGL_MESA_platform_gbm",
            "EGL_KHR_platform_gbm",
        ] {
            if !contains_extension(&client_extensions, required) {
                return Err(format!("missing required EGL client extension: {required}"));
            }
        }

        // SAFETY: `EGL_DEFAULT_DISPLAY` (null) with no attributes is the
        // documented way to obtain the default Wayland display.
        let display = unsafe {
            (egl.get_platform_display)(EGL_PLATFORM_WAYLAND_KHR, ptr::null_mut(), ptr::null())
        };
        let mut gbm = None;
        let display = if display.is_null() {
            // The .cc logs the failure, then falls back to the first render node.
            let Some(device) = open_gbm_device()? else {
                return Err(format!(
                    "eglGetPlatformDisplay(WAYLAND) failed ({}) and no GBM render node",
                    egl_error_name(unsafe {
                        /* SAFETY: querying the thread-local error state is always safe */
                        (egl.get_error)()
                    })
                ));
            };
            // SAFETY: `device.device` is a live gbm_device; EGL takes it by
            // reference and must outlive it (the device lives on `gbm`).
            let display = unsafe {
                (egl.get_platform_display_ext)(EGL_PLATFORM_GBM_KHR, device.device, ptr::null())
            };
            if display.is_null() {
                return Err(format!(
                    "eglGetPlatformDisplayEXT(GBM) failed: {}",
                    egl_error_name(unsafe {
                        /* SAFETY: querying the thread-local error state is always safe */
                        (egl.get_error)()
                    })
                ));
            }
            gbm = Some(device);
            display
        } else {
            display
        };

        let mut major = 0;
        let mut minor = 0;
        // SAFETY: `display` is valid (checked above); the version out-params
        // are plain locals.
        unsafe {
            if (egl.initialize)(display, &raw mut major, &raw mut minor) == EGL_FALSE {
                return Err(format!(
                    "eglInitialize failed: {}",
                    egl_error_name((egl.get_error)())
                ));
            }
            if (egl.bind_api)(EGL_OPENGL_API) == EGL_FALSE {
                return Err("eglBindAPI(EGL_OPENGL_API) failed".into());
            }
            let context =
                (egl.create_context)(display, ptr::null_mut(), ptr::null_mut(), ptr::null());
            if context.is_null() {
                return Err(format!(
                    "eglCreateContext failed: {}",
                    egl_error_name((egl.get_error)())
                ));
            }

            let display_extensions = extension_list((egl.query_string)(display, EGL_EXTENSIONS));
            let import_ext =
                contains_extension(&display_extensions, "EGL_EXT_image_dma_buf_import");
            let import_modifiers_ext = contains_extension(
                &display_extensions,
                "EGL_EXT_image_dma_buf_import_modifiers",
            );
            // The .cc resolves the two query functions only when both
            // extensions are present; `query` stays `None` otherwise.
            let query = if import_ext && import_modifiers_ext {
                let formats =
                    resolve_fn::<QueryFormatsFn>(egl.get_proc_address, c"eglQueryDmaBufFormatsEXT");
                let modifiers = resolve_fn::<QueryModifiersFn>(
                    egl.get_proc_address,
                    c"eglQueryDmaBufModifiersEXT",
                );
                match (formats, modifiers) {
                    (Some(formats), Some(modifiers)) => Some((formats, modifiers)),
                    _ => None,
                }
            } else {
                None
            };

            Ok(Self {
                display,
                context,
                texture: Cell::new(0),
                fbo: Cell::new(0),
                import_ext,
                query,
                egl,
                gl,
                gbm,
            })
        }
    }

    /// Modifiers usable for `format` — libwebrtc `QueryDmaBufModifiers`
    /// semantics:
    ///
    /// - `[]` when the dma-buf import extensions are absent entirely;
    /// - `[DRM_FORMAT_MOD_INVALID]` when the modifier query path is
    ///   unavailable (import-ext only) or the format is unsupported;
    /// - `[mods..., DRM_FORMAT_MOD_INVALID]` otherwise (modifier-less
    ///   buffers always supported).
    pub(crate) fn query_dma_buf_modifiers(&self, format: VideoFormat) -> Vec<u64> {
        let Some((query_formats, query_modifiers)) = self.query else {
            return if self.import_ext {
                vec![DRM_FORMAT_MOD_INVALID]
            } else {
                Vec::new()
            };
        };
        let Some(drm_format) = spa_to_drm_fourcc(format) else {
            // The four negotiated formats all map; anything else has no
            // modifier support by construction.
            return vec![DRM_FORMAT_MOD_INVALID];
        };
        // SAFETY: `self.display` is valid for the instance lifetime; all
        // out-params are plain locals or vectors sized by the preceding call.
        unsafe {
            let mut count: c_int = 0;
            if query_formats(self.display, 0, ptr::null_mut(), &raw mut count) == EGL_FALSE
                || count <= 0
            {
                return vec![DRM_FORMAT_MOD_INVALID];
            }
            let mut formats = vec![0u32; usize::try_from(count).unwrap_or(0)];
            if query_formats(
                self.display,
                count,
                formats.as_mut_ptr().cast(),
                &raw mut count,
            ) == EGL_FALSE
            {
                return vec![DRM_FORMAT_MOD_INVALID];
            }
            if !formats.contains(&drm_format) {
                return vec![DRM_FORMAT_MOD_INVALID];
            }
            let mut mod_count: c_int = 0;
            if query_modifiers(
                self.display,
                fourcc_to_egl_int(drm_format),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut mod_count,
            ) == EGL_FALSE
                || mod_count <= 0
            {
                return vec![DRM_FORMAT_MOD_INVALID];
            }
            let mut modifiers = vec![0u64; usize::try_from(mod_count).unwrap_or(0)];
            // A failed list query still yields whatever was written; the .cc
            // logs and continues, so the caller keeps trying the rest.
            let _ = query_modifiers(
                self.display,
                fourcc_to_egl_int(drm_format),
                mod_count,
                modifiers.as_mut_ptr(),
                ptr::null_mut(),
                &raw mut mod_count,
            );
            // Modifier-less buffers are always supported (the .cc appends
            // DRM_FORMAT_MOD_INVALID unconditionally).
            modifiers.push(DRM_FORMAT_MOD_INVALID);
            modifiers
        }
    }

    /// Synchronous import + readback (same thread as `new()`): imports the
    /// planes as an EGL image (fourcc from the SPA format, per-plane
    /// FD/OFFSET/PITCH plus `MODIFIER_LO/HI` when `modifier` !=
    /// `DRM_FORMAT_MOD_INVALID`), attaches it to the reused texture + FBO,
    /// verifies `GL_FRAMEBUFFER_COMPLETE`, and reads `width*height*4` bytes
    /// into `out` with the GL-format mapping of `egl_dmabuf.cc`
    /// (BGRA/BGRx -> `GL_BGRA`, RGBA/RGBx -> `GL_RGBA`). The EGL image is
    /// destroyed after the read; texture + FBO are reused across frames.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason on any EGL/GL failure (image create,
    /// framebuffer status, `glGetError` after read).
    pub(crate) fn read_dmabuf(
        &self,
        width: u32,
        height: u32,
        format: VideoFormat,
        planes: &[DmabufPlane],
        modifier: u64,
        out: &mut [u8],
    ) -> Result<(), String> {
        let needed = width as usize * height as usize * 4;
        if out.len() < needed {
            return Err(format!("output buffer too small: {needed} bytes needed"));
        }
        let fourcc = spa_to_drm_fourcc(format).ok_or("unsupported SPA format")?;
        let width = c_int::try_from(width).map_err(|_| "width does not fit EGLint")?;
        let height = c_int::try_from(height).map_err(|_| "height does not fit EGLint")?;
        let attrs = build_image_attrs(width, height, fourcc, planes, modifier)?;
        // SAFETY: `display`/`context` are valid EGL objects owned by this
        // instance; surfaceless binding (`EGL_NO_SURFACE`) is the documented
        // way to use FBO readback without a window surface. The image is
        // always destroyed on every exit path below.
        unsafe {
            if (self.egl.make_current)(self.display, ptr::null_mut(), ptr::null_mut(), self.context)
                == EGL_FALSE
            {
                return Err(format!(
                    "eglMakeCurrent failed: {}",
                    egl_error_name((self.egl.get_error)())
                ));
            }
            let image = (self.egl.create_image)(
                self.display,
                ptr::null_mut(),
                EGL_LINUX_DMA_BUF_EXT,
                ptr::null_mut(),
                attrs.as_ptr(),
            );
            if image.is_null() {
                return Err(format!(
                    "eglCreateImageKHR failed: {}",
                    egl_error_name((self.egl.get_error)())
                ));
            }
            if self.texture.get() == 0 {
                let mut texture = 0;
                (self.gl.gen_textures)(1, &raw mut texture);
                (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
                (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
                (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                self.texture.set(texture);
            }
            (self.gl.bind_texture)(GL_TEXTURE_2D, self.texture.get());
            (self.egl.image_target_texture2d)(GL_TEXTURE_2D, image);
            if self.fbo.get() == 0 {
                let mut fbo = 0;
                (self.gl.gen_framebuffers)(1, &raw mut fbo);
                self.fbo.set(fbo);
            }
            (self.gl.bind_framebuffer)(GL_FRAMEBUFFER, self.fbo.get());
            (self.gl.framebuffer_texture2d)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                self.texture.get(),
                0,
            );
            if (self.gl.check_framebuffer_status)(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE {
                (self.egl.destroy_image)(self.display, image);
                return Err("framebuffer incomplete: DMA-BUF import failed".into());
            }
            (self.gl.read_pixels)(
                0,
                0,
                width,
                height,
                gl_read_format(format),
                GL_UNSIGNED_BYTE,
                out.as_mut_ptr().cast(),
            );
            let error = (self.gl.get_error)();
            (self.egl.destroy_image)(self.display, image);
            if error != GL_NO_ERROR {
                return Err(format!("glReadPixels failed: {}", gl_error_name(error)));
            }
        }
        Ok(())
    }
}

impl Drop for EglDmaBuf {
    fn drop(&mut self) {
        // Teardown mirrors the .cc: gbm device first, then context, then
        // display, then the GL objects. The dlopen'ed libraries are NEVER
        // closed (crbug.com/1290566 — unloading libEGL on NVidia crashes).
        if let Some(gbm) = self.gbm.take() {
            drop(gbm);
        }
        if !self.context.is_null() {
            // SAFETY: `display`/`context` are valid EGL objects owned by this
            // instance; destruction is idempotent after the calls below.
            unsafe { (self.egl.destroy_context)(self.display, self.context) };
        }
        if !self.display.is_null() {
            // SAFETY: `display` is a valid EGLDisplay owned by this instance.
            unsafe { (self.egl.terminate)(self.display) };
        }
        if self.fbo.get() != 0 {
            let fbo = self.fbo.get();
            // SAFETY: `fbo` was created by `glGenFramebuffers` on this
            // thread's context; the name is deleted once.
            unsafe { (self.gl.delete_framebuffers)(1, &raw const fbo) };
        }
        if self.texture.get() != 0 {
            let texture = self.texture.get();
            // SAFETY: `texture` was created by `glGenTextures` on this
            // thread's context; the name is deleted once.
            unsafe { (self.gl.delete_textures)(1, &raw const texture) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::raw::c_float;

    type CreateBoFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> *mut c_void;
    type DestroyBoFn = unsafe extern "C" fn(*mut c_void);
    type GetFdFn = unsafe extern "C" fn(*mut c_void) -> c_int;
    type GetStrideFn = unsafe extern "C" fn(*mut c_void) -> u32;
    type ClearColorFn = unsafe extern "C" fn(c_float, c_float, c_float, c_float);
    type ClearFn = unsafe extern "C" fn(u32);

    #[test]
    fn mod_invalid_matches_drm_fourcc_header() {
        assert_eq!(DRM_FORMAT_MOD_INVALID, (1 << 56) - 1);
        assert_eq!(DRM_FORMAT_MOD_INVALID, 0x00FF_FFFF_FFFF_FFFF);
    }

    #[test]
    fn spa_to_drm_fourcc_maps_the_four_negotiated_formats() {
        assert_eq!(
            spa_to_drm_fourcc(VideoFormat::BGRA),
            Some(DRM_FORMAT_ARGB8888)
        );
        assert_eq!(
            spa_to_drm_fourcc(VideoFormat::BGRx),
            Some(DRM_FORMAT_XRGB8888)
        );
        assert_eq!(
            spa_to_drm_fourcc(VideoFormat::RGBA),
            Some(DRM_FORMAT_ABGR8888)
        );
        assert_eq!(
            spa_to_drm_fourcc(VideoFormat::RGBx),
            Some(DRM_FORMAT_XBGR8888)
        );
    }

    #[test]
    fn spa_to_drm_fourcc_rejects_unknown_formats() {
        assert_eq!(spa_to_drm_fourcc(VideoFormat::Unknown), None);
        assert_eq!(spa_to_drm_fourcc(VideoFormat::NV12), None);
    }

    #[test]
    fn gl_read_format_maps_bgra_family_to_bgra() {
        assert_eq!(gl_read_format(VideoFormat::BGRA), GL_BGRA);
        assert_eq!(gl_read_format(VideoFormat::BGRx), GL_BGRA);
    }

    #[test]
    fn gl_read_format_maps_rgba_family_to_rgba() {
        assert_eq!(gl_read_format(VideoFormat::RGBA), GL_RGBA);
        assert_eq!(gl_read_format(VideoFormat::RGBx), GL_RGBA);
    }

    #[test]
    fn modifier_halves_split_u64() {
        assert_eq!(modifier_halves(0x0000_0001_0000_0002), (2, 1));
        assert_eq!(modifier_halves(u64::MAX), (-1, -1));
    }

    #[test]
    fn image_attrs_single_plane_linear_layout() {
        let attrs = build_image_attrs(
            1920,
            1080,
            DRM_FORMAT_ARGB8888,
            &[DmabufPlane {
                fd: 7,
                offset: 0,
                stride: 7680,
            }],
            DRM_FORMAT_MOD_INVALID,
        )
        .unwrap_or_else(|e| panic!("build attrs: {e}"));
        assert_eq!(
            attrs,
            vec![
                EGL_WIDTH,
                1920,
                EGL_HEIGHT,
                1080,
                EGL_LINUX_DRM_FOURCC_EXT,
                fourcc_to_egl_int(DRM_FORMAT_ARGB8888),
                EGL_DMA_BUF_PLANE0_FD_EXT,
                7,
                EGL_DMA_BUF_PLANE0_OFFSET_EXT,
                0,
                EGL_DMA_BUF_PLANE0_PITCH_EXT,
                7680,
                EGL_NONE,
            ]
        );
    }

    #[test]
    fn image_attrs_include_modifier_halves_when_modifier_is_explicit() {
        let modifier = 0x0000_0001_0000_0002;
        let attrs = build_image_attrs(
            16,
            16,
            DRM_FORMAT_ARGB8888,
            &[DmabufPlane {
                fd: 3,
                offset: 4096,
                stride: 64,
            }],
            modifier,
        )
        .unwrap_or_else(|e| panic!("build attrs: {e}"));
        assert_eq!(*attrs.last().unwrap_or(&0), EGL_NONE);
        let lo = attrs
            .iter()
            .position(|a| *a == EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT)
            .unwrap_or_else(|| panic!("missing modifier-lo attribute"));
        assert_eq!(attrs[lo + 1], 2);
        assert_eq!(attrs[lo + 3], 1);
    }

    #[test]
    fn image_attrs_reject_empty_plane_lists() {
        assert!(build_image_attrs(10, 10, DRM_FORMAT_ARGB8888, &[], 0).is_err());
    }

    #[test]
    fn image_attrs_truncate_at_four_planes() {
        let planes = (0..6)
            .map(|fd| DmabufPlane {
                fd,
                offset: 0,
                stride: 64,
            })
            .collect::<Vec<_>>();
        let attrs = build_image_attrs(16, 16, DRM_FORMAT_ARGB8888, &planes, DRM_FORMAT_MOD_INVALID)
            .unwrap_or_else(|e| panic!("build attrs: {e}"));
        // 3 base attr-value pairs + 4 planes * 3 attr-value pairs + the
        // EGL_NONE terminator; modifier attrs are omitted for
        // DRM_FORMAT_MOD_INVALID.
        assert_eq!(attrs.len(), 3 * 2 + 4 * 3 * 2 + 1);
        assert_eq!(*attrs.last().unwrap_or(&0), EGL_NONE);
        for fd in 0..4 {
            assert!(attrs.contains(&fd), "plane fd {fd} missing");
        }
        assert!(!attrs.contains(&4), "sixth plane must be truncated");
        assert!(!attrs.contains(&5), "sixth plane must be truncated");
    }

    #[test]
    fn egl_error_name_covers_common_codes() {
        assert_eq!(egl_error_name(0x3008), "EGL_BAD_DISPLAY");
        assert_eq!(egl_error_name(0x3004), "EGL_BAD_ATTRIBUTE");
        assert_eq!(egl_error_name(0x1234), "unknown EGL error");
    }

    #[test]
    fn gl_error_name_covers_common_codes() {
        assert_eq!(gl_error_name(0x0501), "GL_INVALID_VALUE");
        assert_eq!(gl_error_name(0x0505), "GL_OUT_OF_MEMORY");
        assert_eq!(gl_error_name(0x1234), "unknown GL error");
    }

    #[test]
    fn fourcc_to_egl_int_round_trips_through_u32() {
        assert_eq!(
            fourcc_to_egl_int(DRM_FORMAT_ARGB8888).cast_unsigned(),
            DRM_FORMAT_ARGB8888
        );
        assert_eq!(
            fourcc_to_egl_int(DRM_FORMAT_ABGR8888).cast_unsigned(),
            DRM_FORMAT_ABGR8888
        );
    }

    /// Manual probe (gate 3b of SCREEN-CAPTURE-INHOUSE.md §10): imports a
    /// self-made linear ARGB8888 DMA-BUF filled with a known red pixel and
    /// runs it through the exact EGL import + `glReadPixels` path used by
    /// the capture engine, asserting the readback byte order. A swap here
    /// would silently tint every captured frame (blue/red cast). Run with:
    ///
    /// ```sh
    /// cargo test -p native-rust --release -- --ignored dmabuf_readback_probe --nocapture
    /// ```
    #[test]
    #[ignore = "manual probe: requires a live EGL/Wayland session"]
    fn dmabuf_readback_probe() {
        const W: u32 = 64;
        const H: u32 = 64;
        const GBM_FORMAT_ARGB8888: u32 = 0x3432_5241;
        const GBM_BO_USE_LINEAR: u32 = 1 << 4;

        // SAFETY: `dlopen` is thread-safe; the handle is never closed.
        let gbm =
            unsafe { libc::dlopen(c"libgbm.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        assert!(!gbm.is_null(), "dlopen libgbm.so.1");
        // SAFETY: the resolved symbols are the documented libgbm entry points.
        let (create_bo, destroy_bo, get_fd, get_stride) = unsafe {
            (
                dlsym_fn::<CreateBoFn>(gbm, c"gbm_bo_create")
                    .unwrap_or_else(|| panic!("gbm_bo_create")),
                dlsym_fn::<DestroyBoFn>(gbm, c"gbm_bo_destroy")
                    .unwrap_or_else(|| panic!("gbm_bo_destroy")),
                dlsym_fn::<GetFdFn>(gbm, c"gbm_bo_get_fd")
                    .unwrap_or_else(|| panic!("gbm_bo_get_fd")),
                dlsym_fn::<GetStrideFn>(gbm, c"gbm_bo_get_stride")
                    .unwrap_or_else(|| panic!("gbm_bo_get_stride")),
            )
        };
        // SAFETY: `dlopen` is thread-safe; the handle is never closed.
        let gl_lib =
            unsafe { libc::dlopen(c"libGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        assert!(!gl_lib.is_null(), "dlopen libGL.so.1");
        // SAFETY: glXGetProcAddressARB is the standard GL extension resolver.
        let glx = unsafe { dlsym_fn::<ResolverFn>(gl_lib, c"glXGetProcAddressARB") }
            .unwrap_or_else(|| panic!("glXGetProcAddressARB"));
        // SAFETY: the resolved symbols are core GL entry points.
        let (gl_clear_color, gl_clear) = unsafe {
            (
                resolve_required::<ClearColorFn>(glx, c"glClearColor")
                    .unwrap_or_else(|e| panic!("glClearColor: {e}")),
                resolve_required::<ClearFn>(glx, c"glClear")
                    .unwrap_or_else(|e| panic!("glClear: {e}")),
            )
        };

        let egl = EglDmaBuf::new().unwrap_or_else(|e| panic!("EglDmaBuf::new: {e}"));
        let Some(gbm_device) = open_gbm_device().unwrap_or_else(|e| panic!("open_gbm_device: {e}"))
        else {
            panic!("no GBM render node");
        };
        // SAFETY: `gbm_device.device` is a live gbm_device; a linear buffer
        // is the simplest import target.
        let bo = unsafe {
            create_bo(
                gbm_device.device,
                W,
                H,
                GBM_FORMAT_ARGB8888,
                GBM_BO_USE_LINEAR,
            )
        };
        assert!(!bo.is_null(), "gbm_bo_create failed");
        // SAFETY: `gbm_bo_get_fd` returns a live fd owned by the caller.
        let fd = unsafe { get_fd(bo) };
        assert!(fd >= 0, "gbm_bo_get_fd failed");
        // SAFETY: `bo` is a live gbm buffer created above.
        let stride = unsafe { get_stride(bo) };
        assert_eq!(stride, W * 4, "linear stride must equal width * 4");

        // Fill the buffer through the GPU itself: import it, attach it to an
        // FBO and clear to red — the same driver path KWin uses to write
        // screencast buffers, so CPU staging quirks cannot distort the test.
        // SAFETY: the probe's EglDmaBuf owns the display/context; the bo is a
        // live linear ARGB8888 buffer of W x H.
        let (texture, fbo) =
            unsafe { fill_red_via_gl(&egl, fd, stride, W, H, gl_clear_color, gl_clear) };

        // Now run the production readback path on the same buffer.
        let mut out = vec![0u8; (W * H * 4) as usize];
        let planes = [DmabufPlane {
            fd,
            offset: 0,
            stride: i32::try_from(stride).unwrap_or(0),
        }];
        egl.read_dmabuf(
            W,
            H,
            VideoFormat::BGRA,
            &planes,
            DRM_FORMAT_MOD_INVALID,
            &mut out,
        )
        .unwrap_or_else(|e| panic!("read_dmabuf: {e}"));

        for (label, i) in [
            ("p0", 0usize),
            ("p1", 4),
            ("mid", ((H / 2) * stride + (W / 2) * 4) as usize),
            ("last", (W * H * 4 - 4) as usize),
        ] {
            eprintln!(
                "[probe] {label}: {:02X} {:02X} {:02X} {:02X}",
                out[i],
                out[i + 1],
                out[i + 2],
                out[i + 3]
            );
        }

        // SAFETY: the GL names and the bo are ours to release.
        unsafe {
            (egl.gl.delete_framebuffers)(1, &raw const fbo);
            (egl.gl.delete_textures)(1, &raw const texture);
            destroy_bo(bo);
        }

        let actual = [out[0], out[1], out[2], out[3]];
        let expected = [0x00u8, 0x00, 0xFF, 0xFF];
        eprintln!(
            "[probe] red dma-buf readback: first pixel = {actual:02X?}, expected {expected:02X?}"
        );
        assert_eq!(actual, expected, "EGL readback channel order is swapped");
    }

    /// Imports `fd` as an EGL image, attaches it to a texture + FBO and
    /// clears it to opaque red via `glClear` (the same GPU write path
    /// `KWin`
    /// uses for screencast buffers). Returns the created GL names.
    ///
    /// # Safety
    ///
    /// `egl` must own a live display/context on the current thread; `fd`
    /// must be a live, linear `ARGB8888` dma-buf of `width` x `height`
    /// with the given `stride`. The image is destroyed before returning.
    unsafe fn fill_red_via_gl(
        egl: &EglDmaBuf,
        fd: c_int,
        stride: u32,
        width: u32,
        height: u32,
        gl_clear_color: ClearColorFn,
        gl_clear: ClearFn,
    ) -> (u32, u32) {
        // SAFETY: the caller guarantees `egl` owns a live display/context on
        // this thread and `fd` is a live linear `ARGB8888` dma-buf of
        // `width` x `height`; the image is destroyed on every exit path.
        unsafe {
            const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

            (egl.egl.make_current)(egl.display, ptr::null_mut(), ptr::null_mut(), egl.context);
            let attrs = build_image_attrs(
                i32::try_from(width).unwrap_or(0),
                i32::try_from(height).unwrap_or(0),
                DRM_FORMAT_ARGB8888,
                &[DmabufPlane {
                    fd,
                    offset: 0,
                    stride: i32::try_from(stride).unwrap_or(0),
                }],
                DRM_FORMAT_MOD_INVALID,
            )
            .unwrap_or_else(|e| panic!("build_image_attrs: {e}"));
            let image = (egl.egl.create_image)(
                egl.display,
                ptr::null_mut(),
                EGL_LINUX_DMA_BUF_EXT,
                ptr::null_mut(),
                attrs.as_ptr(),
            );
            assert!(!image.is_null(), "eglCreateImageKHR failed");

            let (mut texture, mut fbo) = (0u32, 0u32);
            (egl.gl.gen_textures)(1, &raw mut texture);
            (egl.gl.bind_texture)(GL_TEXTURE_2D, texture);
            (egl.egl.image_target_texture2d)(GL_TEXTURE_2D, image);
            (egl.gl.gen_framebuffers)(1, &raw mut fbo);
            (egl.gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo);
            (egl.gl.framebuffer_texture2d)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                texture,
                0,
            );
            assert_eq!(
                (egl.gl.check_framebuffer_status)(GL_FRAMEBUFFER),
                GL_FRAMEBUFFER_COMPLETE,
                "framebuffer incomplete"
            );
            gl_clear_color(1.0, 0.0, 0.0, 1.0);
            gl_clear(GL_COLOR_BUFFER_BIT);
            (egl.egl.destroy_image)(egl.display, image);
            (texture, fbo)
        }
    }
}
