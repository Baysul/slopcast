use tauri_runtime_cef::{Cef, RemoteDebugging};

pub fn with_remote_debugging(cef: Cef) -> Cef {
    cef.remote_debugging(RemoteDebugging::Port {
        port: 9222,
        allowed_origins: Vec::new(),
    })
}
