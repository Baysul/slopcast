use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use napi::{Either, Result as NapiResult};
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::{
    IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::core::Interface;

use crate::{AudioApp, AudioAppLevel};

fn napi_err(context: &str, e: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(format!("{context}: {e}"))
}

fn snapshot_process_names() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    // SAFETY: no pointers involved; an invalid snapshot returns an Err, handled
    // by the early return.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return map;
    };
    // SAFETY: all-zero is a valid PROCESSENTRY32W bit pattern; dwSize is set
    // before the struct is passed to any API.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `snapshot` is a valid open handle and `entry` a valid out-buffer
    // with dwSize initialized.
    if unsafe { Process32FirstW(snapshot, &mut entry).is_ok() } {
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            map.insert(entry.th32ProcessID, name);
            // SAFETY: `snapshot` is still valid and `entry` a valid out-buffer.
            if unsafe { Process32NextW(snapshot, &mut entry).is_err() } {
                break;
            }
        }
    }
    // SAFETY: `snapshot` is a valid open handle, closed exactly once here.
    let _ = unsafe { CloseHandle(snapshot) };
    map
}

pub(crate) fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    // SAFETY: standard per-thread COM initialization; balanced below on success.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let com_ok = hr == S_OK;
    let result = enumerate_audio_apps();
    if com_ok {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    result
}

fn enumerate_audio_apps() -> NapiResult<Vec<AudioApp>> {
    let names = snapshot_process_names();
    // SAFETY: COM was initialized on this thread by the caller. Every interface
    // obtained here is valid, used only on this thread, and released on drop.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| napi_err("CoCreateInstance(MMDeviceEnumerator)", e))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| napi_err("GetDefaultAudioEndpoint", e))?;
        let manager: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| napi_err("Activate(IAudioSessionManager2)", e))?;
        let sessions = manager
            .GetSessionEnumerator()
            .map_err(|e| napi_err("GetSessionEnumerator", e))?;
        let count = sessions
            .GetCount()
            .map_err(|e| napi_err("GetSessionEnumerator::GetCount", e))?;

        let mut apps = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..count {
            let Ok(control) = sessions.GetSession(i) else {
                continue;
            };
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            if control2.IsSystemSoundsSession() == S_OK {
                continue;
            }
            let Ok(pid) = control2.GetProcessId() else {
                continue;
            };
            if pid == 0 || !seen.insert(pid) {
                continue;
            }
            let name = names
                .get(&pid)
                .cloned()
                .unwrap_or_else(|| format!("Process {pid}"));
            apps.push(AudioApp {
                id: pid as i32,
                name,
                process_id: pid as i32,
                bundle_id: None,
                window_title: None,
                client_id: None,
                media_title: None,
            });
        }
        Ok(apps)
    }
}

// Audio capture on Windows is not wired to any consumer: WASAPI loopback
// samples would need a virtual device or IPC path to reach the renderer, and
// neither exists yet (AGENTS.md Task 4). Start reports an explicit error
// instead of pretending to capture.
pub(crate) fn start_audio_capture(_: &Either<String, i32>) -> NapiResult<bool> {
    Err(napi::Error::from_reason(
        "Per-application audio capture is not yet implemented on Windows",
    ))
}

pub(crate) fn stop_audio_capture() -> bool {
    true
}

pub(crate) fn is_audio_capture_active() -> bool {
    false
}

pub(crate) fn switch_audio_capture(_: &Either<String, i32>) -> NapiResult<bool> {
    Err(napi::Error::from_reason(
        "Audio target switching is not yet supported on Windows",
    ))
}

pub(crate) fn get_capture_context() -> NapiResult<crate::CaptureContext> {
    Err(napi::Error::from_reason(
        "Capture context is only available on Linux",
    ))
}

pub(crate) fn resolve_audio_app_for_x11_window(_: u32) -> Option<AudioApp> {
    None
}

pub(crate) fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    None
}

pub(crate) fn start_audio_metering() -> NapiResult<bool> {
    Ok(false)
}

pub(crate) fn stop_audio_metering() -> bool {
    true
}

pub(crate) fn get_audio_levels() -> Vec<AudioAppLevel> {
    Vec::new()
}
