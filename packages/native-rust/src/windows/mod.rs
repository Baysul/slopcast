use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Either, Result as NapiResult};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, S_OK, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{CreateEventA, SetEvent, WaitForMultipleObjects};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Error, HRESULT, IUnknown, Interface, PCSTR, Ref, implement};

use crate::{AudioApp, AudioAppLevel};

const PROCESS_LOOPBACK_MIN_BUILD: u32 = 22621;
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
const BLOCK_ALIGN: u16 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    ProcessLoopback,
    SystemLoopback,
}

struct WasapiState {
    is_active: bool,
    target_pid: Option<u32>,
    stop_event_raw: Option<usize>,
    capture_thread: Option<std::thread::JoinHandle<()>>,
    mode: Option<CaptureMode>,
}

static WASAPI_STATE: Mutex<Option<WasapiState>> = Mutex::new(None);

static AUDIO_DATA_CALLBACK: Mutex<
    Option<std::sync::Arc<ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>>,
> = Mutex::new(None);

fn napi_err(context: &str, e: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(format!("{context}: {e}"))
}

fn os_build_number() -> Option<u32> {
    // SAFETY: all-zero is a valid OSVERSIONINFOW bit pattern.
    let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
    info.dwOSVersionInfoSize = size_of::<OSVERSIONINFOW>() as u32;
    // SAFETY: `info` is a valid, correctly sized out-buffer that outlives the call.
    let status = unsafe { RtlGetVersion(&mut info) };
    status.is_ok().then_some(info.dwBuildNumber)
}

fn snapshot_process_names() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    // SAFETY: INVALID_HANDLE_VALUE is returned on failure and checked below.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return map;
    };
    // SAFETY: all-zero is a valid PROCESSENTRY32W bit pattern.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `snapshot` is a valid open handle and `entry` a correctly sized buffer.
    if unsafe { Process32FirstW(snapshot, &mut entry).is_ok() } {
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            map.insert(entry.th32ProcessID, name);
            // SAFETY: same invariants as Process32FirstW; failure signals end of enumeration.
            if unsafe { Process32NextW(snapshot, &mut entry).is_err() } {
                break;
            }
        }
    }
    // SAFETY: `snapshot` was successfully created above and is closed exactly once here.
    let _ = unsafe { CloseHandle(snapshot) };
    map
}

pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
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

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    state: Arc<(Mutex<bool>, Condvar)>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _activateoperation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let (lock, cvar) = &*self.state;
        let mut completed = lock
            .lock()
            .map_err(|e| Error::new(E_FAIL, format!("Activation mutex poisoned: {e}")))?;
        *completed = true;
        cvar.notify_one();
        Ok(())
    }
}

fn wait_for_activation(setup: &Arc<(Mutex<bool>, Condvar)>) -> windows::core::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let (lock, cvar) = &**setup;
    let mut completed = lock
        .lock()
        .map_err(|e| Error::new(E_FAIL, format!("Activation mutex poisoned: {e}")))?;
    while !*completed {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::new(
                HRESULT::from_win32(1460),
                "Timed out waiting for activation",
            ));
        }
        let (guard, _) = cvar
            .wait_timeout(completed, remaining)
            .map_err(|e| Error::new(E_FAIL, format!("Activation condvar poisoned: {e}")))?;
        completed = guard;
    }
    Ok(())
}

fn activate_process_loopback(target_pid: u32) -> windows::core::Result<IAudioClient> {
    // Heap-allocated so the pointee address stays valid even if the async
    // activation outlives this function (see the forget below).
    let mut activation_params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: target_pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    });

    let raw_prop = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&mut *activation_params as *mut AUDIOCLIENT_ACTIVATION_PARAMS)
                            .cast::<u8>(),
                    },
                },
            }),
        },
    };

    let setup = Arc::new((Mutex::new(false), Condvar::new()));
    let callback: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        state: setup.clone(),
    }
    .into();
    // SAFETY: `raw_prop` points to the heap-allocated `activation_params`. On
    // success the completion callback has fired before we return, so COM no
    // longer references the params; on every error path below the Box is
    // deliberately leaked so a late-completing operation can never read freed
    // memory.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw_prop as *const PROPVARIANT),
            &callback,
        )?
    };

    let wait_result = wait_for_activation(&setup);
    if wait_result.is_err() {
        // The operation may still dereference the params after we return; leak
        // them deliberately. Bounded one-time cost on a path that indicates the
        // audio subsystem is already unresponsive.
        std::mem::forget(activation_params);
    }
    wait_result?;

    let mut audio_client: Option<IUnknown> = None;
    let mut result = HRESULT(0);
    // SAFETY: called exactly once, after activation completed; both out-params
    // are valid and outlive the call.
    unsafe { operation.GetActivateResult(&mut result, &mut audio_client) }?;
    result.ok()?;
    let unknown =
        audio_client.ok_or_else(|| Error::new(E_FAIL, "Activation returned no interface"))?;
    unknown.cast::<IAudioClient>()
}

fn activate_system_loopback() -> windows::core::Result<(IAudioClient, *mut WAVEFORMATEX)> {
    // SAFETY: COM was initialized on this thread by the caller. The returned
    // interface and mix-format pointer are owned by the caller.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix_format = client.GetMixFormat()?;
        Ok((client, mix_format))
    }
}

fn make_loopback_format() -> WAVEFORMATEXTENSIBLE {
    const CHANNELS: u16 = 2;
    const SAMPLE_RATE: u32 = 48_000;
    const BITS_PER_SAMPLE: u16 = 32;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(BLOCK_ALIGN),
            nBlockAlign: BLOCK_ALIGN,
            wBitsPerSample: BITS_PER_SAMPLE,
            cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: BITS_PER_SAMPLE,
        },
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        dwChannelMask: 0x3,
    }
}

struct CaptureSession {
    client: IAudioClient,
    capture_client: IAudioCaptureClient,
    audio_event: HANDLE,
}

impl CaptureSession {
    fn drain_packets(
        &self,
        tsfn: Option<&ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>,
    ) -> Result<(), String> {
        // SAFETY: `capture_client` is a valid interface owned by this session;
        // every buffer obtained is released before the next iteration.
        let Some(tsfn) = tsfn else {
            unsafe {
                loop {
                    let packet_frames = self
                        .capture_client
                        .GetNextPacketSize()
                        .map_err(|e| format!("GetNextPacketSize: {e}"))?;
                    if packet_frames == 0 {
                        return Ok(());
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    self.capture_client
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                        .map_err(|e| format!("GetBuffer: {e}"))?;
                    self.capture_client
                        .ReleaseBuffer(frames)
                        .map_err(|e| format!("ReleaseBuffer: {e}"))?;
                }
            }
        };
        // Accumulate all available packets into one buffer to minimise IPC.
        let mut all_frames = Vec::new();
        // SAFETY: `capture_client` is a valid interface owned by this session;
        // every buffer obtained is released before the next iteration.
        unsafe {
            loop {
                let packet_frames = self
                    .capture_client
                    .GetNextPacketSize()
                    .map_err(|e| format!("GetNextPacketSize: {e}"))?;
                if packet_frames == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                self.capture_client
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| format!("GetBuffer: {e}"))?;
                let frame_bytes = frames as usize * BLOCK_ALIGN as usize;
                let slice = std::slice::from_raw_parts(data, frame_bytes);
                all_frames.extend_from_slice(slice);
                self.capture_client
                    .ReleaseBuffer(frames)
                    .map_err(|e| format!("ReleaseBuffer: {e}"))?;
            }
        }
        if !all_frames.is_empty() {
            // Pack i16 LE samples into one binary buffer to avoid per-sample
            // JS number boxing across the N-API boundary.
            let mut i16_bytes = Vec::with_capacity(all_frames.len() / 2);
            for chunk in all_frames.chunks_exact(4) {
                let f = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                i16_bytes.extend_from_slice(
                    &((f.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes(),
                );
            }
            let _ = tsfn.call(
                Ok(napi::bindgen_prelude::Buffer::from(i16_bytes)),
                ThreadsafeFunctionCallMode::NonBlocking,
            );
        }
        Ok(())
    }

    fn run(
        &self,
        stop_event: HANDLE,
        tsfn: Option<&ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>,
    ) -> Result<(), String> {
        let handles = [stop_event, self.audio_event];
        // SAFETY: both handles are valid for the duration of this call — the
        // stop event is owned by the capture state, the audio event by `self`.
        unsafe {
            loop {
                let ev = WaitForMultipleObjects(&handles, false, u32::MAX);
                if ev.0 == WAIT_OBJECT_0.0 {
                    return Ok(());
                }
                if ev.0 == WAIT_OBJECT_0.0 + 1 {
                    self.drain_packets(tsfn)?;
                } else {
                    return Err("WaitForMultipleObjects failed".into());
                }
            }
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        // SAFETY: `self.client` is a valid IAudioClient (never moved out of this
        // struct) and `self.audio_event` a valid HANDLE created in
        // build_capture_session; both are still alive here and used exactly once.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.audio_event);
        }
    }
}

fn build_capture_session(target_pid: u32) -> Result<(CaptureSession, CaptureMode), String> {
    let process_loopback_supported =
        os_build_number().is_some_and(|b| b >= PROCESS_LOOPBACK_MIN_BUILD);

    let (client, mode, mix_format_ptr) = if process_loopback_supported {
        match activate_process_loopback(target_pid) {
            Ok(client) => (client, CaptureMode::ProcessLoopback, None),
            Err(_) => {
                let (c, fmt) = activate_system_loopback()
                    .map_err(|e| format!("System loopback fallback: {e}"))?;
                (c, CaptureMode::SystemLoopback, Some(fmt))
            }
        }
    } else {
        let (c, fmt) =
            activate_system_loopback().map_err(|e| format!("System loopback activation: {e}"))?;
        (c, CaptureMode::SystemLoopback, Some(fmt))
    };

    let explicit_format = make_loopback_format();
    let format_ptr: *const WAVEFORMATEX = match mix_format_ptr {
        Some(ptr) => ptr.cast_const(),
        None => &explicit_format.Format,
    };
    // SAFETY: `client` is a valid IAudioClient. `format_ptr` points either to the
    // COM-allocated mix format (freed via CoTaskMemFree right after Initialize)
    // or to `explicit_format`, which outlives the call. The returned capture
    // client and event handle are owned by the CaptureSession and closed on drop.
    let (capture_client, audio_event) = unsafe {
        let init_result = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_LOOPBACK,
            200_000,
            0,
            format_ptr,
            None,
        );
        if let Some(ptr) = mix_format_ptr {
            CoTaskMemFree(Some(ptr.cast()));
        }
        init_result.map_err(|e| format!("IAudioClient::Initialize: {e}"))?;

        let capture_client = client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| format!("GetService(IAudioCaptureClient): {e}"))?;
        let audio_event = CreateEventA(None, false, false, PCSTR::null())
            .map_err(|e| format!("CreateEventA: {e}"))?;
        client
            .SetEventHandle(audio_event)
            .map_err(|e| format!("SetEventHandle: {e}"))?;
        client
            .Start()
            .map_err(|e| format!("IAudioClient::Start: {e}"))?;
        Ok::<_, String>((capture_client, audio_event))
    }?;

    Ok((
        CaptureSession {
            client,
            capture_client,
            audio_event,
        },
        mode,
    ))
}

fn run_capture(
    target_pid: u32,
    stop_event_raw: usize,
    startup_tx: &Sender<Result<CaptureMode, String>>,
    tsfn: Option<std::sync::Arc<ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>>,
) -> Result<(), String> {
    // SAFETY: standard per-thread COM init on the dedicated capture thread;
    // balanced by CoUninitialize below on success.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if !hr.is_ok() && hr != HRESULT(RPC_E_CHANGED_MODE) {
        let msg = format!("CoInitializeEx failed: {hr:?}");
        let _ = startup_tx.send(Err(msg.clone()));
        return Err(msg);
    }
    let com_ok = hr == S_OK;

    let stop_event = HANDLE(stop_event_raw as *mut std::ffi::c_void);
    let (session, _) = match build_capture_session(target_pid) {
        Ok(s) => {
            let _ = startup_tx.send(Ok(s.1));
            s
        }
        Err(e) => {
            let _ = startup_tx.send(Err(e.clone()));
            return Err(e);
        }
    };

    let run_result = session.run(stop_event, tsfn.as_deref());
    drop(session);
    if com_ok {
        // SAFETY: balances the successful CoInitializeEx at the top.
        unsafe { CoUninitialize() };
    }
    run_result
}

fn stop_capture_locked(state: &mut WasapiState) {
    state.is_active = false;
    if let Some(raw) = state.stop_event_raw.take() {
        // SAFETY: `raw` was created by CreateEventA in start_audio_capture, is
        // only signalled here, and is still a valid handle before the join and close.
        unsafe {
            let _ = SetEvent(HANDLE(raw as *mut std::ffi::c_void));
        }
        if let Some(join) = state.capture_thread.take() {
            let _ = join.join();
        }
        // SAFETY: the capture thread has joined, so the event is no longer referenced
        // by it and is closed exactly once here.
        unsafe {
            let _ = CloseHandle(HANDLE(raw as *mut std::ffi::c_void));
        }
    } else if let Some(join) = state.capture_thread.take() {
        let _ = join.join();
    }
    state.mode = None;
    state.target_pid = None;
}

pub fn set_audio_data_callback(
    callback: std::sync::Arc<ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>,
) -> NapiResult<()> {
    let mut guard = AUDIO_DATA_CALLBACK
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    *guard = Some(callback);
    Ok(())
}

pub fn start_audio_capture(target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    let target_pid = match target_app_id {
        Either::A(label) => label
            .parse::<u32>()
            .map_err(|_| napi::Error::from_reason(format!("Invalid PID string: '{label}'")))?,
        Either::B(pid) => *pid as u32,
    };

    let mut guard = WASAPI_STATE
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let state = guard.get_or_insert_with(|| WasapiState {
        is_active: false,
        target_pid: None,
        stop_event_raw: None,
        capture_thread: None,
        mode: None,
    });
    stop_capture_locked(state);

    // SAFETY: null attribute pointer and name are valid; the returned handle is
    // closed on every exit path (success paths own it via stop_event_raw).
    let stop_event = unsafe { CreateEventA(None, false, false, PCSTR::null()) }
        .map_err(|e| napi_err("CreateEventA", e))?;
    let stop_raw = stop_event.0 as usize;

    let tsfn = AUDIO_DATA_CALLBACK
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?
        .clone();

    let (tx, rx) = channel::<Result<CaptureMode, String>>();
    let join = std::thread::Builder::new()
        .name("wasapi-loopback-capture".into())
        .spawn(move || {
            let _ = run_capture(target_pid, stop_raw, &tx, tsfn);
        })
        .map_err(|e| {
            // SAFETY: the thread never started, so this is the only owner of the handle.
            let _ = unsafe { CloseHandle(stop_event) };
            napi_err("Failed to spawn WASAPI thread", e)
        })?;

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(mode)) => {
            state.is_active = true;
            state.mode = Some(mode);
            state.target_pid = Some(target_pid);
            state.stop_event_raw = Some(stop_raw);
            state.capture_thread = Some(join);
            Ok(true)
        }
        Ok(Err(e)) => {
            let _ = join.join();
            // SAFETY: the capture thread has exited, so no other code references
            // the event; this is the final owner, closed exactly once.
            let _ = unsafe { CloseHandle(stop_event) };
            Err(napi_error("WASAPI startup", e))
        }
        Err(_) => {
            // SAFETY: the event handle is still valid; signalling wakes the capture
            // thread so it can observe the stop request and exit.
            let _ = unsafe { SetEvent(stop_event) };
            let _ = join.join();
            // SAFETY: after join the thread no longer touches the event; closed once.
            let _ = unsafe { CloseHandle(stop_event) };
            Err(napi::Error::from_reason("WASAPI startup timed out"))
        }
    }
}

fn napi_error(context: &str, e: String) -> napi::Error {
    napi::Error::from_reason(format!("{context}: {e}"))
}

pub fn stop_audio_capture() -> NapiResult<bool> {
    let Ok(mut guard) = WASAPI_STATE.lock() else {
        return Ok(true);
    };
    if let Some(state) = guard.as_mut() {
        stop_capture_locked(state);
    }
    Ok(true)
}

pub fn is_audio_capture_active() -> NapiResult<bool> {
    let Ok(mut guard) = WASAPI_STATE.lock() else {
        return Ok(false);
    };
    let Some(state) = guard.as_mut() else {
        return Ok(false);
    };
    if !state.is_active {
        return Ok(false);
    }
    if state
        .capture_thread
        .as_ref()
        .is_some_and(|t| t.is_finished())
    {
        stop_capture_locked(state);
        return Ok(false);
    }
    Ok(true)
}

pub fn switch_audio_capture(_: &Either<String, i32>) -> NapiResult<bool> {
    Err(napi::Error::from_reason(
        "Audio target switching is not yet supported on Windows",
    ))
}

pub fn resolve_audio_app_for_x11_window(_: u32) -> Option<AudioApp> {
    None
}

pub fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    None
}

pub fn get_capture_context() -> NapiResult<crate::CaptureContext> {
    Err(napi::Error::from_reason(
        "Capture context introspection is only available on Linux",
    ))
}

pub fn start_audio_metering() -> NapiResult<bool> {
    Ok(false)
}

pub fn stop_audio_metering() -> NapiResult<bool> {
    Ok(true)
}

pub fn get_audio_levels() -> NapiResult<Vec<AudioAppLevel>> {
    Ok(Vec::new())
}

pub fn set_dmabuf_callback(
    _: std::sync::Arc<ThreadsafeFunction<(i32, i32, i32, i32, i32, i32), ()>>,
) -> NapiResult<()> {
    Ok(())
}

pub fn clear_dmabuf_callback() {}

pub fn start_video_capture(_: u32, _: u32, _: u32, _: u32) -> NapiResult<bool> {
    Err(napi::Error::from_reason(
        "Video capture is not supported on Windows",
    ))
}

pub fn stop_video_capture() -> NapiResult<bool> {
    Ok(true)
}

pub fn is_video_capture_active() -> NapiResult<bool> {
    Ok(false)
}

pub fn list_screen_sources() -> napi::Result<Vec<napi::Unknown<'static>>> {
    Ok(Vec::new())
}
