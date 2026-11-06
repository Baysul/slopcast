//! The presenter session: room lifecycle, capture stages, audio selection, endpoint
//! validation and live encoder updates.
//!
//! This is the orchestration that previously lived in the React renderer (`main.tsx`,
//! `useNativeRoom.ts`, `useAudioCapture.ts`, `useStreamSettings.ts`). It runs on an owned
//! Tokio runtime and publishes [`Snapshot`]s through a watch channel, so hiding or closing
//! windows never owns or stops the session.
//!
//! Conflicting capture operations are rejected while another is in flight, like the renderer's
//! busy guards. Every asynchronous completion is tagged with a capture session, room generation
//! or request generation, so late portal, HTTP and native results cannot revive a stopped
//! session; resources such completions create are released.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};

use crate::api::{HeartbeatResult, PreparedRoom, RoomApi};
use crate::audio::{self, AudioApp, AudioAppGroup, NO_AUDIO, SYSTEM_AUDIO};
use crate::bitrate::recommend_bitrate_cap;
use crate::codecs::{self, CodecInfo};
use crate::config::AppConfig;
use crate::media::{self, CaptureContextView, CaptureSource, SourceSelection};
use crate::platform::PlatformInfo;
use crate::preferences::{
    AppearanceMode, ColorTheme, DesktopPreferences, TypographyPreferences,
    default_telemetry_shortcut,
};
use crate::settings::{StreamSettings, resolution_dims};
use crate::shortcuts::{ShortcutReport, validate_bindings, validate_telemetry_shortcut};
use crate::storage::SettingsStorage;
use crate::telemetry::{StreamTelemetry, TelemetryTracker, TickInputs};

const ROOM_POLL: Duration = Duration::from_secs(1);
// Well inside the server's presenter grace period, so a reachable presenter is never reaped.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
// Brief blips that LiveKit resumes on its own are not worth a notice.
const RECONNECT_NOTICE_DELAY: Duration = Duration::from_secs(3);
const ROOM_RETRY_WINDOW: Duration = Duration::from_secs(45);
const TELEMETRY_INTERVAL: Duration = Duration::from_secs(1);
const AUDIO_APPS_INTERVAL: Duration = Duration::from_secs(3);
const LIVE_UPDATE_DEBOUNCE: Duration = Duration::from_millis(300);
const SHUTDOWN_ROOM_TIMEOUT: Duration = Duration::from_secs(3);
const EVENT_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureStage {
    #[default]
    Idle,
    Previewing,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomConnection {
    Connecting,
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomView {
    pub code: String,
    pub share_url: String,
    pub api_endpoint: String,
    pub connection: RoomConnection,
    pub spectators: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndpointAvailability {
    Typing,
    #[default]
    Checking,
    Healthy,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EndpointHealth {
    pub target: Option<String>,
    pub availability: EndpointAvailability,
    pub healthy: bool,
    pub message: Option<String>,
}

/// The Settings field's own edit, while it differs from the active or pending endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointDraft {
    pub text: String,
    pub availability: EndpointAvailability,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioView {
    pub apps: Vec<AudioApp>,
    pub groups: Vec<AudioAppGroup>,
    /// Capture target id (`-pid` on Linux). `None` means automatic and still undecided.
    pub selected: Option<i32>,
    pub explicit: bool,
    pub auto_detected: Option<AudioApp>,
    pub auto_detect_failed: bool,
    pub capture_context: Option<CaptureContextView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub platform: PlatformInfo,
    pub hydrated: bool,
    pub room: Option<RoomView>,
    pub creating_room: bool,
    pub closing_room: bool,
    pub room_transitioning: bool,
    pub retryable_replacement: bool,
    pub capture: CaptureStage,
    pub capture_busy: bool,
    pub picker_open: bool,
    pub muted: bool,
    pub audio: AudioView,
    pub telemetry: StreamTelemetry,
    /// Hydrated settings: `api_endpoint` is the endpoint in use.
    pub settings: StreamSettings,
    pub codecs: Vec<CodecInfo>,
    pub effective_bitrate: f64,
    pub endpoint: EndpointHealth,
    pub endpoint_draft: Option<EndpointDraft>,
    pub endpoint_switch_candidate: Option<String>,
    pub preferences: DesktopPreferences,
    pub preferences_error: Option<String>,
    pub settings_directory: String,
    pub onboarding_completed: bool,
    pub shortcuts: ShortcutReport,
}

impl Snapshot {
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.capture_busy || self.room_transitioning
    }

    #[must_use]
    pub fn can_create_room(&self) -> bool {
        self.hydrated
            && self.endpoint.healthy
            && self.endpoint.availability != EndpointAvailability::Checking
            && !self.room_transitioning
            && !self.creating_room
            && self.room.is_none()
    }

    #[must_use]
    pub fn room_create_disabled_reason(&self) -> Option<&'static str> {
        if !self.hydrated {
            return Some("Loading saved API endpoint settings.");
        }
        match self.endpoint.availability {
            EndpointAvailability::Checking => {
                Some("Checking the API endpoint before room creation.")
            }
            _ if self.endpoint.healthy => None,
            EndpointAvailability::Error => {
                Some("Fix the API endpoint in Settings → Stream before creating a room.")
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn can_start_share(&self) -> bool {
        self.room.is_some()
            && self.capture == CaptureStage::Idle
            && !self.room_transitioning
            && !self.capture_busy
            && !self.picker_open
    }

    /// The endpoint the Settings field shows when it has no edit of its own.
    #[must_use]
    pub fn source_endpoint(&self) -> &str {
        self.settings
            .pending_api_endpoint
            .as_deref()
            .unwrap_or(&self.settings.api_endpoint)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopAction {
    StartShare,
    StopShare,
    CloseRoom,
    SwitchSource,
    ToggleMute,
    Quit,
}

impl DesktopAction {
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "start-share" => Self::StartShare,
            "stop-share" => Self::StopShare,
            "close-room" => Self::CloseRoom,
            "switch-source" => Self::SwitchSource,
            "toggle-mute" => Self::ToggleMute,
            "quit" => Self::Quit,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    CreateRoom,
    /// Close the room after the person confirmed it.
    CloseRoom,
    RetryRoomConnection,
    StartShare,
    GoLive,
    StopShare,
    SwitchSource,
    SourceSelected(SourceSelection),
    CancelSourcePicker,
    ToggleMute,
    /// An app id from the picker, [`SYSTEM_AUDIO`] or [`NO_AUDIO`].
    SelectAudio(i32),
    RefreshAudioApps,
    /// The Settings field's text changed; validation follows separately.
    EndpointDraftEdited(String),
    ValidateEndpointDraft(String),
    EndpointReset,
    ConfirmEndpointSwitch,
    DeferEndpointSwitch,
    /// Changed stream-settings fields, in the file's camelCase keys.
    UpdateStreamSettings(Map<String, Value>),
    Action(DesktopAction),
    /// Quit after the person confirmed; `anyway` skips the room closure retry.
    Quit {
        anyway: bool,
    },
    SetMainVisible(bool),
    CaptureEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeScope {
    Main,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub scope: NoticeScope,
    pub title: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    CloseRoom,
    Quit,
    QuitAnyway,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Notice(Notice),
    ShowMain,
    Confirm(Confirmation),
    OpenSourcePicker(Vec<CaptureSource>),
    /// Every pending room and capture operation is settled; exit now.
    QuitNow,
}

#[derive(Debug, Clone, PartialEq)]
struct ConfigKey {
    auto: bool,
    bitrate: u64,
    codec: String,
    fps: u64,
    width: u32,
    height: u32,
}

struct Replacement {
    room: PreparedRoom,
    expires_at: Instant,
    resume_sharing: bool,
}

struct Private {
    view: Snapshot,
    room: Option<PreparedRoom>,
    room_generation: u64,
    saw_connected: bool,
    lost_since: Option<Instant>,
    announced_loss: bool,
    replacement: Option<Replacement>,
    endpoint_revision: u32,
    selected_source: Option<SourceSelection>,
    tray_share: bool,
    stop_busy: bool,
    room_action: bool,
    active_audio: Option<i32>,
    live_config_key: Option<ConfigKey>,
    active_codec: String,
    telemetry: Option<TelemetryTracker>,
    config_endpoint: String,
    main_visible: bool,
}

impl Private {
    fn auto_select(&self) -> bool {
        self.view.preferences.auto_select_audio
    }

    fn reset_audio_choice(&mut self) {
        let audio = &mut self.view.audio;
        audio.explicit = false;
        audio.auto_detected = None;
        audio.selected = if self.view.preferences.auto_select_audio {
            None
        } else {
            Some(NO_AUDIO)
        };
        audio.auto_detect_failed = false;
    }

    fn clear_room(&mut self) {
        self.room = None;
        self.room_generation += 1;
        self.saw_connected = false;
        self.lost_since = None;
        self.announced_loss = false;
        self.view.room = None;
    }

    fn stop_telemetry(&mut self) {
        self.telemetry = None;
        self.view.telemetry = StreamTelemetry::default();
    }

    fn effective_bitrate(&self) -> f64 {
        let s = &self.view.settings;
        if s.auto_bitrate {
            recommend_bitrate_cap(&s.video_codec, &s.resolution, s.fps, "static")
        } else {
            s.bitrate_limit
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bitrates and frame rates are sanitized, finite and positive"
    )]
    fn config_key(&self) -> ConfigKey {
        let s = &self.view.settings;
        let (width, height) = resolution_dims(&s.resolution);
        ConfigKey {
            auto: s.auto_bitrate,
            bitrate: self.effective_bitrate().round() as u64,
            codec: s.video_codec.clone(),
            fps: (s.fps * 1000.0).round() as u64,
            width,
            height,
        }
    }

    fn capture_config(&self) -> native_livekit::CaptureConfig {
        media::capture_config(&self.view.settings, self.effective_bitrate())
    }

    fn tick_inputs(&self) -> TickInputs {
        let (width, height) = resolution_dims(&self.view.settings.resolution);
        TickInputs {
            width,
            height,
            target_frame_rate: self.view.settings.fps,
            has_audio: self.active_audio.is_some_and(|id| id != NO_AUDIO),
            spectator_count: self.view.room.as_ref().map_or(0, |r| r.spectators),
        }
    }

    fn endpoint_inputs(&self) -> (String, Option<String>, u32, bool) {
        (
            self.view.settings.api_endpoint.clone(),
            self.view.settings.pending_api_endpoint.clone(),
            self.endpoint_revision,
            self.view.hydrated,
        )
    }

    /// Applies saved settings, keeping the configured endpoint unless a custom one was saved.
    fn hydrate_settings(&mut self, saved: StreamSettings) {
        let mut settings = saved;
        if !self.view.codecs.is_empty()
            && !self
                .view
                .codecs
                .iter()
                .any(|c| c.codec == settings.video_codec)
        {
            settings.video_codec = self.view.codecs[0].codec.clone();
        }
        if !settings.api_endpoint_is_custom && !self.config_endpoint.is_empty() {
            settings.api_endpoint.clone_from(&self.config_endpoint);
        }
        self.view.settings = settings;
        self.view.effective_bitrate = self.effective_bitrate();
    }
}

enum CaptureOp {
    Preview,
    Replace(Option<SourceSelection>),
    GoLive,
}

struct Inner {
    runtime: tokio::runtime::Handle,
    api: RoomApi,
    storage: Arc<SettingsStorage>,
    config: AppConfig,
    state: Mutex<Private>,
    snapshots: watch::Sender<Snapshot>,
    events: mpsc::Sender<Event>,
    capture_session: AtomicU64,
    audio_revision: AtomicU64,
    audio_lock: AsyncMutex<()>,
    endpoint_check: AtomicU64,
    draft_check: AtomicU64,
    live_update: AtomicU64,
    weak: Weak<Inner>,
}

/// The process-lifetime presenter. Dropping it without [`Presenter::shutdown`] leaves cleanup
/// to the native engines' own drop paths; call `shutdown` on quit.
pub struct Presenter {
    runtime: Option<tokio::runtime::Runtime>,
    inner: Arc<Inner>,
    events: Mutex<Option<mpsc::Receiver<Event>>>,
}

/// A clonable command/snapshot handle. The owning presenter still controls runtime shutdown.
#[derive(Clone)]
pub struct PresenterClient {
    inner: Arc<Inner>,
}

impl PresenterClient {
    pub fn dispatch(&self, command: Command) {
        self.inner.dispatch(command);
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.inner.snapshots.borrow().clone()
    }
}

pub struct PresenterOptions {
    pub config: AppConfig,
    pub storage: Arc<SettingsStorage>,
    pub platform: PlatformInfo,
}

impl Presenter {
    #[must_use]
    pub fn client(&self) -> PresenterClient {
        PresenterClient {
            inner: Arc::clone(&self.inner),
        }
    }
    /// Starts the presenter runtime and registers native callbacks.
    ///
    /// # Errors
    /// Returns an error if the runtime cannot start.
    pub fn start(options: PresenterOptions) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("slopcast-core")
            .enable_all()
            .build()
            .map_err(|error| format!("Cannot start the presenter runtime: {error}"))?;
        let storage = options.storage;
        let (preferences, preferences_error) = match storage.read_preferences() {
            Ok(preferences) => (preferences, None),
            Err(error) => (DesktopPreferences::default(), Some(error)),
        };
        let mut private = Private {
            view: Snapshot {
                platform: options.platform,
                hydrated: false,
                room: None,
                creating_room: false,
                closing_room: false,
                room_transitioning: false,
                retryable_replacement: false,
                capture: CaptureStage::Idle,
                capture_busy: false,
                picker_open: false,
                muted: media::is_muted(),
                audio: AudioView::default(),
                telemetry: StreamTelemetry::default(),
                settings: StreamSettings::default(),
                codecs: Vec::new(),
                effective_bitrate: 0.0,
                endpoint: EndpointHealth::default(),
                endpoint_draft: None,
                endpoint_switch_candidate: None,
                preferences,
                preferences_error,
                settings_directory: storage
                    .current_directory()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default(),
                onboarding_completed: storage.onboarding_completed(),
                shortcuts: ShortcutReport::default(),
            },
            room: None,
            room_generation: 0,
            saw_connected: false,
            lost_since: None,
            announced_loss: false,
            replacement: None,
            endpoint_revision: 0,
            selected_source: None,
            tray_share: false,
            stop_busy: false,
            room_action: false,
            active_audio: None,
            live_config_key: None,
            active_codec: String::new(),
            telemetry: None,
            config_endpoint: options.config.api_endpoint.clone(),
            main_visible: true,
        };
        private.reset_audio_choice();
        let (snapshots, _) = watch::channel(private.view.clone());
        let (events, receiver) = mpsc::channel(EVENT_CAPACITY);
        let inner = Arc::new_cyclic(|weak| Inner {
            runtime: runtime.handle().clone(),
            api: RoomApi::new(),
            storage,
            config: options.config,
            state: Mutex::new(private),
            snapshots,
            events,
            capture_session: AtomicU64::new(0),
            audio_revision: AtomicU64::new(0),
            audio_lock: AsyncMutex::new(()),
            endpoint_check: AtomicU64::new(0),
            draft_check: AtomicU64::new(0),
            live_update: AtomicU64::new(0),
            weak: weak.clone(),
        });

        let weak = Arc::downgrade(&inner);
        media::register_callbacks(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.dispatch(Command::CaptureEnded);
            }
        }));
        let hydrate = Arc::clone(&inner);
        runtime.spawn(async move { hydrate.hydrate().await });

        Ok(Self {
            runtime: Some(runtime),
            inner,
            events: Mutex::new(Some(receiver)),
        })
    }

    /// Queues a command. Never blocks the caller.
    pub fn dispatch(&self, command: Command) {
        self.inner.dispatch(command);
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.inner.snapshots.subscribe()
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.inner.snapshots.borrow().clone()
    }

    /// The event stream. Available once.
    pub fn take_events(&self) -> Option<mpsc::Receiver<Event>> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    #[must_use]
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.inner.runtime.clone()
    }

    #[must_use]
    pub fn storage(&self) -> Arc<SettingsStorage> {
        Arc::clone(&self.inner.storage)
    }

    /// # Errors
    /// Returns an error if the preferences cannot be saved.
    pub fn save_general(&self, tray_enabled: bool, auto_select_audio: bool) -> Result<(), String> {
        let saved = self.inner.storage.update_preferences(|p| {
            p.tray_enabled = tray_enabled;
            p.auto_select_audio = auto_select_audio;
            Ok(())
        })?;
        let inner = Arc::clone(&self.inner);
        let auto_changed = inner.update(|p| {
            let changed = p.view.preferences.auto_select_audio != saved.auto_select_audio;
            p.view.preferences = saved;
            p.view.preferences_error = None;
            if changed && !p.view.audio.explicit {
                p.reset_audio_choice();
            }
            changed && !p.view.audio.explicit
        });
        if auto_changed {
            inner.audio_revision.fetch_add(1, Ordering::AcqRel);
            if !auto_select_audio {
                let task = Arc::clone(&inner);
                inner.runtime.spawn(async move {
                    if let Err(error) = task.stop_audio().await {
                        task.notice(
                            NoticeLevel::Error,
                            NoticeScope::Main,
                            "Audio stop failed",
                            &error,
                        );
                    }
                });
            }
        }
        Ok(())
    }

    /// # Errors
    /// Returns an error if the preferences cannot be saved.
    pub fn save_appearance(&self, mode: AppearanceMode, theme: ColorTheme) -> Result<(), String> {
        self.save_preferences(|p| {
            p.appearance_mode = mode;
            p.color_theme = theme;
            Ok(())
        })
    }

    /// # Errors
    /// Returns an error if the typography is invalid or cannot be saved.
    pub fn save_typography(&self, typography: TypographyPreferences) -> Result<(), String> {
        typography.validate()?;
        self.save_preferences(|p| {
            p.typography = typography;
            Ok(())
        })
    }

    /// Saves the in-window telemetry overlay shortcut; `None` restores the default.
    ///
    /// # Errors
    /// Returns an error if the chord is invalid or conflicts with a global shortcut.
    pub fn save_telemetry_shortcut(&self, chord: Option<String>) -> Result<(), String> {
        let chord = chord.unwrap_or_else(default_telemetry_shortcut);
        let reported = self.snapshot().shortcuts.bindings;
        self.save_preferences(|p| {
            validate_telemetry_shortcut(&chord, p.shortcuts.values().chain(reported.values()))?;
            p.telemetry_shortcut = chord;
            Ok(())
        })
    }

    /// Validates global action bindings before the shell registers them.
    ///
    /// # Errors
    /// Returns the user-facing validation failure.
    pub fn validate_shortcuts(&self, bindings: &BTreeMap<String, String>) -> Result<(), String> {
        let snapshot = self.snapshot();
        validate_bindings(bindings, snapshot.platform.is_wayland)?;
        validate_telemetry_shortcut(&snapshot.preferences.telemetry_shortcut, bindings.values())
    }

    /// Persists global action bindings the shell registered successfully.
    ///
    /// # Errors
    /// Returns an error if the preferences cannot be saved.
    pub fn save_shortcuts(&self, bindings: BTreeMap<String, String>) -> Result<(), String> {
        self.save_preferences(|p| {
            p.shortcuts = bindings;
            Ok(())
        })
    }

    fn save_preferences(
        &self,
        edit: impl FnOnce(&mut DesktopPreferences) -> Result<(), String>,
    ) -> Result<(), String> {
        let saved = self.inner.storage.update_preferences(edit)?;
        self.inner.update(|p| {
            p.view.preferences = saved;
            p.view.preferences_error = None;
        });
        Ok(())
    }

    pub fn set_shortcut_report(&self, report: ShortcutReport) {
        self.inner.update(|p| p.view.shortcuts = report);
    }

    /// Moves the settings into `path` and reloads them from there.
    ///
    /// # Errors
    /// Returns an error if the folder is unusable or already holds settings.
    pub fn set_settings_directory(&self, path: &std::path::Path) -> Result<(), String> {
        let directory = self.inner.storage.move_to(path)?;
        let preferences = self.inner.storage.read_preferences();
        let saved = self.inner.storage.read_stream_settings()?;
        let onboarding = self.inner.storage.onboarding_completed();
        self.inner.update(|p| {
            p.view.settings_directory = directory.display().to_string();
            match preferences {
                Ok(preferences) => {
                    p.view.preferences = preferences;
                    p.view.preferences_error = None;
                }
                Err(error) => p.view.preferences_error = Some(error),
            }
            p.view.onboarding_completed = onboarding;
            p.hydrate_settings(saved);
        });
        Ok(())
    }

    /// # Errors
    /// Returns an error if the onboarding file cannot be written.
    pub fn complete_onboarding(&self) -> Result<(), String> {
        self.inner.storage.set_onboarding_completed()?;
        self.inner.update(|p| p.view.onboarding_completed = true);
        Ok(())
    }

    /// Ends the session: bounded best-effort room deletion, then every native component.
    /// Call from outside the runtime (the UI thread) when quitting.
    pub fn shutdown(mut self) {
        let inner = Arc::clone(&self.inner);
        if let Some(runtime) = self.runtime.take() {
            runtime.block_on(async {
                inner.capture_session.fetch_add(1, Ordering::AcqRel);
                let room = inner.update(|p| {
                    let room = p
                        .room
                        .take()
                        .or_else(|| p.replacement.take().map(|r| r.room));
                    p.clear_room();
                    room
                });
                if let Some(room) = room {
                    let delete =
                        tokio::time::timeout(SHUTDOWN_ROOM_TIMEOUT, inner.api.delete_room(&room));
                    match delete.await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => log::warn!("Room {} was not deleted: {error}", room.code),
                        Err(_) => log::warn!("Room {} deletion timed out", room.code),
                    }
                }
                let _ = tokio::task::spawn_blocking(|| {
                    let _ = native_livekit::disconnect_livekit_room();
                    media::shutdown();
                })
                .await;
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| format!("native task failed: {error}"))
}

impl Inner {
    fn arc(&self) -> Option<Arc<Self>> {
        self.weak.upgrade()
    }

    fn dispatch(&self, command: Command) {
        let Some(inner) = self.arc() else {
            return;
        };
        self.runtime
            .spawn(async move { inner.handle(command).await });
    }

    fn spawn(&self, work: impl std::future::Future<Output = ()> + Send + 'static) {
        self.runtime.spawn(work);
    }

    /// Mutates state under the lock and publishes the new snapshot before releasing it, so
    /// snapshots are never published out of order. Re-checks the endpoint when its inputs change.
    fn update<R>(&self, edit: impl FnOnce(&mut Private) -> R) -> R {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let before = guard.endpoint_inputs();
        let result = edit(&mut guard);
        let after = guard.endpoint_inputs();
        self.snapshots.send_replace(guard.view.clone());
        drop(guard);
        if before != after && after.3 {
            self.check_endpoint();
        }
        result
    }

    fn read<R>(&self, read: impl FnOnce(&Private) -> R) -> R {
        read(&self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn emit(&self, event: Event) {
        if let Err(error) = self.events.try_send(event) {
            log::warn!("Presenter event dropped: {error}");
        }
    }

    fn notice(&self, level: NoticeLevel, scope: NoticeScope, title: &str, message: &str) {
        self.emit(Event::Notice(Notice {
            level,
            scope,
            title: title.into(),
            message: message.into(),
        }));
    }

    fn session(&self) -> u64 {
        self.capture_session.load(Ordering::Acquire)
    }

    fn next_session(&self) -> u64 {
        self.capture_session.fetch_add(1, Ordering::AcqRel) + 1
    }

    async fn hydrate(self: Arc<Self>) {
        let (codecs, saved) = match blocking({
            let storage = Arc::clone(&self.storage);
            move || {
                let codecs = codecs::recommend_codec(&codecs::from_native(
                    &native_livekit::get_native_supported_codecs(),
                ));
                (codecs, storage.read_stream_settings())
            }
        })
        .await
        {
            Ok(result) => result,
            Err(error) => (Vec::new(), Err(error)),
        };
        let saved = saved.unwrap_or_else(|error| {
            log::error!("Stream settings unavailable: {error}");
            StreamSettings::default()
        });
        self.update(|p| {
            p.view.codecs = codecs;
            p.hydrate_settings(saved);
            p.view.hydrated = true;
        });
        match blocking(media::start_metering).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) | Err(error) => log::warn!("Audio metering unavailable: {error}"),
        }
        let poll = Arc::clone(&self);
        self.spawn(async move { poll.poll_audio_apps().await });
    }

    async fn handle(self: Arc<Self>, command: Command) {
        match command {
            Command::CreateRoom => {
                self.create_room().await;
            }
            Command::CloseRoom => {
                self.handle_close_room().await;
            }
            Command::RetryRoomConnection => self.retry_room_connection().await,
            Command::StartShare => self.start_share().await,
            Command::GoLive => self.go_live().await,
            Command::StopShare => self.stop_share().await,
            Command::SwitchSource => self.switch_source().await,
            Command::SourceSelected(selection) => {
                self.update(|p| p.view.picker_open = false);
                self.run_capture_action(CaptureOp::Replace(Some(selection)))
                    .await;
            }
            Command::CancelSourcePicker => self.update(|p| p.view.picker_open = false),
            Command::ToggleMute => {
                let muted = media::toggle_mute();
                self.update(|p| p.view.muted = muted);
            }
            Command::SelectAudio(id) => self.select_audio(id).await,
            Command::RefreshAudioApps => self.load_audio_apps().await,
            Command::EndpointDraftEdited(text) => {
                self.draft_check.fetch_add(1, Ordering::AcqRel);
                self.update(|p| {
                    p.view.endpoint_draft = Some(EndpointDraft {
                        text,
                        availability: EndpointAvailability::Typing,
                        message: None,
                    });
                });
            }
            Command::ValidateEndpointDraft(text) => self.validate_draft(text).await,
            Command::EndpointReset => {
                self.draft_check.fetch_add(1, Ordering::AcqRel);
                self.update(|p| {
                    p.view.endpoint_switch_candidate = None;
                    p.view.endpoint_draft = None;
                    p.view.settings.pending_api_endpoint = None;
                });
                self.persist_endpoint_fields().await;
            }
            Command::ConfirmEndpointSwitch => self.replace_room().await,
            Command::DeferEndpointSwitch => {
                let changed = self.update(|p| {
                    let Some(candidate) = p.view.endpoint_switch_candidate.take() else {
                        return false;
                    };
                    p.view.settings.pending_api_endpoint = Some(candidate);
                    true
                });
                if changed {
                    self.persist_endpoint_fields().await;
                }
            }
            Command::UpdateStreamSettings(changes) => self.update_stream_settings(changes).await,
            Command::Action(action) => self.handle_action(action).await,
            Command::Quit { anyway } => self.quit(anyway).await,
            Command::SetMainVisible(visible) => {
                let refresh = self.update(|p| {
                    let became_visible = visible && !p.main_visible;
                    p.main_visible = visible;
                    became_visible
                });
                if refresh {
                    self.load_audio_apps().await;
                }
            }
            Command::CaptureEnded => {
                if self.read(|p| p.view.capture) != CaptureStage::Idle {
                    self.notice(
                        NoticeLevel::Info,
                        NoticeScope::Main,
                        "Stream ended",
                        "The captured window was closed, so sharing stopped.",
                    );
                    self.stop_share().await;
                }
            }
        }
    }

    async fn handle_action(self: &Arc<Self>, action: DesktopAction) {
        if matches!(
            action,
            DesktopAction::SwitchSource | DesktopAction::CloseRoom | DesktopAction::Quit
        ) {
            self.emit(Event::ShowMain);
        }
        match action {
            DesktopAction::StartShare => match self.read(|p| p.view.capture) {
                CaptureStage::Previewing => self.go_live().await,
                CaptureStage::Idle => self.start_share_from_tray().await,
                CaptureStage::Live => {}
            },
            DesktopAction::StopShare => self.stop_share().await,
            DesktopAction::SwitchSource => self.switch_source().await,
            DesktopAction::ToggleMute => {
                let muted = media::toggle_mute();
                self.update(|p| p.view.muted = muted);
            }
            DesktopAction::CloseRoom => {
                if self.read(|p| p.room.is_some()) {
                    self.emit(Event::Confirm(Confirmation::CloseRoom));
                }
            }
            DesktopAction::Quit => {
                let active = self.read(|p| {
                    p.room.is_some() || p.view.capture != CaptureStage::Idle || p.view.capture_busy
                });
                self.emit(if active {
                    Event::Confirm(Confirmation::Quit)
                } else {
                    Event::QuitNow
                });
            }
        }
    }

    async fn quit(self: &Arc<Self>, anyway: bool) {
        self.stop_share().await;
        if !anyway && self.read(|p| p.room.is_some()) && !self.handle_close_room().await {
            self.emit(Event::Confirm(Confirmation::QuitAnyway));
            return;
        }
        self.emit(Event::QuitNow);
    }

    // ----- Rooms ---------------------------------------------------------------------------

    async fn create_room(self: &Arc<Self>) -> bool {
        let endpoint = self.update(|p| {
            if p.view.creating_room || p.room.is_some() || p.view.room_transitioning {
                return None;
            }
            p.reset_audio_choice();
            p.view.creating_room = true;
            Some(p.view.settings.api_endpoint.clone())
        });
        let Some(endpoint) = endpoint else {
            return false;
        };
        let room = match self.api.create_room(&endpoint).await {
            Ok(room) => room,
            Err(error) => {
                log::error!("Failed to prepare room: {error}");
                self.update(|p| p.view.creating_room = false);
                self.notice(
                    NoticeLevel::Error,
                    NoticeScope::Main,
                    "Room creation failed",
                    &error,
                );
                return false;
            }
        };
        let connected = self.connect_prepared(&room).await;
        self.update(|p| p.view.creating_room = false);
        if let Err(error) = connected {
            self.discard_room(&room).await;
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Room connection failed",
                &error,
            );
            return false;
        }
        true
    }

    async fn connect_prepared(self: &Arc<Self>, room: &PreparedRoom) -> Result<(), String> {
        let url = if room.livekit_url.is_empty() {
            self.config.livekit_url.clone()
        } else {
            room.livekit_url.clone()
        };
        let (token, code, identity) =
            (room.token.clone(), room.code.clone(), room.identity.clone());
        blocking(move || native_livekit::connect_livekit_room(url, token, code, identity))
            .await??;
        let generation = self.update(|p| {
            p.clear_room();
            p.room = Some(room.clone());
            p.view.room = Some(RoomView {
                code: room.code.clone(),
                share_url: room.share_url.clone(),
                api_endpoint: room.api_endpoint.clone(),
                connection: RoomConnection::Connecting,
                spectators: 0,
            });
            p.room_generation
        });
        let monitor = Arc::clone(self);
        self.spawn(async move { monitor.monitor_room(generation).await });
        let heartbeat = Arc::clone(self);
        self.spawn(async move { heartbeat.heartbeat(generation).await });
        Ok(())
    }

    async fn discard_room(&self, room: &PreparedRoom) {
        if let Err(error) = self.api.delete_room(room).await {
            log::warn!("Failed to discard prepared room {}: {error}", room.code);
        }
    }

    fn current_room(&self, generation: u64) -> Option<PreparedRoom> {
        self.read(|p| {
            (p.room_generation == generation && !p.view.closing_room)
                .then(|| p.room.clone())
                .flatten()
        })
    }

    async fn monitor_room(self: Arc<Self>, generation: u64) {
        loop {
            if self.read(|p| p.room_generation != generation) {
                return;
            }
            if let Some(room) = self.current_room(generation) {
                self.poll_room(&room, generation).await;
            }
            tokio::time::sleep(ROOM_POLL).await;
        }
    }

    async fn poll_room(self: &Arc<Self>, room: &PreparedRoom, generation: u64) {
        let spectators = self.api.spectator_count(room).await;
        let connected = blocking(native_livekit::is_livekit_room_connected)
            .await
            .unwrap_or(false);
        let check_session = self.update(|p| {
            if p.room_generation != generation || p.view.closing_room {
                return false;
            }
            if let (Some(count), Some(view)) = (spectators, p.view.room.as_mut()) {
                view.spectators = count;
                p.view.telemetry.spectator_count = count;
            }
            if connected {
                p.saw_connected = true;
                p.lost_since = None;
                if std::mem::take(&mut p.announced_loss) {
                    self.notice(
                        NoticeLevel::Success,
                        NoticeScope::Main,
                        "Reconnected",
                        "Spectators are receiving your stream again.",
                    );
                }
                if let Some(view) = p.view.room.as_mut() {
                    view.connection = RoomConnection::Connected;
                }
                return false;
            }
            p.saw_connected
        });
        if !check_session {
            return;
        }
        let has_session = blocking(native_livekit::has_livekit_room_session)
            .await
            .unwrap_or(false);
        let lost = self.update(|p| {
            if p.room_generation != generation || p.view.closing_room {
                return false;
            }
            if has_session {
                // The native session is rejoining on its own; surface it but keep the room.
                if let Some(view) = p.view.room.as_mut() {
                    view.connection = RoomConnection::Reconnecting;
                }
                let since = *p.lost_since.get_or_insert_with(Instant::now);
                if !p.announced_loss && since.elapsed() >= RECONNECT_NOTICE_DELAY {
                    p.announced_loss = true;
                    self.notice(
                        NoticeLevel::Info,
                        NoticeScope::Main,
                        "Connection lost",
                        "Reconnecting to the room. Your stream resumes automatically.",
                    );
                }
                return false;
            }
            p.clear_room();
            true
        });
        if lost {
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Room disconnected",
                "The connection to the room was lost. Create a new room to continue.",
            );
            self.handle_room_disconnect().await;
        }
    }

    /// Keeps the room alive on the server while the stream reconnects, and the native
    /// session's rejoin token fresh for streams that outlive the original token.
    async fn heartbeat(self: Arc<Self>, generation: u64) {
        loop {
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            if self.read(|p| p.room_generation != generation) {
                return;
            }
            let Some(room) = self.current_room(generation) else {
                continue;
            };
            let result = self.api.heartbeat(&room, HEARTBEAT_TIMEOUT).await;
            if self.current_room(generation).is_none() {
                continue;
            }
            match result {
                HeartbeatResult::Ok { token } => {
                    match blocking(move || native_livekit::update_livekit_token(token)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) | Err(error) => {
                            log::warn!("Failed to refresh the presenter token: {error}");
                        }
                    }
                }
                HeartbeatResult::Retry { reason } => {
                    log::warn!("Presenter heartbeat failed, retrying: {reason}");
                }
                HeartbeatResult::Unsupported => {}
                HeartbeatResult::Closed => {
                    self.notice(
                        NoticeLevel::Error,
                        NoticeScope::Main,
                        "Room closed",
                        "The server closed this room. Create a new room to continue.",
                    );
                    let _ = blocking(native_livekit::disconnect_livekit_room).await;
                    self.update(Private::clear_room);
                    self.handle_room_disconnect().await;
                    return;
                }
            }
        }
    }

    async fn handle_room_disconnect(self: &Arc<Self>) {
        self.capture_session.fetch_add(1, Ordering::AcqRel);
        self.update(|p| {
            p.view.capture = CaptureStage::Idle;
            p.live_config_key = None;
            p.stop_telemetry();
            p.endpoint_revision += 1;
        });
        if let Ok(Err(error)) = blocking(media::stop_native_capture).await {
            log::warn!("Capture cleanup after room loss: {error}");
        }
        if let Err(error) = self.stop_audio().await {
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Audio capture cleanup failed",
                &error,
            );
        }
    }

    async fn close_room(self: &Arc<Self>) -> Result<(), String> {
        let room = self.update(|p| {
            let room = p.room.clone();
            if room.is_some() {
                p.view.closing_room = true;
            }
            room
        });
        let Some(room) = room else {
            return Ok(());
        };
        if let Err(error) = self.api.delete_room(&room).await {
            self.update(|p| p.view.closing_room = false);
            return Err(error);
        }
        if let Ok(Err(error)) = blocking(native_livekit::disconnect_livekit_room).await {
            log::warn!("Room disconnect: {error}");
        }
        self.update(|p| {
            p.clear_room();
            p.view.closing_room = false;
        });
        Ok(())
    }

    /// Closes the room after confirmation. Returns whether it closed.
    async fn handle_close_room(self: &Arc<Self>) -> bool {
        let start = self.update(|p| {
            if p.view.room_transitioning || p.room_action {
                return false;
            }
            p.room_action = true;
            p.view.room_transitioning = true;
            true
        });
        if !start {
            return false;
        }
        let closed = match self.close_room().await {
            Ok(()) => {
                self.stop_capture_after_room_closure().await;
                self.update(|p| {
                    if p.view.settings.pending_api_endpoint.is_some() {
                        p.endpoint_revision += 1;
                    }
                });
                true
            }
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    NoticeScope::Main,
                    "Room closure failed",
                    &error,
                );
                false
            }
        };
        self.update(|p| {
            p.room_action = false;
            p.view.room_transitioning = false;
        });
        closed
    }

    async fn stop_capture_after_room_closure(self: &Arc<Self>) {
        self.update(|p| self.reset_capture(p));
        if let Ok(Err(error)) = blocking(media::stop_native_capture).await {
            log::warn!("Capture cleanup after room closure: {error}");
        }
        if let Err(error) = self.stop_audio().await {
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Audio capture cleanup failed",
                &error,
            );
        }
    }

    async fn replace_room(self: &Arc<Self>) {
        let start = self.update(|p| {
            if p.view.room_transitioning {
                return None;
            }
            let candidate = p.view.endpoint_switch_candidate.clone()?;
            p.view.room_transitioning = true;
            p.room_action = true;
            Some((candidate, p.view.capture == CaptureStage::Live))
        });
        let Some((candidate, resume_sharing)) = start else {
            return;
        };
        let finish = |p: &mut Private, pending: Option<&String>| {
            if let Some(pending) = pending {
                p.view.settings.pending_api_endpoint = Some(pending.clone());
            }
            p.view.endpoint_switch_candidate = None;
            p.view.room_transitioning = false;
            p.room_action = false;
        };
        let replacement = match self.api.create_room(&candidate).await {
            Ok(room) => room,
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    NoticeScope::Main,
                    "Room creation failed",
                    &error,
                );
                self.update(|p| finish(p, Some(&candidate)));
                self.persist_endpoint_fields().await;
                return;
            }
        };
        if let Err(error) = self.close_room().await {
            self.discard_room(&replacement).await;
            self.update(|p| finish(p, Some(&candidate)));
            self.persist_endpoint_fields().await;
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Room replacement stopped",
                &error,
            );
            return;
        }
        self.stop_capture_after_room_closure().await;
        if self.connect_prepared(&replacement).await.is_err() {
            let expires_at = Instant::now() + ROOM_RETRY_WINDOW;
            let code = replacement.code.clone();
            self.update(|p| {
                p.replacement = Some(Replacement {
                    room: replacement,
                    expires_at,
                    resume_sharing,
                });
                p.view.retryable_replacement = true;
                finish(p, Some(&candidate));
            });
            self.persist_endpoint_fields().await;
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Replacement room did not connect",
                "One retry is available in the room controls.",
            );
            let expiry = Arc::clone(self);
            self.spawn(async move { expiry.expire_replacement(code, expires_at).await });
            return;
        }
        self.update(|p| {
            Self::activate_endpoint(p, &candidate);
            finish(p, None);
        });
        self.persist_endpoint_fields().await;
        self.resume_sharing(resume_sharing).await;
    }

    async fn expire_replacement(self: Arc<Self>, code: String, expires_at: Instant) {
        tokio::time::sleep_until(expires_at.into()).await;
        let expired = self.update(|p| {
            if p.replacement.as_ref().is_some_and(|r| r.room.code == code) {
                p.view.retryable_replacement = false;
                p.endpoint_revision += 1;
                p.replacement.take()
            } else {
                None
            }
        });
        if let Some(replacement) = expired {
            self.discard_room(&replacement.room).await;
            self.notice(
                NoticeLevel::Info,
                NoticeScope::Main,
                "Replacement room expired",
                "Create a fresh room when you are ready to try again.",
            );
        }
    }

    async fn retry_room_connection(self: &Arc<Self>) {
        let replacement = self.update(|p| {
            if p.view.room_transitioning {
                return None;
            }
            let replacement = p.replacement.take()?;
            p.view.retryable_replacement = false;
            if Instant::now() < replacement.expires_at {
                p.view.room_transitioning = true;
            } else {
                p.endpoint_revision += 1;
            }
            Some(replacement)
        });
        let Some(replacement) = replacement else {
            return;
        };
        if Instant::now() >= replacement.expires_at {
            self.discard_room(&replacement.room).await;
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Replacement room expired",
                "Create a fresh room to try again.",
            );
            return;
        }
        if let Err(error) = self.connect_prepared(&replacement.room).await {
            self.discard_room(&replacement.room).await;
            self.update(|p| {
                p.endpoint_revision += 1;
                p.view.room_transitioning = false;
            });
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Room connection failed",
                &error,
            );
            return;
        }
        self.update(|p| {
            Self::activate_endpoint(p, &replacement.room.api_endpoint);
            p.view.room_transitioning = false;
        });
        self.persist_endpoint_fields().await;
        self.resume_sharing(replacement.resume_sharing).await;
    }

    async fn resume_sharing(self: &Arc<Self>, should_resume: bool) {
        if !should_resume {
            return;
        }
        let session = self.next_session();
        let (config, source) = self.read(|p| (p.capture_config(), p.selected_source));
        let result = blocking(move || media::start_combined(config, source))
            .await
            .and_then(|r| r);
        if let Err(error) = result {
            log::error!("Failed to resume sharing after room replacement: {error}");
            self.cleanup_failed_share().await;
            self.update(|p| p.view.capture = CaptureStage::Idle);
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Sharing did not resume",
                &format!("{error} The new room remains open."),
            );
            return;
        }
        if self.session() != session {
            self.cleanup_failed_share().await;
            return;
        }
        self.activate_live(session).await;
    }

    // ----- Capture -------------------------------------------------------------------------

    fn reset_capture(&self, p: &mut Private) {
        self.capture_session.fetch_add(1, Ordering::AcqRel);
        p.live_config_key = None;
        p.view.capture = CaptureStage::Idle;
        p.stop_telemetry();
        p.active_audio = None;
        if !p.view.audio.explicit {
            p.view.audio.selected = if p.auto_select() {
                None
            } else {
                Some(NO_AUDIO)
            };
        }
        p.view.audio.explicit = false;
        p.view.audio.auto_detected = None;
        p.view.audio.auto_detect_failed = false;
    }

    async fn stop_share(self: &Arc<Self>) {
        let start = self.update(|p| {
            if p.stop_busy {
                return false;
            }
            p.stop_busy = true;
            p.view.picker_open = false;
            true
        });
        if !start {
            return;
        }
        self.capture_session.fetch_add(1, Ordering::AcqRel);
        let stopped = blocking(media::stop_native_capture).await.and_then(|r| r);
        if let Err(error) = self.stop_audio().await {
            log::warn!("Audio stop after sharing: {error}");
        }
        self.update(|p| {
            match &stopped {
                Ok(()) => self.reset_capture(p),
                Err(error) => {
                    log::error!("Screenshare stop failed: {error}");
                    self.notice(
                        NoticeLevel::Error,
                        NoticeScope::Main,
                        "Screenshare stop failed",
                        "Capture could not be stopped cleanly. The room remains open.",
                    );
                }
            }
            p.stop_busy = false;
        });
    }

    async fn run_capture_action(self: &Arc<Self>, op: CaptureOp) {
        let start = self.update(|p| {
            if p.view.capture_busy || p.stop_busy || p.room_action || p.view.room_transitioning {
                return false;
            }
            p.view.capture_busy = true;
            true
        });
        if !start {
            return;
        }
        let result = match op {
            CaptureOp::Preview => self.start_preview_capture(None).await.map(|_| ()),
            CaptureOp::Replace(source) => self.replace_capture_source(source).await,
            CaptureOp::GoLive => self.publish_capture(self.session()).await,
        };
        if let Err(error) = result {
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Screenshare action failed",
                &error,
            );
        }
        self.update(|p| p.view.capture_busy = false);
    }

    async fn start_preview_capture(
        self: &Arc<Self>,
        source: Option<SourceSelection>,
    ) -> Result<u64, String> {
        let session = self.next_session();
        let settings = self.read(|p| p.view.settings.clone());
        let started = blocking(move || media::start_preview(source, &settings))
            .await
            .and_then(|r| r);
        if self.session() != session {
            if started.is_ok()
                && blocking(media::stop_video_capture)
                    .await
                    .and_then(|r| r)
                    .is_err()
            {
                self.notice(
                    NoticeLevel::Error,
                    NoticeScope::Main,
                    "Capture cleanup failed",
                    "A cancelled source did not stop cleanly.",
                );
            }
            return Ok(session);
        }
        if let Err(error) = started {
            log::info!("Capture preview did not start: {error}");
            return Err("No capture source was selected. The room remains open.".into());
        }
        let reveal = self.update(|p| {
            p.selected_source = source;
            p.view.capture = CaptureStage::Previewing;
            std::mem::take(&mut p.tray_share)
        });
        if reveal {
            self.emit(Event::ShowMain);
        }
        Ok(session)
    }

    async fn publish_capture(self: &Arc<Self>, session: u64) -> Result<(), String> {
        if self.session() != session {
            return Ok(());
        }
        let (config, source) = self.read(|p| (p.capture_config(), p.selected_source));
        if let Err(error) = blocking(move || media::go_live(config, source))
            .await
            .and_then(|r| r)
        {
            log::error!("Go live failed: {error}");
            return Err("The selected source could not be published. Try Go Live again.".into());
        }
        if self.session() != session {
            self.cleanup_failed_share().await;
            return Ok(());
        }
        self.activate_live(session).await;
        Ok(())
    }

    async fn activate_live(self: &Arc<Self>, session: u64) {
        if self.session() != session {
            return;
        }
        let target = self.resolve_audio_target().await;
        if self.session() != session {
            return;
        }
        self.capture_audio_for_target(target).await;
        if self.session() != session {
            return;
        }
        self.update(|p| {
            p.active_codec = p.view.settings.video_codec.clone();
            p.live_config_key = Some(p.config_key());
            p.view.capture = CaptureStage::Live;
            let tracker = TelemetryTracker::start(p.tick_inputs());
            p.view.telemetry = tracker.current().clone();
            p.telemetry = Some(tracker);
        });
        self.notice(
            NoticeLevel::Success,
            NoticeScope::Main,
            "Stream started",
            "Your screen is now live.",
        );
        let telemetry = Arc::clone(self);
        self.spawn(async move { telemetry.poll_telemetry(session).await });
        if cfg!(debug_assertions) || crate::platform::synthetic_capture() {
            self.spawn(async {
                let _ = blocking(media::log_audio_sources).await;
            });
        }
    }

    async fn cleanup_failed_share(self: &Arc<Self>) {
        if blocking(media::stop_native_capture)
            .await
            .and_then(|r| r)
            .is_err()
        {
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Main,
                "Capture cleanup failed",
                "Try Stop Screenshare before sharing again.",
            );
        }
        if let Err(error) = self.stop_audio().await {
            log::warn!("Audio cleanup after a failed share: {error}");
        }
        self.update(|p| {
            if !p.view.audio.explicit {
                p.view.audio.selected = if p.auto_select() {
                    None
                } else {
                    Some(NO_AUDIO)
                };
                p.view.audio.auto_detected = None;
            }
        });
    }

    async fn replace_capture_source(
        self: &Arc<Self>,
        source: Option<SourceSelection>,
    ) -> Result<(), String> {
        let stage = self.read(|p| p.view.capture);
        let previous = self.next_session();
        if stage != CaptureStage::Idle {
            blocking(media::stop_video_capture)
                .await
                .and_then(|r| r)
                .map_err(|_| "The current video source could not stop.".to_string())?;
            if self.session() != previous {
                return Ok(());
            }
            self.update(|p| {
                p.stop_telemetry();
                p.live_config_key = None;
                p.view.capture = CaptureStage::Idle;
            });
        }
        let session = self.start_preview_capture(source).await?;
        if stage == CaptureStage::Live {
            self.publish_capture(session).await?;
        }
        Ok(())
    }

    async fn open_source_picker(self: &Arc<Self>) {
        match blocking(media::capture_sources).await.and_then(|r| r) {
            Ok(sources) => {
                self.update(|p| p.view.picker_open = true);
                self.emit(Event::OpenSourcePicker(sources));
            }
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    NoticeScope::Main,
                    "Capture sources unavailable",
                    &error,
                );
            }
        }
    }

    async fn start_share(self: &Arc<Self>) {
        let ready = self.read(|p| {
            p.room.is_some() && p.view.capture == CaptureStage::Idle && !p.view.picker_open
        });
        if !ready {
            return;
        }
        if cfg!(target_os = "windows") {
            self.open_source_picker().await;
            return;
        }
        self.run_capture_action(CaptureOp::Preview).await;
    }

    async fn start_share_from_tray(self: &Arc<Self>) {
        if self.read(|p| p.view.capture_busy || p.room_action) {
            return;
        }
        let has_room = self.read(|p| p.room.is_some());
        if !has_room {
            if !self.read(|p| p.view.endpoint.healthy) {
                self.notice(
                    NoticeLevel::Error,
                    NoticeScope::Main,
                    "API endpoint unavailable",
                    "Fix the API endpoint in Settings → Stream before sharing.",
                );
                return;
            }
            if !self.create_room().await {
                return;
            }
        }
        if cfg!(target_os = "windows") {
            self.open_source_picker().await;
            self.emit(Event::ShowMain);
            return;
        }
        self.update(|p| p.tray_share = true);
        self.run_capture_action(CaptureOp::Preview).await;
        if self.update(|p| std::mem::take(&mut p.tray_share)) {
            self.emit(Event::ShowMain);
        }
    }

    async fn switch_source(self: &Arc<Self>) {
        let ready = self.read(|p| {
            p.room.is_some()
                && p.view.capture != CaptureStage::Idle
                && !p.view.picker_open
                && !p.view.capture_busy
                && !p.room_action
        });
        if !ready {
            return;
        }
        if cfg!(target_os = "windows") {
            self.open_source_picker().await;
            return;
        }
        self.run_capture_action(CaptureOp::Replace(None)).await;
    }

    async fn go_live(self: &Arc<Self>) {
        let ready = self.read(|p| p.view.capture == CaptureStage::Previewing)
            && media::hub().frames_received() > 0;
        if ready {
            self.run_capture_action(CaptureOp::GoLive).await;
        }
    }

    async fn poll_telemetry(self: Arc<Self>, session: u64) {
        let mut interval = tokio::time::interval(TELEMETRY_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            if self.session() != session || self.read(|p| p.view.capture != CaptureStage::Live) {
                return;
            }
            let Ok((sample, stats)) = blocking(|| {
                (
                    native_livekit::get_native_telemetry(),
                    native_livekit::get_desktop_capture_stats(),
                )
            })
            .await
            else {
                continue;
            };
            self.update(|p| {
                if self.session() != session {
                    return;
                }
                let inputs = p.tick_inputs();
                if let Some(tracker) = p.telemetry.as_mut() {
                    p.view.telemetry = tracker
                        .tick(&sample, &stats, inputs, Instant::now())
                        .clone();
                }
            });
        }
    }

    // ----- Audio ---------------------------------------------------------------------------

    async fn poll_audio_apps(self: Arc<Self>) {
        let mut interval = tokio::time::interval(AUDIO_APPS_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if self.read(|p| p.main_visible) {
                self.load_audio_apps().await;
            }
        }
    }

    async fn load_audio_apps(&self) {
        let apps = match blocking(media::list_audio_apps).await.and_then(|r| r) {
            Ok(apps) => apps,
            Err(error) => {
                log::debug!("Audio applications unavailable: {error}");
                return;
            }
        };
        let changed = self.read(|p| p.view.audio.apps != apps);
        if changed {
            self.update(|p| {
                p.view.audio.groups = audio::group_audio_apps(&apps);
                p.view.audio.apps = apps;
            });
        }
    }

    fn pid_targets(&self) -> bool {
        self.read(|p| p.view.platform.platform == "linux")
    }

    async fn apply_audio_target(&self, target: i32) -> Result<bool, String> {
        let revision = self.audio_revision.load(Ordering::Acquire);
        let _guard = self.audio_lock.lock().await;
        if revision != self.audio_revision.load(Ordering::Acquire) {
            return Ok(false);
        }
        let previous = self.read(|p| p.active_audio);
        let applied = blocking(move || media::change_audio_target(previous, target)).await??;
        if !applied {
            return Err("Native audio target change failed".into());
        }
        if revision != self.audio_revision.load(Ordering::Acquire) {
            if !blocking(native_rust::stop_audio_capture).await?? {
                return Err("Cancelled audio capture could not stop".into());
            }
            self.update(|p| p.active_audio = Some(NO_AUDIO));
            return Ok(false);
        }
        self.update(|p| p.active_audio = Some(target));
        Ok(true)
    }

    async fn stop_audio(&self) -> Result<bool, String> {
        self.audio_revision.fetch_add(1, Ordering::AcqRel);
        self.apply_audio_target(NO_AUDIO).await
    }

    async fn attempt_auto_resolve(&self) -> Option<AudioApp> {
        if !self.read(Private::auto_select) {
            return None;
        }
        let revision = self.audio_revision.load(Ordering::Acquire);
        let is_current = || {
            self.read(Private::auto_select)
                && revision == self.audio_revision.load(Ordering::Acquire)
        };
        let (app, context) = blocking(|| media::resolve_audio_source(None))
            .await
            .unwrap_or((None, None));
        if !is_current() {
            return None;
        }
        let pid_targets = self.pid_targets();
        match app.filter(|app| app.id != SYSTEM_AUDIO) {
            Some(app) => {
                self.update(|p| {
                    p.view.audio.selected = Some(audio::to_target_id(&app, pid_targets));
                    p.view.audio.auto_detected = Some(app.clone());
                });
                Some(app)
            }
            None => {
                let context = match context {
                    Some(context) => Some(context),
                    None => blocking(media::capture_context).await.ok().flatten(),
                };
                if is_current() {
                    self.update(|p| {
                        p.view.audio.capture_context = context;
                        p.view.audio.auto_detect_failed = true;
                    });
                }
                None
            }
        }
    }

    async fn resolve_audio_target(&self) -> i32 {
        let explicit_choice = |p: &Private| {
            if p.view.audio.explicit {
                Some(p.view.audio.selected.unwrap_or(NO_AUDIO))
            } else if !p.auto_select() {
                Some(NO_AUDIO)
            } else {
                None
            }
        };
        if let Some(target) = self.read(explicit_choice) {
            return target;
        }
        self.load_audio_apps().await;
        let resolved = self.attempt_auto_resolve().await;
        if let Some(target) = self.read(explicit_choice) {
            return target;
        }
        if let Some(app) = resolved {
            return audio::to_target_id(&app, self.pid_targets());
        }
        self.update(|p| p.view.audio.auto_detect_failed = true);
        self.notice(
            NoticeLevel::Info,
            NoticeScope::Main,
            "No audio detected",
            "Sharing video only. Choose an audio app to include its sound.",
        );
        NO_AUDIO
    }

    async fn capture_audio_for_target(&self, requested: i32) {
        let (target, active) = self.read(|p| {
            let target = if p.view.audio.explicit {
                p.view.audio.selected.unwrap_or(NO_AUDIO)
            } else if !p.auto_select() {
                NO_AUDIO
            } else {
                requested
            };
            (target, p.active_audio)
        });
        if active != Some(target)
            && let Err(error) = self.apply_audio_target(target).await
        {
            log::error!("Audio capture failed (continuing video-only): {error}");
            self.notice(
                NoticeLevel::Info,
                NoticeScope::Main,
                "Audio unavailable",
                "Sharing video only — the selected audio source could not be captured.",
            );
        }
    }

    async fn select_audio(&self, app_id: i32) {
        self.audio_revision.fetch_add(1, Ordering::AcqRel);
        let pid_targets = self.pid_targets();
        let (selected, live, previous) = self.update(|p| {
            p.view.audio.explicit = true;
            let target = p
                .view
                .audio
                .apps
                .iter()
                .find(|app| app.id == app_id)
                .map_or(app_id, |app| audio::to_target_id(app, pid_targets));
            p.view.audio.selected = Some(target);
            p.view.audio.auto_detected = None;
            (target, p.view.capture == CaptureStage::Live, p.active_audio)
        });
        if !live || previous == Some(selected) {
            return;
        }
        if let Err(error) = self.apply_audio_target(selected).await {
            log::error!("Audio switch failed: {error}");
            self.update(|p| p.view.audio.selected = p.active_audio);
            let (title, message) = if previous.is_none() {
                (
                    "Audio start failed",
                    "Could not start capturing the selected audio source.",
                )
            } else {
                (
                    "Audio switch failed",
                    "Could not switch to the selected audio source.",
                )
            };
            self.notice(NoticeLevel::Error, NoticeScope::Main, title, message);
        }
    }

    // ----- Settings and endpoints ----------------------------------------------------------

    fn activate_endpoint(p: &mut Private, endpoint: &str) {
        p.view.settings.api_endpoint = endpoint.into();
        p.view.settings.api_endpoint_is_custom = true;
        p.view.settings.pending_api_endpoint = None;
        p.view.endpoint_draft = None;
        p.view.endpoint.target = Some(endpoint.into());
        p.view.endpoint.availability = EndpointAvailability::Healthy;
        p.view.endpoint.healthy = true;
        p.view.endpoint.message = None;
    }

    async fn persist_endpoint_fields(&self) {
        let changes = self.read(|p| {
            let s = &p.view.settings;
            let mut changes = Map::new();
            if s.api_endpoint_is_custom {
                changes.insert("apiEndpoint".into(), Value::String(s.api_endpoint.clone()));
            }
            changes.insert(
                "apiEndpointIsCustom".into(),
                Value::Bool(s.api_endpoint_is_custom),
            );
            changes.insert(
                "pendingApiEndpoint".into(),
                s.pending_api_endpoint
                    .clone()
                    .map_or(Value::Null, Value::String),
            );
            changes
        });
        let storage = Arc::clone(&self.storage);
        if let Err(error) = blocking(move || storage.save_stream_settings(changes))
            .await
            .and_then(|r| r)
        {
            self.notice(
                NoticeLevel::Error,
                NoticeScope::Settings,
                "Settings save failed",
                &error,
            );
        }
    }

    fn check_endpoint(&self) {
        let Some(inner) = self.arc() else {
            return;
        };
        let generation = self.endpoint_check.fetch_add(1, Ordering::AcqRel) + 1;
        self.spawn(async move {
            let target = inner.update(|p| {
                let target = p
                    .view
                    .settings
                    .pending_api_endpoint
                    .clone()
                    .unwrap_or_else(|| p.view.settings.api_endpoint.clone());
                p.view.endpoint = EndpointHealth {
                    target: Some(target.clone()),
                    availability: EndpointAvailability::Checking,
                    healthy: false,
                    message: None,
                };
                target
            });
            let result = inner.api.check_endpoint(&target).await;
            if inner.endpoint_check.load(Ordering::Acquire) != generation {
                return;
            }
            let validated = inner.update(|p| match result {
                Ok(endpoint) => {
                    p.view.endpoint.availability = EndpointAvailability::Healthy;
                    p.view.endpoint.healthy = true;
                    Some(endpoint)
                }
                Err(failure) => {
                    p.view.endpoint.availability = EndpointAvailability::Error;
                    p.view.endpoint.message = Some(failure.message);
                    None
                }
            });
            if let Some(endpoint) = validated {
                inner.endpoint_validated(endpoint).await;
            }
        });
    }

    async fn validate_draft(self: &Arc<Self>, text: String) {
        let generation = self.draft_check.fetch_add(1, Ordering::AcqRel) + 1;
        self.update(|p| {
            p.view.endpoint_draft = Some(EndpointDraft {
                text: text.clone(),
                availability: EndpointAvailability::Checking,
                message: None,
            });
        });
        let result = self.api.check_endpoint(&text).await;
        if self.draft_check.load(Ordering::Acquire) != generation {
            return;
        }
        match result {
            Ok(endpoint) => {
                self.update(|p| {
                    p.view.endpoint_draft = Some(EndpointDraft {
                        text: endpoint.clone(),
                        availability: EndpointAvailability::Healthy,
                        message: None,
                    });
                });
                self.endpoint_validated(endpoint).await;
            }
            Err(failure) => self.update(|p| {
                if let Some(draft) = p.view.endpoint_draft.as_mut() {
                    draft.availability = EndpointAvailability::Error;
                    draft.message = Some(failure.message);
                }
            }),
        }
    }

    /// A healthy endpoint was confirmed, either the current target or a Settings edit.
    async fn endpoint_validated(&self, endpoint: String) {
        enum Outcome {
            Nothing,
            Persist,
            Confirm,
        }
        let outcome = self.update(|p| {
            let has_room = p.room.is_some();
            let has_replacement = p.replacement.is_some();
            if endpoint == p.view.settings.api_endpoint {
                p.view.endpoint.target = Some(endpoint.clone());
                p.view.endpoint.availability = EndpointAvailability::Healthy;
                p.view.endpoint.healthy = true;
                if has_room || p.view.settings.pending_api_endpoint.is_none() {
                    return Outcome::Nothing;
                }
                p.view.settings.pending_api_endpoint = None;
                return Outcome::Persist;
            }
            if p.view.settings.pending_api_endpoint.as_ref() == Some(&endpoint)
                && (has_room || has_replacement)
            {
                return Outcome::Nothing;
            }
            if !has_room && !has_replacement {
                Self::activate_endpoint(p, &endpoint);
                return Outcome::Persist;
            }
            p.view.endpoint_switch_candidate = Some(endpoint.clone());
            Outcome::Confirm
        });
        match outcome {
            Outcome::Nothing => {}
            Outcome::Persist => self.persist_endpoint_fields().await,
            Outcome::Confirm => self.emit(Event::ShowMain),
        }
    }

    async fn update_stream_settings(self: &Arc<Self>, changes: Map<String, Value>) {
        if changes.is_empty() {
            return;
        }
        let storage = Arc::clone(&self.storage);
        let saved = match blocking(move || storage.save_stream_settings(changes))
            .await
            .and_then(|r| r)
        {
            Ok(saved) => saved,
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    NoticeScope::Settings,
                    "Settings save failed",
                    &format!("Changes could not be saved: {error}"),
                );
                return;
            }
        };
        self.update(|p| {
            let endpoint = (
                p.view.settings.api_endpoint.clone(),
                p.view.settings.api_endpoint_is_custom,
                p.view.settings.pending_api_endpoint.clone(),
            );
            p.hydrate_settings(saved);
            // Endpoint fields change only through validation, never through a plain save.
            p.view.settings.api_endpoint = endpoint.0;
            p.view.settings.api_endpoint_is_custom = endpoint.1;
            p.view.settings.pending_api_endpoint = endpoint.2;
        });
        self.notice(
            NoticeLevel::Success,
            NoticeScope::Settings,
            "Stream settings saved",
            "",
        );
        self.schedule_live_update();
    }

    fn schedule_live_update(self: &Arc<Self>) {
        let needed = self.read(|p| {
            p.view.capture == CaptureStage::Live
                && p.live_config_key.as_ref() != Some(&p.config_key())
        });
        if !needed {
            return;
        }
        let generation = self.live_update.fetch_add(1, Ordering::AcqRel) + 1;
        let inner = Arc::clone(self);
        self.spawn(async move {
            tokio::time::sleep(LIVE_UPDATE_DEBOUNCE).await;
            if inner.live_update.load(Ordering::Acquire) != generation {
                return;
            }
            let session = inner.session();
            let (config, key, previous_codec) =
                inner.read(|p| (p.capture_config(), p.config_key(), p.active_codec.clone()));
            let codec = key.codec.clone();
            let summary = format!(
                "codec={codec} fps={} bitrate={:.0}Mbps",
                config.fps,
                config.max_bitrate.unwrap_or(0.0) / 1e6
            );
            let applied = blocking(move || media::update_video(config))
                .await
                .unwrap_or(false);
            if !applied || inner.session() != session {
                return;
            }
            inner.update(|p| {
                p.live_config_key = Some(key);
                p.active_codec.clone_from(&codec);
                if let Some(tracker) = p.telemetry.as_mut() {
                    tracker.reset_baselines();
                }
            });
            log::info!("[Presenter] Live encoder update: {summary}");
            if previous_codec != codec {
                inner.notice(
                    NoticeLevel::Info,
                    NoticeScope::Main,
                    "Video codec updated",
                    &format!("Switched video codec to {}", codecs::codec_label(&codec)),
                );
            }
        });
    }
}
