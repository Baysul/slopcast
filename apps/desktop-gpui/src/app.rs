//! The main window: room lifecycle, capture controls, preview and telemetry.
//!
//! The window is a rendering of the core's [`Snapshot`]. Every control dispatches a
//! [`Command`]; nothing here owns session state, so hiding the window cannot stop a stream.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use desktop_core::audio::{NO_AUDIO, SYSTEM_AUDIO};
use desktop_core::media::{self, CaptureSource, SourceKind};
use desktop_core::presenter::{
    CaptureStage, Command, Confirmation, DesktopAction, Event, Notice, NoticeLevel, Presenter,
    RoomConnection, Snapshot,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, Theme, ThemeMode};
use gpui_kit::*;

// Slopcast's desktop palette: `--background`, `--card` and `--safelight` from its theme files.
const BACKGROUND: Hsla = hsla(0.6154, 0.4194, 0.0608, 1.0);
const SURFACE: Hsla = hsla(0.6136, 0.3929, 0.1098, 1.0);
const VIEWPORT: Hsla = hsla(0.615, 0.420, 0.040, 1.0);
const BORDER: Hsla = hsla(0.613, 0.280, 0.205, 1.0);
const SAFELIGHT: Hsla = hsla(0.0738, 0.5083, 0.5294, 1.0);
const MUTED: Hsla = hsla(0.6111, 0.1200, 0.6200, 1.0);
const SUCCESS: Hsla = hsla(0.3600, 0.5500, 0.5600, 1.0);
const DANGER: Hsla = hsla(0.0000, 0.6000, 0.5800, 1.0);

const SIDEBAR_WIDTH: Pixels = px(360.);

/// Opens the main window and installs the application's quit path.
pub fn open(cx: &mut App, presenter: Presenter) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.background = BACKGROUND;
        theme.primary = SAFELIGHT;
        theme.primary_foreground = BACKGROUND;
        theme.ring = SAFELIGHT;
    });

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(120.), px(120.)),
            size(px(1180.), px(760.)),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Slopcast".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let entity = match open_window(options, cx, |_, cx| {
        cx.new(|cx| PresenterApp::new(presenter, cx))
    }) {
        Ok((handle, entity)) => {
            entity.update(cx, |app, _| app.window = Some(handle));
            entity
        }
        Err(error) => {
            log::error!("Cannot open Slopcast: {error}");
            cx.quit();
            return;
        }
    };

    // Shutdown runs off the UI thread: room deletion and native teardown are bounded but slow.
    let quitting = entity.clone();
    let subscription = cx.on_app_quit(move |cx| {
        let presenter = quitting.update(cx, |app, _| app.presenter.take());
        let background = cx.background_executor().clone();
        async move {
            if let Some(presenter) = presenter {
                background.spawn(async move { presenter.shutdown() }).await;
            }
        }
    });
    entity.update(cx, |app, _| app.quit_subscription = Some(subscription));
}

struct PresenterApp {
    window: Option<AnyWindowHandle>,
    quit_subscription: Option<Subscription>,
    presenter: Option<Presenter>,
    view: Snapshot,
    preview: Option<Arc<RenderImage>>,
    /// The preview area in physical pixels, so native scaling matches GPUI's rendering.
    viewport: Rc<Cell<Option<(u32, u32)>>>,
    notices: Vec<Notice>,
    sources: Option<Vec<CaptureSource>>,
    confirmation: Option<Confirmation>,
    frames_received: u64,
    /// Snapshot, event and frame subscriptions, kept alive for the window's lifetime.
    tasks: Vec<Task<()>>,
}

impl PresenterApp {
    fn new(presenter: Presenter, cx: &mut Context<Self>) -> Self {
        let mut app = Self {
            window: None,
            quit_subscription: None,
            view: presenter.snapshot(),
            presenter: Some(presenter),
            preview: None,
            viewport: Rc::new(Cell::new(None)),
            notices: Vec::new(),
            sources: None,
            confirmation: None,
            frames_received: 0,
            tasks: Vec::new(),
        };
        app.subscribe(cx);
        app
    }

    fn subscribe(&mut self, cx: &mut Context<Self>) {
        let Some(presenter) = self.presenter.as_ref() else {
            return;
        };
        let mut snapshots = presenter.subscribe();
        let events = presenter.take_events();
        let hub = media::hub();

        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                let snapshot = snapshots.borrow_and_update().clone();
                if this
                    .update(cx, |app, cx| {
                        app.apply(snapshot);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                if snapshots.changed().await.is_err() {
                    break;
                }
            }
        }));

        if let Some(mut events) = events {
            self.tasks.push(cx.spawn(async move |this, cx| {
                while let Some(event) = events.recv().await {
                    if this
                        .update(cx, |app, cx| app.handle_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }

        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                if let Some(frame) = hub.take_frame() {
                    let image = preview_image(frame);
                    if this
                        .update(cx, |app, cx| {
                            app.preview = image;
                            app.frames_received += 1;
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                // Woken by the native preview callback; no polling while nothing is captured.
                hub.frame_ready().await;
            }
        }));
    }

    /// Applies a new snapshot, releasing preview resources the core no longer produces.
    fn apply(&mut self, snapshot: Snapshot) {
        if snapshot.capture == CaptureStage::Idle {
            self.preview = None;
            self.clear_viewport();
        }
        self.view = snapshot;
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Notice(notice) => {
                self.notices.push(notice);
                if self.notices.len() > 8 {
                    self.notices.remove(0);
                }
            }
            Event::ShowMain => {
                if let Some(window) = self.window {
                    let _ = window.update(cx, |_, window, _| window.activate_window());
                }
            }
            Event::Confirm(confirmation) => self.confirmation = Some(confirmation),
            Event::OpenSourcePicker(sources) => self.sources = Some(sources),
            Event::QuitNow => {
                self.confirmation = None;
                cx.quit();
                return;
            }
        }
        cx.notify();
    }

    fn send(&self, command: Command) {
        if let Some(presenter) = self.presenter.as_ref() {
            presenter.dispatch(command);
        }
    }

    fn clear_viewport(&mut self) {
        if self.viewport.replace(None).is_some() {
            media::clear_preview_viewport();
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let room = self.view.room.as_ref();
        let (status, color) = match room {
            None => ("No room".to_string(), MUTED),
            Some(room) => (connection_label(room.connection).to_string(), {
                match room.connection {
                    RoomConnection::Connected => SUCCESS,
                    RoomConnection::Connecting | RoomConnection::Reconnecting => SAFELIGHT,
                }
            }),
        };
        let endpoint = self.view.settings.api_endpoint.clone();

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(BORDER)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().font_semibold().child("Slopcast"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(8.)).rounded_full().bg(color))
                            .child(div().text_xs().text_color(MUTED).child(status)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_xs().text_color(MUTED).child(endpoint))
                    .child(Button::new("quit").small().ghost().label("Quit").on_click(
                        cx.listener(|app, _, _, _| {
                            app.send(Command::Action(DesktopAction::Quit));
                        }),
                    )),
            )
    }

    fn render_preview(&self) -> impl IntoElement {
        let placeholder = if self.view.capture == CaptureStage::Idle {
            "No preview yet. Start sharing to see your screen here."
        } else {
            "Waiting for the first preview frame…"
        };

        let content: AnyElement = match self.preview.clone() {
            Some(image) => img(image)
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element(),
            None => div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .text_sm()
                .text_color(MUTED)
                .child(placeholder)
                .into_any_element(),
        };

        let viewport = Rc::clone(&self.viewport);

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .bg(VIEWPORT)
            .on_children_prepainted(move |bounds, window, _| {
                let Some(bounds) = bounds.first().copied() else {
                    return;
                };
                let scale = window.scale_factor();
                let width = f32::from(bounds.size.width) * scale;
                let height = f32::from(bounds.size.height) * scale;
                if width < 1.0 || height < 1.0 {
                    return;
                }
                let size = (width.round() as u32, height.round() as u32);
                if viewport.get() != Some(size) {
                    viewport.set(Some(size));
                    media::set_preview_viewport(size.0, size.1);
                }
            })
            .child(content)
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .gap_3()
            .w(SIDEBAR_WIDTH)
            .h_full()
            .p_3()
            .bg(SURFACE)
            .border_l_1()
            .border_color(BORDER)
            .overflow_y_scroll()
            .child(self.render_room_card(cx))
            .child(self.render_capture_card(cx))
            .child(self.render_audio_card(cx))
            .child(self.render_telemetry_card())
    }

    fn render_room_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let card = card("Room");
        let Some(room) = self.view.room.as_ref() else {
            let mut elements: Vec<AnyElement> = Vec::new();
            if let Some(reason) = self.view.room_create_disabled_reason() {
                elements.push(text(reason).into_any_element());
            }
            return card
                .child(text(
                    "Create a room, share the link, then choose what to present.",
                ))
                .child(
                    Button::new("create-room")
                        .primary()
                        .label(if self.view.creating_room {
                            "Creating room…"
                        } else {
                            "Create room"
                        })
                        .loading(self.view.creating_room)
                        .disabled(!self.view.can_create_room())
                        .on_click(cx.listener(|app, _, _, _| app.send(Command::CreateRoom))),
                )
                .children(elements);
        };

        let url = room.share_url.clone();
        let copy_url = url.clone();
        card.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_lg().font_semibold().child(room.code.clone()))
                .child(status_chip(room.connection)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .p_2()
                .rounded_md()
                .bg(BACKGROUND)
                .child(div().flex_1().text_xs().truncate().child(url))
                .child(
                    Button::new("copy-link")
                        .small()
                        .secondary()
                        .label("Copy")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(text(match room.spectators {
                    1 => "1 spectator".to_string(),
                    count => format!("{count} spectators"),
                }))
                .child(
                    Button::new("close-room")
                        .small()
                        .danger()
                        .label("Close room")
                        .disabled(self.view.room_transitioning || self.view.closing_room)
                        .on_click(cx.listener(|app, _, _, _| {
                            app.send(Command::Action(DesktopAction::CloseRoom));
                        })),
                ),
        )
    }

    fn render_capture_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let stage = self.view.capture;
        let busy = self.view.capture_busy;
        let muted = self.view.muted;
        let mut controls: Vec<AnyElement> = Vec::new();

        match stage {
            CaptureStage::Idle => controls.push(
                Button::new("start-share")
                    .primary()
                    .label("Start sharing")
                    .disabled(!self.view.can_start_share())
                    .on_click(cx.listener(|app, _, _, _| app.send(Command::StartShare)))
                    .into_any_element(),
            ),
            CaptureStage::Previewing => controls.push(
                Button::new("go-live")
                    .primary()
                    .label("Go live")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, _| app.send(Command::GoLive)))
                    .into_any_element(),
            ),
            CaptureStage::Live => {}
        }

        if stage != CaptureStage::Idle {
            controls.push(
                Button::new("switch-source")
                    .secondary()
                    .label("Switch source")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, _| app.send(Command::SwitchSource)))
                    .into_any_element(),
            );
            controls.push(
                Button::new("stop-share")
                    .danger()
                    .label("Stop sharing")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, _| app.send(Command::StopShare)))
                    .into_any_element(),
            );
        }

        controls.push(
            Button::new("mute")
                .secondary()
                .toggled(muted)
                .label(if muted { "Unmute" } else { "Mute" })
                .on_click(cx.listener(|app, _, _, _| app.send(Command::ToggleMute)))
                .into_any_element(),
        );

        let subtitle: AnyElement = match stage {
            CaptureStage::Idle => text("Nothing is captured.").into_any_element(),
            CaptureStage::Previewing => {
                text("Previewing. Spectators see nothing until you go live.").into_any_element()
            }
            CaptureStage::Live => text("Live to everyone watching the link.").into_any_element(),
        };

        card("Capture")
            .child(subtitle)
            .child(div().flex().flex_wrap().gap_2().children(controls))
            .children(busy.then(|| text("Working…").into_any_element()))
    }

    fn render_audio_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let audio = &self.view.audio;
        let mut choices: Vec<(String, i32)> = vec![
            ("No audio".to_string(), NO_AUDIO),
            ("System audio".to_string(), SYSTEM_AUDIO),
        ];
        for group in &audio.groups {
            let name = &group.representative.name;
            let label = if group.members.len() > 1 {
                format!("{name} ({})", group.members.len())
            } else {
                name.clone()
            };
            choices.push((label, group.representative.id));
        }

        let buttons: Vec<AnyElement> = choices
            .into_iter()
            .map(|(label, id)| {
                Button::new(SharedString::from(format!("audio-{id}")))
                    .small()
                    .secondary()
                    .label(label)
                    .toggled(audio.selected == Some(id))
                    .on_click(cx.listener(move |app, _, _, _| {
                        app.send(Command::SelectAudio(id));
                    }))
                    .into_any_element()
            })
            .collect();

        let status: AnyElement = match (audio.explicit, audio.auto_detected.as_ref()) {
            (false, Some(app)) => text(format!("Following {}", app.name)).into_any_element(),
            (false, None) if audio.auto_detect_failed => {
                text("No matching app found. Choose audio below.").into_any_element()
            }
            _ => text("Choose which application audio the stream carries.").into_any_element(),
        };

        card("Audio")
            .child(status)
            .child(div().flex().flex_wrap().gap_1p5().children(buttons))
            .children(audio.capture_context.as_ref().map(|context| {
                text(match context.media_name.as_deref() {
                    Some(name) => format!("Capturing {} · {}", context.source_type, name),
                    None => format!(
                        "Capturing {}",
                        context
                            .window_caption
                            .as_deref()
                            .unwrap_or(&context.desktop)
                    ),
                })
                .into_any_element()
            }))
    }

    fn render_telemetry_card(&self) -> impl IntoElement {
        let telemetry = &self.view.telemetry;
        let spectators = self.view.room.as_ref().map_or(0, |room| room.spectators);

        card("Stream")
            .child(row(
                "Status",
                if telemetry.live { "Live" } else { "Idle" }.to_string(),
            ))
            .child(row(
                "Resolution",
                match (telemetry.width, telemetry.height) {
                    (Some(width), Some(height)) => format!("{width}×{height}"),
                    _ => "—".to_string(),
                },
            ))
            .child(row(
                "Frame rate",
                telemetry
                    .frame_rate
                    .map_or_else(|| "—".to_string(), |fps| format!("{fps:.0} fps")),
            ))
            .child(row(
                "Bitrate",
                telemetry
                    .video_bitrate
                    .map_or_else(|| "—".to_string(), megabits),
            ))
            .child(row(
                "Round trip",
                telemetry
                    .rtt_ms
                    .map_or_else(|| "—".to_string(), |rtt| format!("{rtt:.0} ms")),
            ))
            .child(row(
                "Packet loss",
                telemetry
                    .packet_loss_pct
                    .map_or_else(|| "—".to_string(), |loss| format!("{loss:.1} %")),
            ))
            .child(row("Spectators", spectators.to_string()))
            .child(self.render_bitrate_bars())
    }

    fn render_bitrate_bars(&self) -> AnyElement {
        let history = &self.view.telemetry.bitrate_history;
        if history.is_empty() {
            return div().into_any_element();
        }
        let peak = history.iter().copied().fold(0.0_f64, f64::max).max(1.0);
        let bars: Vec<AnyElement> = history
            .iter()
            .map(|value| {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a normalized bar height is the intended truncation"
                )]
                let ratio = (value / peak).clamp(0.0, 1.0) as f32;
                div()
                    .flex_1()
                    .h(px(2.0 + ratio * 22.0))
                    .rounded_sm()
                    .bg(SAFELIGHT)
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .items_end()
            .gap(px(2.))
            .h(px(24.))
            .w_full()
            .children(bars)
            .into_any_element()
    }

    fn render_notices(&self) -> impl IntoElement {
        let notices: Vec<AnyElement> = self
            .notices
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|notice| {
                let color = match notice.level {
                    NoticeLevel::Info => MUTED,
                    NoticeLevel::Success => SUCCESS,
                    NoticeLevel::Error => DANGER,
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(div().size(px(6.)).rounded_full().bg(color))
                    .child(div().text_color(color).child(notice.title.clone()))
                    .child(
                        div()
                            .text_color(MUTED)
                            .truncate()
                            .child(notice.message.clone()),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(BORDER)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(MUTED)
                    .child(format!("Preview frames: {}", self.frames_received))
                    .child(self.view.settings_directory.clone()),
            )
            .children(notices)
    }

    fn render_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(sources) = self.sources.as_ref() {
            let sources = sources.clone();
            let rows: Vec<AnyElement> = sources
                .iter()
                .map(|source| {
                    let source = source.clone();
                    Button::new(SharedString::from(format!("source-{}", source.id)))
                        .secondary()
                        .label(format!(
                            "{} · {}",
                            source_kind_label(source.kind),
                            source.title
                        ))
                        .on_click(cx.listener(move |app, _, _, _| {
                            app.sources = None;
                            app.send(Command::SourceSelected(
                                desktop_core::media::SourceSelection {
                                    kind: source.kind,
                                    id: source.id,
                                },
                            ));
                        }))
                        .into_any_element()
                })
                .collect();
            return dialog(
                "Choose what to share",
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(360.))
                    .id("picker-list")
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element(),
                vec![
                    Button::new("cancel-picker")
                        .secondary()
                        .label("Cancel")
                        .on_click(cx.listener(|app, _, _, _| {
                            app.sources = None;
                            app.send(Command::CancelSourcePicker);
                        }))
                        .into_any_element(),
                ],
            );
        }

        let Some(confirmation) = self.confirmation else {
            return div().into_any_element();
        };
        let (title, message, confirm, consequence) = match confirmation {
            Confirmation::CloseRoom => (
                "Close this room?",
                "Everyone watching loses the stream and the link stops working.",
                "Close room",
                Command::CloseRoom,
            ),
            Confirmation::Quit => (
                "Quit Slopcast?",
                "Sharing stops and the room is deleted.",
                "Quit",
                Command::Quit { anyway: false },
            ),
            Confirmation::QuitAnyway => (
                "Quit anyway?",
                "The room could not be deleted. It will remain until it expires.",
                "Quit anyway",
                Command::Quit { anyway: true },
            ),
        };

        dialog(
            title,
            text(message),
            vec![
                Button::new("cancel-confirm")
                    .secondary()
                    .label("Cancel")
                    .on_click(cx.listener(|app, _, _, _| app.confirmation = None))
                    .into_any_element(),
                Button::new("confirm")
                    .danger()
                    .label(confirm)
                    .on_click(cx.listener(move |app, _, _, _| {
                        app.confirmation = None;
                        app.send(consequence.clone());
                    }))
                    .into_any_element(),
            ],
        )
    }
}

impl Render for PresenterApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(BACKGROUND)
            .text_sm()
            .text_color(cx.theme().foreground)
            .child(self.render_header(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_preview())
                    .child(self.render_sidebar(cx)),
            )
            .child(self.render_notices())
            .child(self.render_overlay(cx))
    }
}

fn preview_image(frame: media::PreviewFrame) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    // GPUI's renderer consumes BGRA; the native preview payload is already in that order.
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

fn card(title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(BACKGROUND)
        .border_1()
        .border_color(BORDER)
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(MUTED)
                .child(title),
        )
}

fn row(label: &'static str, value: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .text_xs()
        .child(div().text_color(MUTED).child(label))
        .child(value)
        .into_any_element()
}

fn text(value: impl Into<SharedString>) -> AnyElement {
    div()
        .text_xs()
        .text_color(MUTED)
        .child(value.into())
        .into_any_element()
}

fn status_chip(connection: RoomConnection) -> AnyElement {
    let (label, color) = match connection {
        RoomConnection::Connected => ("Connected", SUCCESS),
        RoomConnection::Connecting => ("Connecting", SAFELIGHT),
        RoomConnection::Reconnecting => ("Reconnecting", SAFELIGHT),
    };
    div()
        .px_2()
        .py_1()
        .rounded_full()
        .bg(color.opacity(0.15))
        .text_color(color)
        .text_xs()
        .child(label)
        .into_any_element()
}

fn dialog(title: &'static str, body: AnyElement, actions: Vec<AnyElement>) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(0.6154, 0.4194, 0.0608, 0.72))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .w(px(460.))
                .p_4()
                .rounded_lg()
                .bg(SURFACE)
                .border_1()
                .border_color(BORDER)
                .child(div().font_semibold().child(title))
                .child(body)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .children(actions),
                ),
        )
        .into_any_element()
}

fn connection_label(connection: RoomConnection) -> &'static str {
    match connection {
        RoomConnection::Connecting => "Connecting",
        RoomConnection::Connected => "Connected",
        RoomConnection::Reconnecting => "Reconnecting",
    }
}

fn source_kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Screen => "Screen",
        SourceKind::Window => "Window",
    }
}

fn megabits(bits_per_second: f64) -> String {
    format!("{:.1} Mb/s", bits_per_second / 1_000_000.0)
}
