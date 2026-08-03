//! Link-time stand-ins for the Node-API C symbols.
//!
//! The `napi` crate imports the `napi_*` functions from the host Node process,
//! so a native test binary has nothing to resolve them against and `cargo test`
//! fails at link time. These stubs provide the symbol definitions for test
//! builds only (they are compiled out of production builds). Pure-Rust unit
//! tests never call them; if one ever is reached, the panic reports the
//! misuse loudly instead of silently corrupting the heap. Signatures mirror
//! `napi-sys` `functions.rs` exactly so a stray call would at least be ABI-correct.

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
pub extern "C" fn napi_call_threadsafe_function(
    _func: napi::sys::napi_threadsafe_function,
    _data: *mut std::ffi::c_void,
    _is_blocking: napi::sys::napi_threadsafe_function_call_mode,
) -> i32 {
    panic!("Node-API symbol `napi_call_threadsafe_function` called from a Rust unit test");
}
