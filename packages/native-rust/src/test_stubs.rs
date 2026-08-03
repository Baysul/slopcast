//! Link-time stand-ins for the Node-API C symbols.
//!
//! The `napi` crate imports the `napi_*` functions from the host Node process,
//! so a native test binary has nothing to resolve them against and `cargo test`
//! fails at link time. These stubs provide the symbol definitions for test
//! builds only (they are compiled out of production builds). Pure-Rust unit
//! tests never call them; if one ever is reached, the panic reports the
//! misuse loudly instead of silently corrupting the heap. Signatures mirror
//! `napi-sys` `functions.rs` exactly so a stray call would at least be ABI-correct.

// `napi-sys` declares each import with the exact parameter list below.
#[unsafe(no_mangle)]
pub extern "C" fn napi_create_string_utf8(
    _env: napi::sys::napi_env,
    _str_: *const std::ffi::c_char,
    _length: isize,
    _result: *mut napi::sys::napi_value,
) -> i32 {
    panic!("Node-API symbol `napi_create_string_utf8` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_create_error(
    _env: napi::sys::napi_env,
    _code: napi::sys::napi_value,
    _msg: napi::sys::napi_value,
    _result: *mut napi::sys::napi_value,
) -> i32 {
    panic!("Node-API symbol `napi_create_error` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_set_named_property(
    _env: napi::sys::napi_env,
    _object: napi::sys::napi_value,
    _utf8name: *const std::ffi::c_char,
    _value: napi::sys::napi_value,
) -> i32 {
    panic!("Node-API symbol `napi_set_named_property` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_delete_reference(
    _env: napi::sys::napi_env,
    _ref_: napi::sys::napi_ref,
) -> i32 {
    panic!("Node-API symbol `napi_delete_reference` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_reference_unref(
    _env: napi::sys::napi_env,
    _ref_: napi::sys::napi_ref,
    _result: *mut u32,
) -> i32 {
    panic!("Node-API symbol `napi_reference_unref` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_get_reference_value(
    _env: napi::sys::napi_env,
    _ref_: napi::sys::napi_ref,
    _result: *mut napi::sys::napi_value,
) -> i32 {
    panic!("Node-API symbol `napi_get_reference_value` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_throw(_env: napi::sys::napi_env, _error: napi::sys::napi_value) -> i32 {
    panic!("Node-API symbol `napi_throw` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_is_error(
    _env: napi::sys::napi_env,
    _value: napi::sys::napi_value,
    _result: *mut bool,
) -> i32 {
    panic!("Node-API symbol `napi_is_error` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_is_exception_pending(_env: napi::sys::napi_env, _result: *mut bool) -> i32 {
    panic!("Node-API symbol `napi_is_exception_pending` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_get_and_clear_last_exception(
    _env: napi::sys::napi_env,
    _result: *mut napi::sys::napi_value,
) -> i32 {
    panic!("Node-API symbol `napi_get_and_clear_last_exception` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_call_threadsafe_function(
    _func: napi::sys::napi_threadsafe_function,
    _data: *mut std::ffi::c_void,
    _is_blocking: napi::sys::napi_threadsafe_function_call_mode,
) -> i32 {
    panic!("Node-API symbol `napi_call_threadsafe_function` called from a Rust unit test");
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_release_threadsafe_function(
    _func: napi::sys::napi_threadsafe_function,
    _mode: napi::sys::napi_threadsafe_function_release_mode,
) -> i32 {
    panic!("Node-API symbol `napi_release_threadsafe_function` called from a Rust unit test");
}
