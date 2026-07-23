use crate::AudioApp;
use napi::{Either, Result as NapiResult};

mod wasapi {
    use super::AudioApp;
    use napi::Result as NapiResult;
    use std::collections::{HashMap, HashSet};
    use std::mem::{size_of, ManuallyDrop};
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use windows::core::{
        implement, Error, HRESULT, IUnknown, Interface, Ref, PCSTR,
    };
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, S_OK, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::{
        ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
        IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
        IAudioCaptureClient, IAudioClient, IAudioSessionControl2, IAudioSessionManager2,
        IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
        AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
    };
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, BLOB, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    use windows::Win32::System::Threading::{CreateEventA, SetEvent, WaitForMultipleObjects};
    use windows::Win32::System::Variant::VT_BLOB;

    /// Process-scoped loopback requires Windows 11 22H2 (build 22621) or later.
    const PROCESS_LOOPBACK_MIN_BUILD: u32 = 22621;
    /// `INFINITE` timeout for `WaitForMultipleObjects`.
    const WAIT_INFINITE_MS: u32 = 0xFFFF_FFFF;
    /// Raw value of `WAIT_FAILED`.
    const WAIT_FAILED_RAW: u32 = 0xFFFF_FFFF;
    /// `RPC_E_CHANGED_MODE` — COM already initialized with a different model.
    const RPC_E_CHANGED_MODE_RAW: i32 = 0x8001_0106u32 as i32;
    /// Stereo float32 frame size for the explicit process-loopback format.
    const BLOCK_ALIGN: u16 = 8;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CaptureMode {
        /// Process-scoped loopback capturing only the target process tree.
        ProcessLoopback,
        /// Standard system-wide loopback (no per-process filtering possible).
        SystemLoopback,
    }

    struct WasapiState {
        is_active: bool,
        target_pid: Option<u32>,
        stop_event_raw: Option<usize>,
        capture_thread: Option<std::thread::JoinHandle<()>>,
        mode: Option<CaptureMode>,
    }

    impl WasapiState {
        fn new() -> Self {
            Self {
                is_active: false,
                target_pid: None,
                stop_event_raw: None,
                capture_thread: None,
                mode: None,
            }
        }
    }

    static WASAPI_STATE: Mutex<Option<WasapiState>> = Mutex::new(None);

    fn napi_err(context: &str, e: impl std::fmt::Display) -> napi::Error {
        napi::Error::from_reason(format!("{}: {}", context, e))
    }

    // -----------------------------------------------------------------------
    // OS version / process enumeration helpers
    // -----------------------------------------------------------------------

    /// Returns the host OS build number via `RtlGetVersion` (unlike
    /// `GetVersionExW`, this is not subject to manifest-based lying).
    fn os_build_number() -> Option<u32> {
        unsafe {
            let mut info: OSVERSIONINFOW = std::mem::zeroed();
            info.dwOSVersionInfoSize = size_of::<OSVERSIONINFOW>() as u32;
            let status = RtlGetVersion(&mut info);
            if status.is_ok() {
                Some(info.dwBuildNumber)
            } else {
                None
            }
        }
    }

    /// Maps PID → executable name for every process in the ToolHelp snapshot.
    fn snapshot_process_names() -> HashMap<u32, String> {
        let mut map = HashMap::new();
        unsafe {
            let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
                Ok(h) => h,
                Err(_) => return map,
            };
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    let end = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                    map.insert(entry.th32ProcessID, name);
                    if Process32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
        }
        map
    }

    // -----------------------------------------------------------------------
    // Application enumeration (audio sessions on the default render endpoint)
    // -----------------------------------------------------------------------

    pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            let com_initialized = hr.is_ok();
            let result = enumerate_audio_apps();
            if com_initialized {
                CoUninitialize();
            }
            result
        }
    }

    unsafe fn enumerate_audio_apps() -> NapiResult<Vec<AudioApp>> {
        let names = snapshot_process_names();

        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| napi_err("CoCreateInstance(MMDeviceEnumerator) failed", e))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| napi_err("GetDefaultAudioEndpoint failed", e))?;
        let manager: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| napi_err("Activate(IAudioSessionManager2) failed", e))?;
        let sessions = manager
            .GetSessionEnumerator()
            .map_err(|e| napi_err("GetSessionEnumerator failed", e))?;
        let count = sessions
            .GetCount()
            .map_err(|e| napi_err("IAudioSessionEnumerator::GetCount failed", e))?;

        let mut apps = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..count {
            let Ok(control) = sessions.GetSession(i) else {
                continue;
            };
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            // S_OK means this is the system-sounds session: skip it.
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
                .unwrap_or_else(|| format!("Process {}", pid));
            apps.push(AudioApp {
                id: pid as i32,
                name,
                process_id: pid as i32,
                bundle_id: None,
            });
        }
        Ok(apps)
    }

    // -----------------------------------------------------------------------
    // Process-scoped loopback activation (Windows 11 22H2+)
    // -----------------------------------------------------------------------

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
            let mut completed = lock.lock().unwrap_or_else(|e| e.into_inner());
            *completed = true;
            drop(completed);
            cvar.notify_one();
            Ok(())
        }
    }

    /// Activates an `IAudioClient` in process-loopback mode which captures
    /// ONLY the audio of the tree of `target_pid` and nothing else.
    unsafe fn activate_process_loopback(target_pid: u32) -> windows::core::Result<IAudioClient> {
        let mut activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: target_pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };

        // Pack the activation params into a VT_BLOB PROPVARIANT.
        let raw_prop = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: (&mut activation_params
                                as *mut AUDIOCLIENT_ACTIVATION_PARAMS)
                                .cast::<u8>(),
                        },
                    },
                }),
            },
        };
        // `raw_prop` and `activation_params` outlive the completion wait below.
        let activation_params_ptr: *const PROPVARIANT = &raw_prop;

        let setup = Arc::new((Mutex::new(false), Condvar::new()));
        let callback: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
            state: setup.clone(),
        }
        .into();

        let operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(activation_params_ptr),
            &callback,
        )?;

        // Wait (with deadline) for the async activation to complete.
        let deadline = Instant::now() + Duration::from_secs(10);
        let (lock, cvar) = &*setup;
        let mut completed = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !*completed {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::new(
                    HRESULT::from_win32(1460), // ERROR_TIMEOUT
                    "Timed out waiting for process loopback activation",
                ));
            }
            let (guard, _) = cvar
                .wait_timeout(completed, remaining)
                .unwrap_or_else(|e| e.into_inner());
            completed = guard;
        }
        drop(completed);

        let mut audio_client: Option<IUnknown> = None;
        let mut result = HRESULT(0);
        operation.GetActivateResult(&mut result, &mut audio_client)?;
        result.ok()?;
        let unknown = audio_client
            .ok_or_else(|| Error::new(E_FAIL, "Process loopback activation returned no interface"))?;
        unknown.cast::<IAudioClient>()
    }

    // -----------------------------------------------------------------------
    // System-wide loopback fallback
    // -----------------------------------------------------------------------

    /// Activates a regular `IAudioClient` on the default render endpoint and
    /// returns it together with the mix format (caller frees with
    /// `CoTaskMemFree` after `Initialize`).
    unsafe fn activate_system_loopback() -> windows::core::Result<(IAudioClient, *mut WAVEFORMATEX)> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix_format = client.GetMixFormat()?;
        Ok((client, mix_format))
    }

    /// Explicit 48 kHz stereo float32 format. Process loopback clients do not
    /// support `GetMixFormat`, so an explicit format must be supplied.
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
            dwChannelMask: 0x3, // front left + front right
        }
    }

    // -----------------------------------------------------------------------
    // Capture session (RAII: Stop + CloseHandle on drop)
    // -----------------------------------------------------------------------

    struct CaptureSession {
        client: IAudioClient,
        capture_client: IAudioCaptureClient,
        audio_event: HANDLE,
        mode: CaptureMode,
    }

    impl CaptureSession {
        /// Drains all pending capture packets. The PCM frames are the handoff
        /// point for the WebRTC/Opus encoding pipeline.
        fn drain_packets(&self) -> Result<(), String> {
            unsafe {
                loop {
                    let packet_frames = self
                        .capture_client
                        .GetNextPacketSize()
                        .map_err(|e| format!("GetNextPacketSize failed: {}", e))?;
                    if packet_frames == 0 {
                        return Ok(());
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    self.capture_client
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                        .map_err(|e| format!("GetBuffer failed: {}", e))?;
                    // PCM handoff point: `data` holds `frames` frames of
                    // interleaved float32 stereo PCM
                    // (`frames * BLOCK_ALIGN` bytes). Forward into the
                    // WebRTC audio pipeline here.
                    let _pcm_byte_len = frames as usize * BLOCK_ALIGN as usize;
                    self.capture_client
                        .ReleaseBuffer(frames)
                        .map_err(|e| format!("ReleaseBuffer failed: {}", e))?;
                }
            }
        }

        /// Event-driven capture loop: waits for either the stop event or the
        /// audio-ready event and drains capture packets until stopped.
        fn run(&self, stop_event: HANDLE) -> Result<(), String> {
            let handles = [stop_event, self.audio_event];
            loop {
                let ev = unsafe { WaitForMultipleObjects(&handles, false, WAIT_INFINITE_MS) };
                if ev.0 == WAIT_OBJECT_0.0 {
                    // Stop event signaled.
                    return Ok(());
                }
                if ev.0 == WAIT_OBJECT_0.0 + 1 {
                    self.drain_packets()?;
                } else if ev.0 == WAIT_FAILED_RAW {
                    return Err("WaitForMultipleObjects returned WAIT_FAILED".to_string());
                }
            }
        }
    }

    impl Drop for CaptureSession {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
                let _ = CloseHandle(self.audio_event);
            }
        }
    }

    /// Builds and starts a capture session for `target_pid`, selecting
    /// process-scoped loopback when supported and falling back to system-wide
    /// loopback otherwise.
    unsafe fn build_capture_session(target_pid: u32) -> Result<CaptureSession, String> {
        let process_loopback_supported = os_build_number()
            .map(|build| build >= PROCESS_LOOPBACK_MIN_BUILD)
            .unwrap_or(false);

        let (client, mode, mix_format_ptr): (IAudioClient, CaptureMode, Option<*mut WAVEFORMATEX>) =
            if process_loopback_supported {
                match activate_process_loopback(target_pid) {
                    Ok(client) => (client, CaptureMode::ProcessLoopback, None),
                    Err(_) => {
                        let (client, fmt) = activate_system_loopback()
                            .map_err(|e2| format!("System loopback activation failed: {}", e2))?;
                        (client, CaptureMode::SystemLoopback, Some(fmt))
                    }
                }
            } else {
                let (client, fmt) = activate_system_loopback()
                    .map_err(|e| format!("System loopback activation failed: {}", e))?;
                (client, CaptureMode::SystemLoopback, Some(fmt))
            };

        // Process loopback requires an explicit format; the system loopback
        // uses the device mix format (freed after Initialize).
        let explicit_format = make_loopback_format();
        let format_ptr: *const WAVEFORMATEX = match mix_format_ptr {
            Some(ptr) => ptr,
            None => &explicit_format.Format,
        };
        let init_result = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_LOOPBACK,
            200_000, // 20 ms buffer, in 100 ns units
            0,
            format_ptr,
            None,
        );
        if let Some(ptr) = mix_format_ptr {
            CoTaskMemFree(Some(ptr.cast()));
        }
        init_result.map_err(|e| format!("IAudioClient::Initialize failed: {}", e))?;

        let capture_client = client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| format!("GetService(IAudioCaptureClient) failed: {}", e))?;
        let audio_event = CreateEventA(None, false, false, PCSTR::null())
            .map_err(|e| format!("CreateEventA failed: {}", e))?;
        client
            .SetEventHandle(audio_event)
            .map_err(|e| format!("SetEventHandle failed: {}", e))?;
        client
            .Start()
            .map_err(|e| format!("IAudioClient::Start failed: {}", e))?;

        Ok(CaptureSession {
            client,
            capture_client,
            audio_event,
            mode,
        })
    }

    /// Entry point of the capture thread. Owns all COM objects; reports
    /// startup success/failure through `startup_tx`.
    unsafe fn run_capture(
        target_pid: u32,
        stop_event_raw: usize,
        startup_tx: &Sender<Result<CaptureMode, String>>,
    ) -> Result<(), String> {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        let com_initialized = hr.is_ok() || hr == HRESULT(RPC_E_CHANGED_MODE_RAW);
        if !com_initialized {
            let msg = format!("CoInitializeEx failed: {:?}", hr);
            let _ = startup_tx.send(Err(msg.clone()));
            return Err(msg);
        }

        let stop_event = HANDLE(stop_event_raw as *mut std::ffi::c_void);
        let result = match build_capture_session(target_pid) {
            Ok(session) => {
                let _ = startup_tx.send(Ok(session.mode));
                session.run(stop_event)
            }
            Err(e) => {
                let _ = startup_tx.send(Err(e.clone()));
                Err(e)
            }
        };

        if hr.is_ok() {
            CoUninitialize();
        }
        result
    }

    fn stop_capture_locked(state: &mut WasapiState) {
        state.is_active = false;
        if let Some(raw) = state.stop_event_raw.take() {
            let stop_event = HANDLE(raw as *mut std::ffi::c_void);
            unsafe {
                let _ = SetEvent(stop_event);
            }
            if let Some(join) = state.capture_thread.take() {
                let _ = join.join();
            }
            unsafe {
                let _ = CloseHandle(stop_event);
            }
        } else if let Some(join) = state.capture_thread.take() {
            let _ = join.join();
        }
        state.mode = None;
        state.target_pid = None;
    }

    // -----------------------------------------------------------------------
    // Public module interface
    // -----------------------------------------------------------------------

    pub fn start_capture(target_pid: u32) -> NapiResult<bool> {
        let mut guard = WASAPI_STATE
            .lock()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let state = guard.get_or_insert_with(WasapiState::new);

        // Restart semantics: stop any running capture first.
        stop_capture_locked(state);

        let stop_event = unsafe { CreateEventA(None, false, false, PCSTR::null()) }
            .map_err(|e| napi_err("CreateEventA failed", e))?;
        let stop_raw = stop_event.0 as usize;

        let (tx, rx) = channel::<Result<CaptureMode, String>>();
        let join = match std::thread::Builder::new()
            .name("wasapi-loopback-capture".to_string())
            .spawn(move || {
                let _ = unsafe { run_capture(target_pid, stop_raw, &tx) };
            }) {
            Ok(j) => j,
            Err(e) => {
                unsafe {
                    let _ = CloseHandle(stop_event);
                }
                return Err(napi_err("Failed to spawn WASAPI capture thread", e));
            }
        };

        // Block until the capture thread reports startup success or failure.
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
                unsafe {
                    let _ = CloseHandle(stop_event);
                }
                Err(napi::Error::from_reason(format!(
                    "WASAPI capture startup failed: {}",
                    e
                )))
            }
            Err(_) => {
                unsafe {
                    let _ = SetEvent(stop_event);
                }
                let _ = join.join();
                unsafe {
                    let _ = CloseHandle(stop_event);
                }
                Err(napi::Error::from_reason(
                    "WASAPI capture startup timed out".to_string(),
                ))
            }
        }
    }

    pub fn stop_capture() -> NapiResult<bool> {
        let mut guard = WASAPI_STATE
            .lock()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if let Some(state) = guard.as_mut() {
            stop_capture_locked(state);
        }
        Ok(true)
    }

    pub fn is_capture_active() -> NapiResult<bool> {
        let mut guard = WASAPI_STATE
            .lock()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if let Some(state) = guard.as_mut() {
            let thread_alive = state
                .capture_thread
                .as_ref()
                .map(|t| !t.is_finished())
                .unwrap_or(false);
            if state.is_active && !thread_alive {
                // The capture thread died (e.g. audio device unplugged);
                // clean up its remains before reporting inactive.
                stop_capture_locked(state);
                return Ok(false);
            }
            Ok(state.is_active)
        } else {
            Ok(false)
        }
    }
}

pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    wasapi::list_audio_applications()
}

pub fn start_audio_capture(target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    let pid = match target_app_id {
        Either::B(n) if *n > 0 => Some(*n as u32),
        Either::A(s) => s.trim().parse::<u32>().ok().filter(|p| *p > 0),
        _ => None,
    }
    .ok_or_else(|| napi::Error::from_reason("A process ID is required as the audio capture target"))?;
    wasapi::start_capture(pid)
}

pub fn stop_audio_capture() -> NapiResult<bool> { wasapi::stop_capture() }

pub fn is_audio_capture_active() -> NapiResult<bool> { wasapi::is_capture_active() }
