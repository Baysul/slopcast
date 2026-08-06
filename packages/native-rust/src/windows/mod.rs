#![allow(
    clippy::ref_as_ptr,
    clippy::inline_always,
    reason = "the windows-rs #[implement] macro generates #[inline(always)] Interface helpers with raw-pointer casts"
)]

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::{AudioApp, AudioTarget};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, S_FALSE, S_OK, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
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
use windows::Win32::System::Threading::{
    CreateEventA, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Error, GUID, HRESULT, IUnknown, Interface, PCSTR, Ref, implement};

const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const PROCESS_LOOPBACK_MIN_BUILD: u32 = 20348;
const RPC_E_CHANGED_MODE: i32 = -2_147_410_682;
const WASAPI_BUFFER_DURATION_100NS: i64 = 200_000; // 20 ms
const KSAUDIO_SPEAKER_STEREO: u32 = 0x3;
const TARGET_OUTPUT_SAMPLE_RATE: u32 = 48_000;
const WIN32_ERROR_TIMEOUT: u32 = 1460;
const WAIT_TIMEOUT_INFINITE: u32 = u32::MAX;

const WAVE_FORMAT_PCM_TAG: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT_TAG: u16 = 3;
const WAVEFORMATEXTENSIBLE_MIN_CB_SIZE: u16 = 22;

const MONO_CHANNELS: u16 = 1;
const STEREO_CHANNELS: u16 = 2;
const BITS_PER_SAMPLE_16: u16 = 16;
const BITS_PER_SAMPLE_24: u16 = 24;
const BITS_PER_SAMPLE_32: u16 = 32;
const BITS_PER_SAMPLE_64: u16 = 64;

const I16_MAX_F32: f32 = 32_767.0;
const I16_NORM_FACTOR: f32 = 32_768.0;
const I24_NORM_FACTOR: f32 = 8_388_608.0;
const I32_NORM_FACTOR: f32 = 2_147_483_648.0;
const I24_MSB_SIGN_BIT: u8 = 0x80;
const SIGN_EXTEND_BYTE: u8 = 0xFF;

const STEREO_I16_BYTES_PER_FRAME: usize = 4;
const STEREO_F32_BYTES_PER_FRAME: usize = 8;
const MONO_F32_BYTES_PER_FRAME: usize = 4;
const INITIAL_PCM_BUFFER_CAPACITY: usize = 16_384;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(10);
const ACTIVATION_POLL_INTERVAL: Duration = Duration::from_millis(50);

const SYSTEM_AUDIO_PID_DEFAULT: u32 = 0;
const SYSTEM_AUDIO_PID_ALL: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SendHandle(HANDLE);
// SAFETY: `HANDLE` is a plain pointer-sized kernel handle with no thread-local
// state; `OwnedHandle`/`SendHandle` only transfer or close it on owning threads.
unsafe impl Send for SendHandle {}
// SAFETY: see `Send` impl above — handles are freely shareable across threads.
unsafe impl Sync for SendHandle {}

struct CoTaskMemPtr<T>(*mut T);

impl<T> CoTaskMemPtr<T> {
    fn new(ptr: *mut T) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(CoTaskMemPtr(ptr))
        }
    }

    fn as_ptr(&self) -> *const T {
        self.0.cast_const()
    }
}

impl<T> Drop for CoTaskMemPtr<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was allocated by COM (e.g. CoTaskMemAlloc/GetMixFormat).
            unsafe {
                CoTaskMemFree(Some(self.0.cast()));
            }
        }
    }
}

#[derive(Debug)]
struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Result<Self, String> {
        if handle.is_invalid() {
            Err("Invalid Win32 handle".into())
        } else {
            Ok(OwnedHandle(handle))
        }
    }

    fn handle(&self) -> HANDLE {
        self.0
    }

    fn into_raw(self) -> HANDLE {
        let h = self.0;
        std::mem::forget(self);
        h
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: `self.0` is a valid open Win32 handle.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    ProcessLoopback,
    SystemLoopback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleType {
    Int,
    Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioFormat {
    channels: u16,
    sample_rate: u32,
    container_bits: u16,
    valid_bits: u16,
    block_align: u16,
    sample_type: SampleType,
    channel_mask: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FastFormat {
    StereoF32,
    StereoI16,
    MonoF32,
    Generic,
}

impl AudioFormat {
    fn fast_format(&self) -> FastFormat {
        if self.channels == STEREO_CHANNELS
            && self.container_bits == BITS_PER_SAMPLE_32
            && self.valid_bits == BITS_PER_SAMPLE_32
            && self.sample_type == SampleType::Float
        {
            FastFormat::StereoF32
        } else if self.channels == STEREO_CHANNELS
            && self.container_bits == BITS_PER_SAMPLE_16
            && self.valid_bits == BITS_PER_SAMPLE_16
            && self.sample_type == SampleType::Int
        {
            FastFormat::StereoI16
        } else if self.channels == MONO_CHANNELS
            && self.container_bits == BITS_PER_SAMPLE_32
            && self.valid_bits == BITS_PER_SAMPLE_32
            && self.sample_type == SampleType::Float
        {
            FastFormat::MonoF32
        } else {
            FastFormat::Generic
        }
    }
}

#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "samples are clamped to [-1, 1] before scaling by 32767, so the product always fits in i16"
)]
fn push_i16_stereo(buf: &mut Vec<u8>, l: f32, r: f32) {
    let l_i16 = (l.clamp(-1.0, 1.0) * I16_MAX_F32).round() as i16;
    let r_i16 = (r.clamp(-1.0, 1.0) * I16_MAX_F32).round() as i16;
    let l_bytes = l_i16.to_le_bytes();
    let r_bytes = r_i16.to_le_bytes();
    buf.extend_from_slice(&[l_bytes[0], l_bytes[1], r_bytes[0], r_bytes[1]]);
}

struct StereoResampler {
    in_sample_rate: u32,
    out_sample_rate: u32,
    last_frame: (f32, f32),
    phase: f64,
    initialized: bool,
}

impl StereoResampler {
    fn new(in_sample_rate: u32, out_sample_rate: u32) -> Self {
        Self {
            in_sample_rate,
            out_sample_rate,
            last_frame: (0.0, 0.0),
            phase: 0.0,
            initialized: false,
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "linear-interpolation blend factor needs no more than f32 precision"
    )]
    fn process_frame<F>(&mut self, current_frame: (f32, f32), mut emit_output: F)
    where
        F: FnMut((f32, f32)),
    {
        if self.in_sample_rate == self.out_sample_rate {
            emit_output(current_frame);
            return;
        }

        if !self.initialized {
            self.last_frame = current_frame;
            self.initialized = true;
        }

        let ratio = f64::from(self.in_sample_rate) / f64::from(self.out_sample_rate);
        while self.phase <= 1.0 {
            let t = self.phase as f32;
            let l = self.last_frame.0 * (1.0 - t) + current_frame.0 * t;
            let r = self.last_frame.1 * (1.0 - t) + current_frame.1 * t;
            emit_output((l, r));
            self.phase += ratio;
        }
        self.phase -= 1.0;
        self.last_frame = current_frame;
    }
}

struct WasapiState {
    is_active: bool,
    target_pid: Option<u32>,
    stop_event: Option<SendHandle>,
    capture_thread: Option<std::thread::JoinHandle<()>>,
    mode: Option<CaptureMode>,
}

pub struct WasapiManager {
    state: Mutex<Option<WasapiState>>,
}

static MANAGER: WasapiManager = WasapiManager::new();

fn validate_target_pid(pid: i32) -> Result<u32, String> {
    if pid == -1 || pid == 0 {
        Ok(SYSTEM_AUDIO_PID_DEFAULT)
    } else if pid > 0 {
        Ok(u32::try_from(pid).unwrap_or(0))
    } else {
        Err(format!(
            "Invalid target process ID: {pid}. Must be -1 (system audio), 0, or a positive PID"
        ))
    }
}

impl WasapiManager {
    const fn new() -> Self {
        Self {
            state: Mutex::new(None),
        }
    }

    pub fn stop_audio_capture(&self) -> bool {
        let Ok(mut guard) = self.state.lock() else {
            return true;
        };
        if let Some(state) = guard.as_mut() {
            Self::stop_capture_locked(state);
        }
        true
    }

    pub fn is_audio_capture_active(&self) -> bool {
        let Ok(mut guard) = self.state.lock() else {
            return false;
        };
        let Some(state) = guard.as_mut() else {
            return false;
        };
        if !state.is_active {
            return false;
        }
        if state
            .capture_thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            Self::stop_capture_locked(state);
            return false;
        }
        true
    }

    fn stop_capture_locked(state: &mut WasapiState) {
        crate::audio_ring::stop_audio_ring();
        state.is_active = false;
        let join = state.capture_thread.take();
        let stop_handle = state.stop_event.take();
        if let Some(SendHandle(handle)) = stop_handle {
            // SAFETY: `handle` was created by CreateEventA in start_audio_capture,
            // is only signalled here, and is still a valid handle until the reaper
            // closes it after the thread has joined.
            unsafe {
                let _ = SetEvent(handle);
            }
        }
        state.mode = None;
        state.target_pid = None;
        if let Some(join) = join {
            // Reap off-thread: a stalled WASAPI capture read must never block
            // the Electron main process. The stop event is closed only after
            // the thread has joined, keeping its reference valid.
            let _ = std::thread::Builder::new()
                .name("wasapi-reaper".into())
                .spawn(move || {
                    let _ = join.join();
                    if let Some(SendHandle(handle)) = stop_handle {
                        // SAFETY: the capture thread has joined, so the event is
                        // no longer referenced by it and is closed exactly once here.
                        unsafe {
                            let _ = CloseHandle(handle);
                        }
                    }
                });
        }
    }

    pub fn start_audio_capture(&self, target: &AudioTarget) -> Result<bool, String> {
        let target_pid = match target {
            AudioTarget::Label(label) => {
                let p = label
                    .parse::<i32>()
                    .map_err(|_| format!("Invalid PID string: '{label}'"))?;
                validate_target_pid(p)?
            }
            AudioTarget::Id(pid) => validate_target_pid(*pid)?,
        };

        let stop_event = OwnedHandle::new(
            // SAFETY: standard event-object creation; the returned handle is
            // wrapped in OwnedHandle, which closes it exactly once.
            unsafe { CreateEventA(None, false, false, PCSTR::null()) }
                .map_err(|e| format!("CreateEventA: {e}"))?,
        )?;

        let (startup_tx, startup_rx) = channel::<Result<CaptureMode, String>>();
        let (run_tx, run_rx) = channel::<()>();

        let stop_send_handle = SendHandle(stop_event.handle());
        let join = std::thread::Builder::new()
            .name("wasapi-loopback-capture".into())
            .spawn(move || {
                let _ = run_capture(target_pid, stop_send_handle, &startup_tx, &run_rx);
            })
            .map_err(|e| format!("Failed to spawn WASAPI thread: {e}"))?;

        let stop_raw = stop_event.into_raw();

        match startup_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(mode)) => {
                let mut guard = match self.state.lock() {
                    Ok(g) => g,
                    Err(e) => {
                        return Err(cleanup_failed_startup(
                            run_tx,
                            stop_raw,
                            join,
                            e.to_string(),
                        ));
                    }
                };
                let state = guard.get_or_insert_with(|| WasapiState {
                    is_active: false,
                    target_pid: None,
                    stop_event: None,
                    capture_thread: None,
                    mode: None,
                });
                Self::stop_capture_locked(state);

                if let Err(e) = crate::audio_ring::start_audio_ring() {
                    return Err(cleanup_failed_startup(run_tx, stop_raw, join, e));
                }

                state.is_active = true;
                state.mode = Some(mode);
                state.target_pid = Some(target_pid);
                state.stop_event = Some(SendHandle(stop_raw));
                state.capture_thread = Some(join);

                let _ = run_tx.send(());
                Ok(true)
            }
            Ok(Err(e)) => Err(cleanup_failed_startup(
                run_tx,
                stop_raw,
                join,
                format!("WASAPI startup: {e}"),
            )),
            Err(_) => Err(cleanup_failed_startup(
                run_tx,
                stop_raw,
                join,
                "WASAPI startup timed out".into(),
            )),
        }
    }
}

fn cleanup_failed_startup(
    run_tx: Sender<()>,
    stop_raw: HANDLE,
    join: std::thread::JoinHandle<()>,
    msg: String,
) -> String {
    drop(run_tx);
    // SAFETY: `stop_raw` is a valid CreateEventA handle; signalling it lets a
    // capture thread blocked on the event exit before we join it.
    let _ = unsafe { SetEvent(stop_raw) };
    let _ = join.join();
    // SAFETY: the capture thread has joined, so `stop_raw` is no longer
    // referenced and can be closed exactly once here.
    let _ = unsafe { CloseHandle(stop_raw) };
    msg
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "dwOSVersionInfoSize is the fixed OSVERSIONINFOW struct size, far below u32::MAX"
)]
fn os_build_number() -> Option<u32> {
    // SAFETY: all-zero is a valid OSVERSIONINFOW bit pattern.
    let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
    info.dwOSVersionInfoSize = u32::try_from(size_of::<OSVERSIONINFOW>()).unwrap_or(0);
    // SAFETY: `info` is a valid, correctly sized out-buffer that outlives the call.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    status.is_ok().then_some(info.dwBuildNumber)
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "one parse routine covering the WASAPI format matrix; format tags and cbSize are 16-bit values typed u32 by the windows crate"
)]
fn parse_wave_format(fmt_ptr: *const WAVEFORMATEX) -> Result<AudioFormat, String> {
    if fmt_ptr.is_null() {
        return Err("Null WAVEFORMATEX pointer".into());
    }
    // SAFETY: `fmt_ptr` was obtained from GetMixFormat or points to explicit_format.
    unsafe {
        let fmt = &*fmt_ptr;
        let w_format_tag = fmt.wFormatTag;
        let n_channels = fmt.nChannels;
        let n_samples_per_sec = fmt.nSamplesPerSec;
        let n_block_align = fmt.nBlockAlign;
        let w_bits_per_sample = fmt.wBitsPerSample;
        let cb_size = fmt.cbSize;

        if n_channels == 0 || n_samples_per_sec == 0 || n_block_align == 0 || w_bits_per_sample == 0
        {
            return Err("Invalid WAVEFORMATEX parameters".into());
        }

        let container_bits = w_bits_per_sample;
        let mut valid_bits = w_bits_per_sample;

        let min_block_align = n_channels.saturating_mul(container_bits / 8);
        if min_block_align == 0 || n_block_align < min_block_align {
            return Err(format!(
                "nBlockAlign {n_block_align} is less than required minimum {min_block_align}"
            ));
        }

        let sample_type = if w_format_tag == WAVE_FORMAT_PCM_TAG {
            SampleType::Int
        } else if w_format_tag == WAVE_FORMAT_IEEE_FLOAT_TAG {
            SampleType::Float
        } else if w_format_tag == WAVE_FORMAT_EXTENSIBLE as u16 {
            if cb_size < WAVEFORMATEXTENSIBLE_MIN_CB_SIZE {
                return Err("WAVEFORMATEXTENSIBLE cbSize too small".into());
            }
            let ext_ptr = fmt_ptr.cast::<WAVEFORMATEXTENSIBLE>();
            let subformat = std::ptr::addr_of!((*ext_ptr).SubFormat).read_unaligned();
            let v_bits =
                std::ptr::addr_of!((*ext_ptr).Samples.wValidBitsPerSample).read_unaligned();
            if v_bits > 0 && v_bits <= container_bits {
                valid_bits = v_bits;
            }

            if subformat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                SampleType::Float
            } else if subformat == KSDATAFORMAT_SUBTYPE_PCM {
                SampleType::Int
            } else {
                return Err(format!(
                    "Unsupported WAVEFORMATEXTENSIBLE SubFormat GUID: {subformat:?}"
                ));
            }
        } else {
            return Err(format!(
                "Unsupported WAVEFORMATEX format tag: {w_format_tag}"
            ));
        };

        let channel_mask = if w_format_tag == WAVE_FORMAT_EXTENSIBLE as u16
            && cb_size >= WAVEFORMATEXTENSIBLE_MIN_CB_SIZE
        {
            let ext_ptr = fmt_ptr.cast::<WAVEFORMATEXTENSIBLE>();
            let mask = std::ptr::addr_of!((*ext_ptr).dwChannelMask).read_unaligned();
            if mask != 0 { Some(mask) } else { None }
        } else {
            None
        };

        match sample_type {
            SampleType::Int => match container_bits {
                BITS_PER_SAMPLE_16 => {
                    if valid_bits > BITS_PER_SAMPLE_16 {
                        return Err(format!(
                            "Invalid valid_bits {valid_bits} for 16-bit int container"
                        ));
                    }
                }
                BITS_PER_SAMPLE_24 => {
                    if valid_bits > BITS_PER_SAMPLE_24 {
                        return Err(format!(
                            "Invalid valid_bits {valid_bits} for 24-bit int container"
                        ));
                    }
                }
                BITS_PER_SAMPLE_32 => {
                    if valid_bits > BITS_PER_SAMPLE_32 {
                        return Err(format!(
                            "Invalid valid_bits {valid_bits} for 32-bit int container"
                        ));
                    }
                }
                other => {
                    return Err(format!("Unsupported int container size: {other} bits"));
                }
            },
            SampleType::Float => match container_bits {
                BITS_PER_SAMPLE_32 => {
                    if valid_bits != BITS_PER_SAMPLE_32 {
                        return Err(format!(
                            "Unsupported float valid_bits {valid_bits} for 32-bit container"
                        ));
                    }
                }
                BITS_PER_SAMPLE_64 => {
                    if valid_bits != BITS_PER_SAMPLE_64 {
                        return Err(format!(
                            "Unsupported float valid_bits {valid_bits} for 64-bit container"
                        ));
                    }
                }
                other => {
                    return Err(format!("Unsupported float container size: {other} bits"));
                }
            },
        }

        Ok(AudioFormat {
            channels: n_channels,
            sample_rate: n_samples_per_sec,
            container_bits,
            valid_bits,
            block_align: n_block_align,
            sample_type,
            channel_mask,
        })
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "normalized sample values; f32 is the WebRTC sink format"
)]
fn extract_stereo_f32(frame_bytes: &[u8], format: &AudioFormat) -> (f32, f32) {
    let bytes_per_sample = format.container_bits as usize / 8;
    if bytes_per_sample == 0 || frame_bytes.len() < bytes_per_sample * format.channels as usize {
        return (0.0, 0.0);
    }

    let read_sample = |ch_idx: usize| -> f32 {
        if ch_idx >= format.channels as usize {
            return 0.0;
        }
        let offset = ch_idx * bytes_per_sample;
        let sample_bytes = &frame_bytes[offset..offset + bytes_per_sample];

        match (format.sample_type, bytes_per_sample) {
            (SampleType::Float, 4) => {
                let b = [
                    sample_bytes[0],
                    sample_bytes[1],
                    sample_bytes[2],
                    sample_bytes[3],
                ];
                let val = f32::from_le_bytes(b);
                if val.is_nan() { 0.0 } else { val }
            }
            (SampleType::Float, 8) => {
                let b = [
                    sample_bytes[0],
                    sample_bytes[1],
                    sample_bytes[2],
                    sample_bytes[3],
                    sample_bytes[4],
                    sample_bytes[5],
                    sample_bytes[6],
                    sample_bytes[7],
                ];
                let val = f64::from_le_bytes(b);
                if val.is_nan() { 0.0 } else { val as f32 }
            }
            (SampleType::Int, 2) => {
                let b = [sample_bytes[0], sample_bytes[1]];
                let raw_val = i16::from_le_bytes(b);
                let shift = BITS_PER_SAMPLE_16 - format.valid_bits;
                let val = (raw_val >> shift) << shift;
                f32::from(val) / I16_NORM_FACTOR
            }
            (SampleType::Int, 3) => {
                let b2_sign = if sample_bytes[2] & I24_MSB_SIGN_BIT != 0 {
                    SIGN_EXTEND_BYTE
                } else {
                    0x00
                };
                let raw_val = i32::from_le_bytes([
                    sample_bytes[0],
                    sample_bytes[1],
                    sample_bytes[2],
                    b2_sign,
                ]);
                let shift = BITS_PER_SAMPLE_24 - format.valid_bits;
                let val = (raw_val >> shift) << shift;
                val as f32 / I24_NORM_FACTOR
            }
            (SampleType::Int, 4) => {
                let b = [
                    sample_bytes[0],
                    sample_bytes[1],
                    sample_bytes[2],
                    sample_bytes[3],
                ];
                let raw_val = i32::from_le_bytes(b);
                let shift = BITS_PER_SAMPLE_32 - format.valid_bits;
                let val = (raw_val >> shift) << shift;
                val as f32 / I32_NORM_FACTOR
            }
            _ => 0.0,
        }
    };

    let ch0 = read_sample(0);
    if format.channels == MONO_CHANNELS {
        (ch0, ch0)
    } else {
        let ch1 = read_sample(1);
        (ch0, ch1)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "dwSize is the fixed PROCESSENTRY32W struct size, far below u32::MAX"
)]
fn snapshot_process_names() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    // SAFETY: returns a snapshot handle (or INVALID_HANDLE_VALUE, rejected by
    // OwnedHandle::new), which owns the snapshot until closed.
    let Ok(snapshot_raw) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return map;
    };
    let Ok(snapshot) = OwnedHandle::new(snapshot_raw) else {
        return map;
    };
    let snapshot_handle = snapshot.handle();
    // SAFETY: PROCESSENTRY32W is a POD struct; zeroing produces a valid
    // default that Process32FirstW overwrites.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0);
    // SAFETY: `snapshot_handle` is a valid open snapshot; `entry` is a
    // writable buffer of the exact struct type the API expects.
    if unsafe { Process32FirstW(snapshot_handle, &raw mut entry).is_ok() } {
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            map.insert(entry.th32ProcessID, name);
            // SAFETY: same as Process32FirstW above — the snapshot and the
            // entry buffer remain valid for the whole enumeration.
            if unsafe { Process32NextW(snapshot_handle, &raw mut entry).is_err() } {
                break;
            }
        }
    }
    map
}

pub fn list_audio_applications() -> Result<Vec<AudioApp>, String> {
    // SAFETY: standard per-thread COM initialization; balanced below on success.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let com_ok = hr == S_OK || hr == S_FALSE;
    let com_changed_mode = hr == HRESULT(RPC_E_CHANGED_MODE);
    if !com_ok && !com_changed_mode {
        return Err(format!("CoInitializeEx failed: {hr:?}"));
    }
    let result = enumerate_audio_apps();
    if com_ok {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    result
}

#[allow(
    clippy::cast_possible_wrap,
    reason = "Windows PIDs are far below i32::MAX; AudioApp fields are i32 for the historical JS boundary"
)]
fn enumerate_audio_apps() -> Result<Vec<AudioApp>, String> {
    let names = snapshot_process_names();
    // SAFETY: COM was initialized on this thread by the caller. Every interface
    // obtained here is valid, used only on this thread, and released on drop.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("CoCreateInstance(MMDeviceEnumerator): {e}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("GetDefaultAudioEndpoint: {e}"))?;
        let manager: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Activate(IAudioSessionManager2): {e}"))?;
        let sessions = manager
            .GetSessionEnumerator()
            .map_err(|e| format!("GetSessionEnumerator: {e}"))?;
        let count = sessions
            .GetCount()
            .map_err(|e| format!("GetSessionEnumerator::GetCount: {e}"))?;

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

fn wait_for_activation(
    setup: &Arc<(Mutex<bool>, Condvar)>,
    stop_event: Option<HANDLE>,
) -> windows::core::Result<()> {
    let deadline = Instant::now() + ACTIVATION_TIMEOUT;
    let (lock, cvar) = &**setup;
    let mut completed = lock
        .lock()
        .map_err(|e| Error::new(E_FAIL, format!("Activation mutex poisoned: {e}")))?;
    while !*completed {
        if let Some(stop_handle) = stop_event {
            // SAFETY: `stop_handle` is a valid handle created in start_audio_capture.
            if unsafe { WaitForSingleObject(stop_handle, 0) } == WAIT_OBJECT_0 {
                return Err(Error::new(E_FAIL, "Activation cancelled by stop event"));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::new(
                HRESULT::from_win32(WIN32_ERROR_TIMEOUT),
                "Timed out waiting for activation",
            ));
        }
        let wait_dur = remaining.min(ACTIVATION_POLL_INTERVAL);
        let (guard, _) = cvar
            .wait_timeout(completed, wait_dur)
            .map_err(|e| Error::new(E_FAIL, format!("Activation condvar poisoned: {e}")))?;
        completed = guard;
    }
    Ok(())
}

/// Leaks activation params when activation times out or is unresponsive.
///
/// # Rationale
/// `ActivateAudioInterfaceAsync` runs asynchronously on a system thread pool.
/// If the call times out waiting for `ActivationCompleted`, COM may still access
/// the params buffer later. Leaking the `Box` prevents a use-after-free.
fn leak_activation_params(params: Box<AUDIOCLIENT_ACTIVATION_PARAMS>) {
    std::mem::forget(params);
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "cbSize is the fixed AUDIOCLIENT_ACTIVATION_PARAMS struct size, far below u32::MAX"
)]
fn activate_process_loopback(
    target_pid: u32,
    stop_event: Option<HANDLE>,
) -> windows::core::Result<IAudioClient> {
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
                        cbSize: u32::try_from(size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>())
                            .unwrap_or(0),
                        pBlobData: (&raw mut *activation_params).cast::<u8>(),
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
    // longer references the params; on error paths `leak_activation_params`
    // prevents use-after-free if the operation completes late.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const raw_prop),
            &callback,
        )?
    };

    let wait_result = wait_for_activation(&setup, stop_event);
    if wait_result.is_err() {
        leak_activation_params(activation_params);
    }
    wait_result?;

    let mut audio_client: Option<IUnknown> = None;
    let mut result = HRESULT(0);
    // SAFETY: called exactly once, after activation completed; both out-params
    // are valid and outlive the call.
    unsafe { operation.GetActivateResult(&raw mut result, &raw mut audio_client) }?;
    result.ok()?;
    let unknown =
        audio_client.ok_or_else(|| Error::new(E_FAIL, "Activation returned no interface"))?;
    unknown.cast::<IAudioClient>()
}

fn activate_system_loopback() -> windows::core::Result<(IAudioClient, CoTaskMemPtr<WAVEFORMATEX>)> {
    // SAFETY: COM was initialized on this thread by the caller. The returned
    // interface and mix-format pointer are owned by the caller.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix_format = client.GetMixFormat()?;
        let mix_format_ptr = CoTaskMemPtr::new(mix_format)
            .ok_or_else(|| Error::new(E_FAIL, "GetMixFormat returned null pointer"))?;
        Ok((client, mix_format_ptr))
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "WAVEFORMATEXTENSIBLE format tag and cbSize are 16-bit values; the constants are u32 only because the windows crate types them so"
)]
fn make_loopback_format() -> WAVEFORMATEXTENSIBLE {
    const CHANNELS: u16 = STEREO_CHANNELS;
    const SAMPLE_RATE: u32 = TARGET_OUTPUT_SAMPLE_RATE;
    const BITS_PER_SAMPLE: u16 = BITS_PER_SAMPLE_32;
    const BLOCK_ALIGN: u16 = CHANNELS * (BITS_PER_SAMPLE / 8);
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
        dwChannelMask: KSAUDIO_SPEAKER_STEREO,
    }
}

struct BufferGuard<'a> {
    capture_client: &'a IAudioCaptureClient,
    frames: u32,
    released: bool,
}

impl<'a> BufferGuard<'a> {
    fn new(capture_client: &'a IAudioCaptureClient, frames: u32) -> Self {
        Self {
            capture_client,
            frames,
            released: false,
        }
    }

    fn release(&mut self) {
        if !self.released {
            // SAFETY: `capture_client` is a valid interface owned by this
            // session; ReleaseBuffer must be called exactly once per GetBuffer.
            unsafe {
                let _ = self.capture_client.ReleaseBuffer(self.frames);
            }
            self.released = true;
        }
    }
}

impl Drop for BufferGuard<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

struct CaptureSession {
    client: IAudioClient,
    capture_client: IAudioCaptureClient,
    audio_event: HANDLE,
    format: AudioFormat,
    resampler: StereoResampler,
    pcm_buffer: Vec<u8>,
}

impl CaptureSession {
    fn new(
        client: IAudioClient,
        capture_client: IAudioCaptureClient,
        audio_event: HANDLE,
        format: AudioFormat,
    ) -> Self {
        let resampler = StereoResampler::new(format.sample_rate, TARGET_OUTPUT_SAMPLE_RATE);
        Self {
            client,
            capture_client,
            audio_event,
            format,
            resampler,
            pcm_buffer: Vec::with_capacity(INITIAL_PCM_BUFFER_CAPACITY),
        }
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        clippy::similar_names,
        reason = "single conversion routine covering every WASAPI fast path; per-arm f32/i16 sample bindings are named by format and samples are clamped to [-1, 1] before scaling to i16"
    )]
    fn drain_packets(&mut self) -> Result<(), String> {
        self.pcm_buffer.clear();
        let frame_size = self.format.block_align as usize;
        let is_passthrough_rate = self.resampler.in_sample_rate == self.resampler.out_sample_rate;
        let fast_fmt = self.format.fast_format();

        // SAFETY: `capture_client` is a valid interface owned by this session;
        // every buffer obtained is released safely via `BufferGuard`.
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
                    .GetBuffer(&raw mut data, &raw mut frames, &raw mut flags, None, None)
                    .map_err(|e| format!("GetBuffer: {e}"))?;

                let mut guard = BufferGuard::new(&self.capture_client, frames);

                let is_silent = (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) != 0;
                if is_silent {
                    if is_passthrough_rate {
                        let added_bytes = frames as usize * STEREO_I16_BYTES_PER_FRAME;
                        self.pcm_buffer
                            .resize(self.pcm_buffer.len() + added_bytes, 0);
                    } else {
                        self.pcm_buffer
                            .reserve(frames as usize * STEREO_I16_BYTES_PER_FRAME);
                        for _ in 0..frames {
                            let buf = &mut self.pcm_buffer;
                            self.resampler.process_frame((0.0, 0.0), |(out_l, out_r)| {
                                push_i16_stereo(buf, out_l, out_r);
                            });
                        }
                    }
                } else {
                    let packet_bytes = (frames as usize)
                        .checked_mul(frame_size)
                        .ok_or("Overflow calculating packet byte length")?;
                    let raw_bytes = std::slice::from_raw_parts(data, packet_bytes);

                    if is_passthrough_rate {
                        match fast_fmt {
                            FastFormat::StereoF32 => {
                                let start = self.pcm_buffer.len();
                                let added = frames as usize * STEREO_I16_BYTES_PER_FRAME;
                                self.pcm_buffer.resize(start + added, 0);
                                let out_slice = &mut self.pcm_buffer[start..];

                                for (i, frame_chunk) in raw_bytes
                                    .chunks_exact(STEREO_F32_BYTES_PER_FRAME)
                                    .enumerate()
                                {
                                    let l_bits = u32::from_le_bytes([
                                        frame_chunk[0],
                                        frame_chunk[1],
                                        frame_chunk[2],
                                        frame_chunk[3],
                                    ]);
                                    let r_bits = u32::from_le_bytes([
                                        frame_chunk[4],
                                        frame_chunk[5],
                                        frame_chunk[6],
                                        frame_chunk[7],
                                    ]);
                                    let l_f32 = f32::from_bits(l_bits);
                                    let r_f32 = f32::from_bits(r_bits);
                                    let l_f32 = if l_f32.is_nan() { 0.0 } else { l_f32 };
                                    let r_f32 = if r_f32.is_nan() { 0.0 } else { r_f32 };
                                    let l_s16 =
                                        (l_f32.clamp(-1.0, 1.0) * I16_MAX_F32).round() as i16;
                                    let r_s16 =
                                        (r_f32.clamp(-1.0, 1.0) * I16_MAX_F32).round() as i16;
                                    let l_bytes = l_s16.to_le_bytes();
                                    let r_bytes = r_s16.to_le_bytes();
                                    let out = &mut out_slice[i * STEREO_I16_BYTES_PER_FRAME
                                        ..(i + 1) * STEREO_I16_BYTES_PER_FRAME];
                                    out[0] = l_bytes[0];
                                    out[1] = l_bytes[1];
                                    out[2] = r_bytes[0];
                                    out[3] = r_bytes[1];
                                }
                            }
                            FastFormat::StereoI16 => {
                                self.pcm_buffer.extend_from_slice(raw_bytes);
                            }
                            FastFormat::MonoF32 => {
                                let start = self.pcm_buffer.len();
                                let added = frames as usize * STEREO_I16_BYTES_PER_FRAME;
                                self.pcm_buffer.resize(start + added, 0);
                                let out_slice = &mut self.pcm_buffer[start..];

                                for (i, frame_chunk) in
                                    raw_bytes.chunks_exact(MONO_F32_BYTES_PER_FRAME).enumerate()
                                {
                                    let bits = u32::from_le_bytes([
                                        frame_chunk[0],
                                        frame_chunk[1],
                                        frame_chunk[2],
                                        frame_chunk[3],
                                    ]);
                                    let val_f32 = f32::from_bits(bits);
                                    let val_f32 = if val_f32.is_nan() { 0.0 } else { val_f32 };
                                    let val_s16 =
                                        (val_f32.clamp(-1.0, 1.0) * I16_MAX_F32).round() as i16;
                                    let bytes = val_s16.to_le_bytes();
                                    let out = &mut out_slice[i * STEREO_I16_BYTES_PER_FRAME
                                        ..(i + 1) * STEREO_I16_BYTES_PER_FRAME];
                                    out[0] = bytes[0];
                                    out[1] = bytes[1];
                                    out[2] = bytes[0];
                                    out[3] = bytes[1];
                                }
                            }
                            FastFormat::Generic => {
                                let start = self.pcm_buffer.len();
                                let added = frames as usize * STEREO_I16_BYTES_PER_FRAME;
                                self.pcm_buffer.resize(start + added, 0);
                                let out_slice = &mut self.pcm_buffer[start..];

                                for (i, frame_chunk) in
                                    raw_bytes.chunks_exact(frame_size).enumerate()
                                {
                                    let (out_l, out_r) =
                                        extract_stereo_f32(frame_chunk, &self.format);
                                    let l_s16 =
                                        (out_l.clamp(-1.0, 1.0) * I16_MAX_F32).round() as i16;
                                    let r_s16 =
                                        (out_r.clamp(-1.0, 1.0) * I16_MAX_F32).round() as i16;
                                    let l_bytes = l_s16.to_le_bytes();
                                    let r_bytes = r_s16.to_le_bytes();
                                    let out = &mut out_slice[i * STEREO_I16_BYTES_PER_FRAME
                                        ..(i + 1) * STEREO_I16_BYTES_PER_FRAME];
                                    out[0] = l_bytes[0];
                                    out[1] = l_bytes[1];
                                    out[2] = r_bytes[0];
                                    out[3] = r_bytes[1];
                                }
                            }
                        }
                    } else {
                        self.pcm_buffer
                            .reserve(frames as usize * STEREO_I16_BYTES_PER_FRAME);
                        for frame_chunk in raw_bytes.chunks_exact(frame_size) {
                            let sample = match fast_fmt {
                                FastFormat::StereoF32 => {
                                    let l_bits = u32::from_le_bytes([
                                        frame_chunk[0],
                                        frame_chunk[1],
                                        frame_chunk[2],
                                        frame_chunk[3],
                                    ]);
                                    let r_bits = u32::from_le_bytes([
                                        frame_chunk[4],
                                        frame_chunk[5],
                                        frame_chunk[6],
                                        frame_chunk[7],
                                    ]);
                                    let l_f32 = f32::from_bits(l_bits);
                                    let r_f32 = f32::from_bits(r_bits);
                                    let l_f32 = if l_f32.is_nan() { 0.0 } else { l_f32 };
                                    let r_f32 = if r_f32.is_nan() { 0.0 } else { r_f32 };
                                    (l_f32, r_f32)
                                }
                                FastFormat::StereoI16 => {
                                    let l_i16 =
                                        i16::from_le_bytes([frame_chunk[0], frame_chunk[1]]);
                                    let r_i16 =
                                        i16::from_le_bytes([frame_chunk[2], frame_chunk[3]]);
                                    (
                                        f32::from(l_i16) / I16_NORM_FACTOR,
                                        f32::from(r_i16) / I16_NORM_FACTOR,
                                    )
                                }
                                FastFormat::MonoF32 => {
                                    let bits = u32::from_le_bytes([
                                        frame_chunk[0],
                                        frame_chunk[1],
                                        frame_chunk[2],
                                        frame_chunk[3],
                                    ]);
                                    let val_f32 = f32::from_bits(bits);
                                    let val_f32 = if val_f32.is_nan() { 0.0 } else { val_f32 };
                                    (val_f32, val_f32)
                                }
                                FastFormat::Generic => {
                                    extract_stereo_f32(frame_chunk, &self.format)
                                }
                            };

                            let buf = &mut self.pcm_buffer;
                            self.resampler.process_frame(sample, |(out_l, out_r)| {
                                push_i16_stereo(buf, out_l, out_r);
                            });
                        }
                    }
                }

                guard.release();
            }
        }

        if !self.pcm_buffer.is_empty() {
            crate::audio_ring::push_pcm_bytes(&self.pcm_buffer);
        }
        Ok(())
    }

    fn run(&mut self, stop_event: HANDLE) -> Result<(), String> {
        let handles = [stop_event, self.audio_event];
        // SAFETY: both handles are valid for the duration of this call — the
        // stop event is owned by the capture state, the audio event by `self`.
        unsafe {
            loop {
                let ev = WaitForMultipleObjects(&handles, false, WAIT_TIMEOUT_INFINITE);
                if ev.0 == WAIT_OBJECT_0.0 {
                    return Ok(());
                }
                if ev.0 == WAIT_OBJECT_0.0 + 1 {
                    self.drain_packets()?;
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

fn build_capture_session(
    target_pid: u32,
    stop_event: Option<HANDLE>,
) -> Result<(CaptureSession, CaptureMode), String> {
    let is_system_audio =
        target_pid == SYSTEM_AUDIO_PID_DEFAULT || target_pid == SYSTEM_AUDIO_PID_ALL;

    let (client, mode, mix_format_ptr) = if is_system_audio {
        let (c, fmt) =
            activate_system_loopback().map_err(|e| format!("System loopback activation: {e}"))?;
        (c, CaptureMode::SystemLoopback, Some(fmt))
    } else {
        let build_num = os_build_number().unwrap_or(0);
        if build_num < PROCESS_LOOPBACK_MIN_BUILD {
            return Err(format!(
                "Process loopback requires Windows 10 build {PROCESS_LOOPBACK_MIN_BUILD} or higher (current build: {build_num})"
            ));
        }
        let client = activate_process_loopback(target_pid, stop_event)
            .map_err(|e| format!("Process loopback activation failed for PID {target_pid}: {e}"))?;
        (client, CaptureMode::ProcessLoopback, None)
    };

    let explicit_format = make_loopback_format();
    let format_ptr: *const WAVEFORMATEX = match mix_format_ptr.as_ref() {
        Some(ptr) => ptr.as_ptr(),
        None => &raw const explicit_format.Format,
    };

    let audio_format =
        parse_wave_format(format_ptr).map_err(|e| format!("Failed to parse audio format: {e}"))?;

    // SAFETY: `client` is a valid IAudioClient. `format_ptr` points either to the
    // COM-allocated mix format (freed via CoTaskMemPtr Drop) or to `explicit_format`.
    let (capture_client, audio_event) = unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_LOOPBACK,
                WASAPI_BUFFER_DURATION_100NS,
                0,
                format_ptr,
                None,
            )
            .map_err(|e| format!("IAudioClient::Initialize: {e}"))?;

        let capture_client = client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| format!("GetService(IAudioCaptureClient): {e}"))?;

        let audio_event = OwnedHandle::new(
            CreateEventA(None, false, false, PCSTR::null())
                .map_err(|e| format!("CreateEventA: {e}"))?,
        )?;
        client
            .SetEventHandle(audio_event.handle())
            .map_err(|e| format!("SetEventHandle: {e}"))?;
        client
            .Start()
            .map_err(|e| format!("IAudioClient::Start: {e}"))?;

        Ok::<_, String>((capture_client, audio_event.into_raw()))
    }?;

    Ok((
        CaptureSession::new(client, capture_client, audio_event, audio_format),
        mode,
    ))
}

fn run_capture(
    target_pid: u32,
    stop_handle: SendHandle,
    startup_tx: &Sender<Result<CaptureMode, String>>,
    run_rx: &std::sync::mpsc::Receiver<()>,
) -> Result<(), String> {
    // SAFETY: standard per-thread COM init on the dedicated capture thread;
    // balanced by CoUninitialize below on success.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if !hr.is_ok() && hr != HRESULT(RPC_E_CHANGED_MODE) {
        let msg = format!("CoInitializeEx failed: {hr:?}");
        let _ = startup_tx.send(Err(msg.clone()));
        return Err(msg);
    }
    let com_ok = hr == S_OK || hr == S_FALSE;

    let mut session = match build_capture_session(target_pid, Some(stop_handle.0)) {
        Ok((session, mode)) => {
            let _ = startup_tx.send(Ok(mode));
            if run_rx.recv().is_err() {
                if com_ok {
                    // SAFETY: balances the successful CoInitializeEx at the top.
                    unsafe { CoUninitialize() };
                }
                return Err("Capture start aborted by caller".into());
            }
            session
        }
        Err(e) => {
            let _ = startup_tx.send(Err(e.clone()));
            if com_ok {
                // SAFETY: balances the successful CoInitializeEx at the top.
                unsafe { CoUninitialize() };
            }
            return Err(e);
        }
    };

    let run_result = session.run(stop_handle.0);
    drop(session);
    if com_ok {
        // SAFETY: balances the successful CoInitializeEx at the top.
        unsafe { CoUninitialize() };
    }
    run_result
}

pub fn start_audio_capture(target: &AudioTarget) -> Result<bool, String> {
    MANAGER.start_audio_capture(target)
}

pub fn stop_audio_capture() -> bool {
    MANAGER.stop_audio_capture()
}

pub fn is_audio_capture_active() -> bool {
    MANAGER.is_audio_capture_active()
}

pub fn switch_audio_capture(target: &AudioTarget) -> Result<bool, String> {
    start_audio_capture(target)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "keeps the platform-module signature uniform with the fallible linux implementation"
)]
pub fn dump_audio_sources() -> Result<Vec<std::collections::HashMap<String, String>>, String> {
    Ok(Vec::new())
}

pub fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    None
}

pub fn get_capture_context() -> Result<crate::CaptureContext, String> {
    Err("Capture context introspection is only available on Linux".into())
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "keeps the platform-module signature uniform with the fallible linux implementation"
)]
pub fn start_audio_metering() -> Result<bool, String> {
    Ok(false)
}

pub fn stop_audio_metering() -> bool {
    true
}

pub fn set_wave_callback(_callback: Box<dyn Fn(Vec<crate::AudioAppWave>) + Send + Sync>) {}

pub fn clear_wave_callback() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm16(channels: u16, rate: u32, bits: u16) -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM_TAG,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(channels) * u32::from(bits / 8),
            nBlockAlign: channels * (bits / 8),
            wBitsPerSample: bits,
            cbSize: 0,
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "WAVE_FORMAT_EXTENSIBLE is 0xFFFE, a 16-bit value typed u32 by the windows crate"
    )]
    fn extensible(
        channels: u16,
        bits: u16,
        valid_bits: u16,
        subformat: GUID,
        mask: u32,
        cb_size: u16,
    ) -> WAVEFORMATEXTENSIBLE {
        WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: channels,
                nSamplesPerSec: 48_000,
                nAvgBytesPerSec: 48_000 * u32::from(channels) * u32::from(bits / 8),
                nBlockAlign: channels * (bits / 8),
                wBitsPerSample: bits,
                cbSize: cb_size,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: valid_bits,
            },
            SubFormat: subformat,
            dwChannelMask: mask,
        }
    }

    fn parse(fmt: &WAVEFORMATEX) -> Result<AudioFormat, String> {
        parse_wave_format(std::ptr::from_ref(fmt))
    }

    #[test]
    fn test_resampler_initialization() {
        let mut resampler = StereoResampler::new(44_100, 48_000);
        let mut outputs = Vec::new();
        // First frame: 1.0, 1.0
        resampler.process_frame((1.0, 1.0), |frame| outputs.push(frame));
        assert!(!outputs.is_empty());
        // The first output frame should NOT be silence (0.0, 0.0)
        assert_eq!(outputs[0], (1.0, 1.0));
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "test inputs are small integers that round-trip exactly in f32"
    )]
    fn resampler_passthrough_at_same_rate() {
        let mut resampler = StereoResampler::new(48_000, 48_000);
        let mut outputs = Vec::new();
        for i in 0..100 {
            resampler.process_frame((i as f32, -i as f32), |f| outputs.push(f));
        }
        assert_eq!(outputs.len(), 100);
        assert_eq!(outputs[50], (50.0, -50.0));
    }

    #[test]
    fn resampler_upsamples_44100_to_48000() {
        let mut resampler = StereoResampler::new(44_100, 48_000);
        let mut outputs = Vec::new();
        for _i in 0..441 {
            resampler.process_frame((1.0, 1.0), |f| outputs.push(f));
        }
        // 441 input frames must produce at least 480 output frames; the
        // phase accumulator must not drop or duplicate the last frame.
        assert!(outputs.len() >= 480, "got {} outputs", outputs.len());
        assert_eq!(outputs[0], (1.0, 1.0));
    }

    #[test]
    fn resampler_downsamples_48000_to_44100() {
        let mut resampler = StereoResampler::new(48_000, 44_100);
        let mut outputs = Vec::new();
        for _i in 0..480 {
            resampler.process_frame((1.0, 1.0), |f| outputs.push(f));
        }
        assert!(outputs.len() <= 442, "got {} outputs", outputs.len());
    }

    #[test]
    fn resampler_phase_accumulates_across_frames_without_reset() {
        let mut resampler = StereoResampler::new(44_100, 48_000);
        let mut outputs = Vec::new();
        // 1 input frame per call; the while-loop phase must keep advancing
        // so the total output count matches a single bulk pass.
        for _ in 0..882 {
            resampler.process_frame((1.0, 1.0), |f| outputs.push(f));
        }
        let mut bulk = StereoResampler::new(44_100, 48_000);
        let mut bulk_outputs = Vec::new();
        bulk.process_frame((1.0, 1.0), |f| bulk_outputs.push(f));
        for _ in 0..881 {
            bulk.process_frame((1.0, 1.0), |f| bulk_outputs.push(f));
        }
        assert_eq!(outputs.len(), bulk_outputs.len());
    }

    #[test]
    fn parse_wave_format_rejects_null_pointer() {
        assert!(parse_wave_format(std::ptr::null()).is_err());
    }

    #[test]
    fn parse_wave_format_rejects_zeroed_format() {
        let zero = WAVEFORMATEX {
            wFormatTag: 0,
            nChannels: 0,
            nSamplesPerSec: 0,
            nAvgBytesPerSec: 0,
            nBlockAlign: 0,
            wBitsPerSample: 0,
            cbSize: 0,
        };
        assert!(parse(&zero).is_err());
    }

    #[test]
    fn parse_wave_format_accepts_plain_pcm_16_stereo() {
        let fmt = pcm16(2, 48_000, 16);
        let parsed = parse(&fmt).unwrap_or_else(|e| panic!("valid PCM16: {e}"));
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.sample_rate, 48_000);
        assert_eq!(parsed.container_bits, 16);
        assert_eq!(parsed.valid_bits, 16);
        assert_eq!(parsed.sample_type, SampleType::Int);
        assert_eq!(parsed.channel_mask, None);
    }

    #[test]
    fn parse_wave_format_accepts_plain_pcm_24_and_32() {
        for bits in [24, 32] {
            let fmt = pcm16(2, 48_000, bits);
            let parsed = parse(&fmt).unwrap_or_else(|e| panic!("valid PCM: {e}"));
            assert_eq!(parsed.container_bits, bits);
            assert_eq!(parsed.valid_bits, bits);
        }
    }

    #[test]
    fn parse_wave_format_rejects_unsupported_container_bits() {
        let fmt = pcm16(2, 48_000, 20);
        assert!(parse(&fmt).is_err());
    }

    #[test]
    fn parse_wave_format_rejects_block_align_below_minimum() {
        let mut fmt = pcm16(2, 48_000, 16);
        fmt.nBlockAlign = 2; // minimum is 4 (2ch * 2 bytes)
        assert!(parse(&fmt).is_err());
    }

    #[test]
    fn parse_wave_format_rejects_unknown_format_tag() {
        let mut fmt = pcm16(2, 48_000, 16);
        fmt.wFormatTag = 0xDEAD;
        assert!(parse(&fmt).is_err());
    }

    #[test]
    fn parse_wave_format_extensible_float_reads_valid_bits_and_mask() {
        let fmt = extensible(
            2,
            32,
            24,
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            KSAUDIO_SPEAKER_STEREO,
            22,
        );
        let parsed = parse(&fmt.Format).unwrap_or_else(|e| panic!("extensible float: {e}"));
        assert_eq!(parsed.sample_type, SampleType::Float);
        assert_eq!(parsed.valid_bits, 24);
        assert_eq!(parsed.channel_mask, Some(KSAUDIO_SPEAKER_STEREO));
    }

    #[test]
    fn parse_wave_format_extensible_pcm_subformat() {
        let fmt = extensible(1, 16, 16, KSDATAFORMAT_SUBTYPE_PCM, 0, 22);
        let parsed = parse(&fmt.Format).unwrap_or_else(|e| panic!("extensible pcm: {e}"));
        assert_eq!(parsed.sample_type, SampleType::Int);
        assert_eq!(parsed.channel_mask, None);
    }

    #[test]
    fn parse_wave_format_rejects_unsupported_extensible_subformat() {
        let bogus = GUID::from_u128(0xDEAD_BEEF_CAFE_F00D_1234_5678_9ABC_DEF0);
        let fmt = extensible(2, 32, 32, bogus, 0, 22);
        assert!(parse(&fmt.Format).is_err());
    }

    #[test]
    fn parse_wave_format_rejects_oversized_valid_bits() {
        let fmt = extensible(2, 16, 17, KSDATAFORMAT_SUBTYPE_PCM, 0, 22);
        assert!(parse(&fmt.Format).is_err());

        let fmt_f32 = extensible(2, 32, 24, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, 0, 22);
        assert!(parse(&fmt_f32.Format).is_err());
    }

    #[test]
    fn parse_wave_format_rejects_extensible_without_subformat_bytes() {
        let fmt = extensible(2, 32, 32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, 0, 0);
        assert!(parse(&fmt.Format).is_err());
    }

    #[test]
    fn fast_format_classifies_common_layouts() {
        let f32st = AudioFormat {
            channels: 2,
            sample_rate: 48_000,
            container_bits: 32,
            valid_bits: 32,
            block_align: 8,
            sample_type: SampleType::Float,
            channel_mask: None,
        };
        assert_eq!(f32st.fast_format(), FastFormat::StereoF32);

        let i16st = AudioFormat {
            channels: 2,
            sample_rate: 48_000,
            container_bits: 16,
            valid_bits: 16,
            block_align: 4,
            sample_type: SampleType::Int,
            channel_mask: None,
        };
        assert_eq!(i16st.fast_format(), FastFormat::StereoI16);

        let f32mono = AudioFormat {
            channels: 1,
            sample_rate: 48_000,
            container_bits: 32,
            valid_bits: 32,
            block_align: 4,
            sample_type: SampleType::Float,
            channel_mask: None,
        };
        assert_eq!(f32mono.fast_format(), FastFormat::MonoF32);
    }

    #[test]
    fn fast_format_generic_for_everything_else() {
        let generic = AudioFormat {
            channels: 6,
            sample_rate: 48_000,
            container_bits: 24,
            valid_bits: 24,
            block_align: 18,
            sample_type: SampleType::Int,
            channel_mask: Some(0x3F),
        };
        assert_eq!(generic.fast_format(), FastFormat::Generic);
    }

    #[test]
    fn extract_stereo_f32_duplicates_mono_channel() {
        let fmt = AudioFormat {
            channels: 1,
            sample_rate: 48_000,
            container_bits: 32,
            valid_bits: 32,
            block_align: 4,
            sample_type: SampleType::Float,
            channel_mask: None,
        };
        let frame = 0.5f32.to_le_bytes();
        let (l, r) = extract_stereo_f32(&frame, &fmt);
        assert!((l - 0.5).abs() < 1e-6);
        assert!((r - 0.5).abs() < 1e-6);
    }

    #[test]
    fn extract_stereo_f32_converts_i16_with_valid_bits_shift() {
        let fmt = AudioFormat {
            channels: 2,
            sample_rate: 48_000,
            container_bits: 16,
            valid_bits: 12,
            block_align: 4,
            sample_type: SampleType::Int,
            channel_mask: None,
        };
        // 0x0800 = 2048 with only 12 valid bits: shifted up to 0x8000, i.e. -1.0.
        let frame = [0x00, 0x08, 0x00, 0x08];
        let (l, r) = extract_stereo_f32(&frame, &fmt);
        assert!((l - (-1.0)).abs() < 1e-4, "l = {l}");
        assert!((r - (-1.0)).abs() < 1e-4, "r = {r}");
    }

    #[test]
    fn extract_stereo_f32_sign_extends_i24() {
        let fmt = AudioFormat {
            channels: 1,
            sample_rate: 48_000,
            container_bits: 24,
            valid_bits: 24,
            block_align: 3,
            sample_type: SampleType::Int,
            channel_mask: None,
        };
        // -0.5 in 24-bit: 0xFFC00000 >> 8 = 0xFFC000.
        let frame = [0x00, 0xC0, 0xFF];
        let (l, _r) = extract_stereo_f32(&frame, &fmt);
        assert!((l - (-0.5)).abs() < 1e-4, "l = {l}");
    }

    #[test]
    fn extract_stereo_f32_returns_silence_for_nan_and_short_frames() {
        let fmt = AudioFormat {
            channels: 2,
            sample_rate: 48_000,
            container_bits: 32,
            valid_bits: 32,
            block_align: 8,
            sample_type: SampleType::Float,
            channel_mask: None,
        };
        let mut nan_frame = [0u8; 8];
        nan_frame[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        nan_frame[4..].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(extract_stereo_f32(&nan_frame, &fmt), (0.0, 0.0));

        assert_eq!(extract_stereo_f32(&[0u8; 4], &fmt), (0.0, 0.0));
    }

    #[test]
    fn validate_target_pid_maps_system_audio_sentinels() {
        assert_eq!(
            validate_target_pid(-1).unwrap_or_else(|e| panic!("system audio: {e}")),
            SYSTEM_AUDIO_PID_DEFAULT
        );
        assert_eq!(
            validate_target_pid(0).unwrap_or_else(|e| panic!("system audio: {e}")),
            SYSTEM_AUDIO_PID_DEFAULT
        );
        assert_eq!(
            validate_target_pid(1234).unwrap_or_else(|e| panic!("pid: {e}")),
            1234
        );
    }

    #[test]
    fn validate_target_pid_rejects_other_negatives() {
        assert!(validate_target_pid(-2).is_err());
        assert!(validate_target_pid(i32::MIN).is_err());
    }
}
