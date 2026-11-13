//! The main window: room lifecycle, capture controls, preview and telemetry.
//!
//! The window is a rendering of the core's [`Snapshot`]. Every control dispatches a
//! [`Command`]; nothing here owns session state, so hiding the window cannot stop a stream.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crate::ui::{self, button, icon, section_heading};
use desktop_core::audio::{NO_AUDIO, SYSTEM_AUDIO};
use desktop_core::media::{self, CaptureSource, SourceKind};
use desktop_core::presenter::{
    CaptureStage, Command, Confirmation, DesktopAction, Event, Notice, NoticeLevel, Presenter,
    RoomConnection, Snapshot,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::{ActiveTheme, Disableable, StyledExt};
use gpui_kit::*;

const SUCCESS: Hsla = hsla(0.3600, 0.5500, 0.5600, 1.0);
const SIDEBAR_WIDTH: Pixels = px(352.);

/// Starts the process-lifetime presenter and opens its main window.
pub fn open(
    cx: &mut App,
    presenter: Presenter,
    signals: async_channel::Sender<crate::services::Signal>,
    receiver: async_channel::Receiver<crate::services::Signal>,
    instance: crate::instance::Instance,
) {
    let entity = cx.new(|cx| PresenterApp::new(presenter, signals, receiver, instance, cx));
    show_main(&entity, cx);
    let activate = entity.downgrade();
    cx.on_system_notification_response(move |_, cx| {
        if let Some(entity) = activate.upgrade() {
            show_main(&entity, cx);
        }
    });
    let quitting = entity.clone();
    let subscription = cx.on_app_quit(move |cx| {
        let (presenter, instance) = quitting.update(cx, |app, cx| {
            app.replace_preview(None, cx);
            app.clear_viewport();
            app.services.take();
            (app.presenter.take(), app.instance.take())
        });
        let background = cx.background_executor().clone();
        async move {
            background
                .spawn(async move {
                    if let Some(presenter) = presenter {
                        presenter.shutdown();
                    }
                    drop(instance);
                })
                .await;
        }
    });
    entity.update(cx, |app, _| app.quit_subscription = Some(subscription));
}

pub(super) fn show_main(entity: &Entity<PresenterApp>, cx: &mut App) {
    let previous = entity.read(cx).window;
    if let Some(handle) = previous
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Maximized(Bounds::new(
            point(px(120.), px(120.)),
            size(px(1100.), px(900.)),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Slopcast".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        window_decorations: Some(WindowDecorations::Client),
        app_owns_titlebar_drag: true,
        app_id: Some("org.slopcast.Slopcast".into()),
        ..Default::default()
    };
    let content = entity.clone();
    match open_window(options, cx, move |window, cx| {
        #[cfg(feature = "e2e")]
        crate::e2e::record_gpu(window.gpu_specs());
        let owner = content.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            owner
                .update(cx, |app, cx| {
                    if app.tray_ready && app.view.preferences.tray_enabled {
                        app.window = None;
                        app.visible = false;
                        app.clear_viewport();
                        app.replace_preview(None, cx);
                        app.send(Command::SetMainVisible(false));
                        true
                    } else {
                        app.send(Command::Action(DesktopAction::Quit));
                        false
                    }
                })
                .unwrap_or(true)
        });
        let owner = content.downgrade();
        let visibility = window.observe_window_visibility(move |_, window, cx| {
            let _ = owner.update(cx, |app, cx| {
                app.visible = window.is_visible();
                app.send(Command::SetMainVisible(app.visible));
                if !app.visible {
                    app.clear_viewport();
                    app.replace_preview(None, cx);
                }
                cx.notify();
            });
        });
        content.update(cx, |app, cx| {
            app.visibility = Some(visibility);
            app.visible = true;
            app.focus.focus(window, cx);
            cx.notify();
        });
        content
    }) {
        Ok((handle, _)) => entity.update(cx, |app, cx| {
            app.window = Some(handle);
            app.send(Command::SetMainVisible(true));
            cx.notify();
        }),
        Err(error) => {
            log::error!("Cannot open Slopcast: {error}");
            cx.quit();
        }
    }
}

pub(crate) struct PresenterApp {
    window: Option<AnyWindowHandle>,
    quit_subscription: Option<Subscription>,
    pub(crate) presenter: Option<Presenter>,
    services: Option<crate::services::Services>,
    instance: Option<crate::instance::Instance>,
    visibility: Option<Subscription>,
    visible: bool,
    tray_ready: bool,
    settings: Option<AnyWindowHandle>,
    focus: FocusHandle,
    title_drag: ui::TitleDrag,
    telemetry_visible: bool,
    pub(crate) view: Snapshot,
    preview: Option<Arc<RenderImage>>,
    preview_area: Size<Pixels>,
    selected_source: Option<media::SourceSelection>,
    audio_waves: std::collections::BTreeMap<i32, Vec<f64>>,
    /// The preview area in physical pixels, so native scaling matches GPUI's rendering.
    viewport: Rc<Cell<Option<(u32, u32)>>>,
    notices: Vec<Notice>,
    sources: Option<Vec<CaptureSource>>,
    confirmation: Option<Confirmation>,
    frames_received: u64,
    copied: Option<&'static str>,
    stop_confirm: bool,
    /// Snapshot, event and frame subscriptions, kept alive for the window's lifetime.
    tasks: Vec<Task<()>>,
}

impl PresenterApp {
    fn new(
        presenter: Presenter,
        signals: async_channel::Sender<crate::services::Signal>,
        receiver: async_channel::Receiver<crate::services::Signal>,
        instance: crate::instance::Instance,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::settings::apply_appearance(&presenter.snapshot().preferences, cx);
        let services = crate::services::Services::new(&presenter, signals);
        let mut app = Self {
            window: None,
            quit_subscription: None,
            view: presenter.snapshot(),
            presenter: Some(presenter),
            services: Some(services),
            instance: Some(instance),
            visibility: None,
            visible: true,
            tray_ready: false,
            settings: None,
            focus: cx.focus_handle(),
            title_drag: Default::default(),
            telemetry_visible: true,
            preview: None,
            preview_area: size(px(700.), px(800.)),
            selected_source: None,
            audio_waves: std::collections::BTreeMap::new(),
            viewport: Rc::new(Cell::new(None)),
            notices: Vec::new(),
            sources: None,
            confirmation: None,
            frames_received: 0,
            copied: None,
            stop_confirm: false,
            tasks: Vec::new(),
        };
        app.subscribe(cx);
        app.tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(signal) = receiver.recv().await {
                if this
                    .update(cx, |app, cx| app.handle_signal(signal, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        #[cfg(feature = "e2e")]
        crate::e2e::start(&mut app, cx);
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
                        app.apply(snapshot, cx);
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

        let waves = Arc::clone(&hub);
        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                if let Some(update) = waves.take_waves() {
                    if this
                        .update(cx, |app, cx| {
                            for wave in update {
                                app.audio_waves.insert(wave.id, wave.columns);
                            }
                            if app.visible {
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                waves.waves_ready().await;
            }
        }));

        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                if let Some(frame) = hub.take_frame() {
                    #[cfg(feature = "e2e")]
                    crate::e2e::record_frame(&frame);
                    let image = preview_image(frame);
                    if this
                        .update(cx, |app, cx| {
                            if app.visible && app.view.capture != CaptureStage::Idle {
                                app.replace_preview(image, cx);
                            }
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
    fn apply(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        #[cfg(feature = "e2e")]
        let snapshot = crate::e2e::snapshot(snapshot);
        if snapshot.preferences != self.view.preferences || !self.view.hydrated {
            crate::settings::apply_appearance(&snapshot.preferences, cx);
        }
        if let Some(services) = &mut self.services {
            services.sync(&snapshot);
        }
        if snapshot.capture == CaptureStage::Idle {
            self.replace_preview(None, cx);
            self.clear_viewport();
        }
        self.view = snapshot;
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Notice(notice) => {
                if !self.visible {
                    cx.show_system_notification(SystemNotification {
                        tag: "slopcast-status".into(),
                        title: notice.title.clone().into(),
                        body: notice.message.clone().into(),
                        actions: Vec::new(),
                    });
                }
                self.notices.push(notice);
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(6))
                        .await;
                    let _ = this.update(cx, |app, cx| {
                        if !app.notices.is_empty() {
                            app.notices.remove(0);
                            cx.notify();
                        }
                    });
                })
                .detach();
                if self.notices.len() > 8 {
                    self.notices.remove(0);
                }
            }
            Event::ShowMain => {
                let entity = cx.entity();
                cx.defer(move |cx| show_main(&entity, cx));
            }
            Event::Confirm(confirmation) => {
                self.confirmation = Some(confirmation);
                if let Some(handle) = self.window {
                    let focus = self.focus.clone();
                    let _ = handle.update(cx, move |_, window, cx| focus.focus(window, cx));
                }
            }
            Event::OpenSourcePicker(sources) => {
                self.sources = Some(sources);
                self.selected_source = None;
            }
            Event::QuitNow => {
                self.confirmation = None;
                cx.quit();
                return;
            }
        }
        cx.notify();
    }

    pub(crate) fn send(&self, command: Command) {
        if let Some(presenter) = self.presenter.as_ref() {
            presenter.dispatch(command);
        }
    }

    #[cfg(feature = "e2e")]
    pub(crate) fn fixture_confirmation(&mut self) {
        self.confirmation = Some(Confirmation::CloseRoom);
    }

    fn replace_preview(&mut self, image: Option<Arc<RenderImage>>, cx: &mut Context<Self>) {
        if let Some(previous) = std::mem::replace(&mut self.preview, image) {
            cx.drop_image(previous, None);
        }
    }

    pub(super) fn open_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.settings
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        self.settings = crate::settings::open(
            cx.entity(),
            self.view.clone(),
            self.presenter
                .as_ref()
                .map(|presenter| presenter.subscribe()),
            cx,
        );
    }

    pub(super) fn request_shortcuts(
        &mut self,
        bindings: std::collections::BTreeMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        let result = self
            .presenter
            .as_ref()
            .ok_or("Presenter unavailable".to_string())
            .and_then(|presenter| presenter.validate_shortcuts(&bindings));
        match result {
            Ok(()) => {
                if let Some(services) = &mut self.services {
                    services.configure_shortcuts(bindings);
                }
            }
            Err(error) => self.show_error(error, cx),
        }
    }

    pub(super) fn show_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.handle_event(
            Event::Notice(Notice {
                level: NoticeLevel::Error,
                scope: desktop_core::presenter::NoticeScope::Main,
                title: "Slopcast".into(),
                message,
            }),
            cx,
        );
    }

    fn handle_signal(&mut self, signal: crate::services::Signal, cx: &mut Context<Self>) {
        use crate::services::Signal;
        match signal {
            Signal::ShowMain => {
                let entity = cx.entity();
                cx.defer(move |cx| show_main(&entity, cx));
            }
            Signal::Settings => self.open_settings(cx),
            Signal::Action(action) => self.send(Command::Action(action)),
            Signal::TrayReady(ready) => {
                self.tray_ready = ready;
                if !ready && self.window.is_none() {
                    let entity = cx.entity();
                    cx.defer(move |cx| show_main(&entity, cx));
                }
            }
            Signal::ShortcutsBound(generation, bindings, report) => {
                if !self
                    .services
                    .as_ref()
                    .is_some_and(|service| service.accepts_shortcuts(generation))
                {
                    return;
                }
                if let Some(presenter) = &self.presenter {
                    if let Err(error) = presenter.save_shortcuts(bindings) {
                        if let Some(services) = &mut self.services {
                            services
                                .rollback_shortcuts(&presenter.snapshot().preferences.shortcuts);
                        }
                        self.show_error(error, cx);
                    } else {
                        if let Some(services) = &mut self.services {
                            services.commit_shortcuts();
                        }
                        presenter.set_shortcut_report(report);
                    }
                }
            }
            Signal::ShortcutStatus(generation, report) => {
                if !self
                    .services
                    .as_ref()
                    .is_some_and(|service| service.accepts_shortcuts(generation))
                {
                    return;
                }
                if let Some(presenter) = &self.presenter {
                    presenter.set_shortcut_report(report);
                }
            }
            Signal::Error(error) => self.show_error(error, cx),
        }
        cx.notify();
    }

    fn clear_viewport(&mut self) {
        if self.viewport.replace(None).is_some() {
            media::clear_preview_viewport();
            #[cfg(feature = "e2e")]
            crate::e2e::record_viewport(None);
        }
    }

    fn close_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tray_ready && self.view.preferences.tray_enabled {
            self.window = None;
            self.visible = false;
            self.clear_viewport();
            self.replace_preview(None, cx);
            self.send(Command::SetMainVisible(false));
            window.remove_window();
        } else {
            self.send(Command::Action(DesktopAction::Quit));
        }
    }

    fn copy_room(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        let Some(room) = &self.view.room else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(if kind == "code" {
            room.code.clone()
        } else {
            room.share_url.clone()
        }));
        self.copied = Some(kind);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            let _ = this.update(cx, |app, cx| {
                app.copied = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        ui::title_bar(
            None,
            self.view.capture == CaptureStage::Live,
            self.title_drag.clone(),
            cx,
        )
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .children((self.view.capture == CaptureStage::Previewing).then(|| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(cx.theme().muted_foreground.opacity(0.6)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(cx.theme().muted_foreground)
                                .child("P R E V I E W"),
                        )
                })),
        )
        .child(
            button("settings", "")
                .ghost()
                .icon(IconName::Settings)
                .w(px(44.))
                .h(px(40.))
                .rounded(px(0.))
                .tooltip("Settings (Ctrl+,)")
                .on_click(cx.listener(|app, _, _, cx| app.open_settings(cx))),
        )
        .child(ui::window_buttons(
            cx,
            window.is_maximized(),
            cx.listener(|app, _, window, cx| app.close_main(window, cx)),
        ))
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.view.onboarding_completed {
            return div().into_any_element();
        }
        div().relative().flex().flex_col().gap_4().flex_shrink_0().mb_8().p_6().pb_7()
            .rounded_lg().border_1().border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.8))
            .child(div().flex().items_center().gap_3()
                .child(div().p_2().rounded_xl().bg(cx.theme().primary.opacity(0.1)).child(icon(IconName::ScreenShare, 20., cx.theme().primary)))
                .child(div().flex_1().child(div().text_base().font_bold().child("Welcome to Slopcast"))
                    .child(ui::caption("Share your screen with surgical audio control", cx)))
                .child(button("dismiss-welcome", "").ghost().icon(IconName::X).on_click(cx.listener(|app, _, _, cx| app.dismiss_welcome(cx)))))
            .child(div().flex().gap_3().children([
                (IconName::ScreenShare, "Create a room", "Click Create Live Room in the sidebar to get a shareable link."),
                (IconName::Users, "Pick what to share", "Select a window or video file. Audio is auto-detected from your chosen window."),
                (IconName::ArrowRight, "Go live", "Hit Start Screenshare. Share the link — spectators join instantly, no install."),
            ].into_iter().map(|(glyph, title, hint)| div().flex().flex_1().min_w_0().gap_3().p_3().rounded_lg()
                .bg(cx.theme().secondary).border_1().border_color(cx.theme().border)
                .child(div().flex_shrink_0().size(px(24.)).flex().items_center().justify_center().rounded_full()
                    .bg(cx.theme().primary.opacity(0.15)).child(icon(glyph, 14., cx.theme().primary)))
                .child(div().flex_1().min_w_0().child(div().font_semibold().child(title)).child(ui::caption(hint, cx))))))
            .child(div().flex().items_center().justify_between()
                .child(div().flex().items_center().gap_1p5().child(icon(IconName::Users, 14., cx.theme().muted_foreground))
                    .child(ui::caption("Spectators watch in-browser — no account needed", cx)))
                .child(button("welcome-got-it", "Got it").ghost().text_color(cx.theme().primary).icon(IconName::ArrowRight)
                    .on_click(cx.listener(|app, _, _, cx| app.dismiss_welcome(cx)))))
            .into_any_element()
    }

    fn dismiss_welcome(&mut self, cx: &mut Context<Self>) {
        if let Some(presenter) = &self.presenter
            && let Err(error) = presenter.complete_onboarding()
        {
            self.show_error(error, cx);
        }
    }

    fn render_preview(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Measure the actual padded container so the card also fits around the welcome guide.
        let width = self.preview_area.width.max(px(1.));
        let height = self.preview_area.height.max(px(1.));
        let preview_size = size(
            width.min(height * (16. / 9.)),
            height.min(width * (9. / 16.)),
        );
        let owner = cx.entity().downgrade();
        let active = self.visible && self.view.capture != CaptureStage::Idle;
        let content = if let Some(image) = self.preview.clone().filter(|_| active) {
            img(image)
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element()
        } else {
            let (title, hint) = match self.view.capture {
                CaptureStage::Idle if self.view.room.is_some() => (
                    "Ready to preview",
                    "Choose a window or screen to preview. You’ll go live from the preview.",
                ),
                CaptureStage::Idle => (
                    "No room yet",
                    "Create a live room to get your invite link, then pick a source to preview before you go live.",
                ),
                _ => (
                    "Waiting for your selection…",
                    "Select a window or screen in the system dialog. Your preview will appear here.",
                ),
            };
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .px_6()
                .size_full()
                .child(
                    div()
                        .p_3()
                        .mb_1()
                        .rounded_full()
                        .bg(cx.theme().primary.opacity(0.1))
                        .child(icon(IconName::ScreenShare, 28., cx.theme().primary)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(rgb(0xf5f6f8))
                        .child(title),
                )
                .child(
                    div()
                        .text_sm()
                        .text_center()
                        .max_w(px(320.))
                        .line_height(relative(1.625))
                        .text_color(rgb(0xa1a9b8))
                        .child(hint),
                )
                .children(
                    (self.view.room.is_some() && self.view.capture == CaptureStage::Idle).then(
                        || {
                            button(
                                "preview-copy-link",
                                if self.copied == Some("link") {
                                    "Link copied"
                                } else {
                                    "Copy invite link"
                                },
                            )
                            .ghost()
                            .text_xs()
                            .text_color(cx.theme().primary)
                            .on_click(cx.listener(|app, _, _, cx| app.copy_room("link", cx)))
                        },
                    ),
                )
                .into_any_element()
        };
        let viewport = Rc::clone(&self.viewport);
        #[cfg(feature = "e2e")]
        let paint_image = self.preview.is_some();
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .p_6()
            .child(self.render_welcome(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .size_full()
                            .on_children_prepainted(move |bounds, _, cx| {
                                if let Some(bounds) = bounds.first() {
                                    let size = bounds.size;
                                    let owner = owner.clone();
                                    cx.defer(move |cx| {
                                        let _ = owner.update(cx, |app, cx| {
                                            if app.preview_area != size {
                                                app.preview_area = size;
                                                cx.notify();
                                            }
                                        });
                                    });
                                }
                            })
                            .child(div().size_full()),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .relative()
                                    .flex_shrink_0()
                                    .w(preview_size.width)
                                    .h(preview_size.height)
                                    .rounded_lg()
                                    .overflow_hidden()
                                    .shadow_2xl()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(rgb(0x000000))
                                    .child(
                                        div()
                                            .size_full()
                                            .on_children_prepainted(move |bounds, window, _| {
                                                if !active {
                                                    return;
                                                }
                                                #[cfg(feature = "e2e")]
                                                if paint_image {
                                                    crate::e2e::record_paint();
                                                }
                                                let Some(bounds) = bounds.first().copied() else {
                                                    return;
                                                };
                                                let scale = window.scale_factor();
                                                let width = f32::from(bounds.size.width) * scale;
                                                let height = f32::from(bounds.size.height) * scale;
                                                if width < 1. || height < 1. {
                                                    return;
                                                }
                                                let size =
                                                    (width.round() as u32, height.round() as u32);
                                                if viewport.get() != Some(size) {
                                                    viewport.set(Some(size));
                                                    media::set_preview_viewport(size.0, size.1);
                                                    #[cfg(feature = "e2e")]
                                                    crate::e2e::record_viewport(Some(size));
                                                }
                                            })
                                            .child(content),
                                    )
                                    .children((self.view.capture == CaptureStage::Previewing).then(
                                        || {
                                            div()
                                                .absolute()
                                                .top_3()
                                                .left_3()
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .rounded_full()
                                                .px_2p5()
                                                .py_1()
                                                .bg(black().opacity(0.6))
                                                .child(
                                                    div()
                                                        .size(px(6.))
                                                        .rounded_full()
                                                        .bg(cx.theme().primary),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_semibold()
                                                        .text_color(white())
                                                        .child("PREVIEW"),
                                                )
                                        },
                                    ))
                                    .children(
                                        (self.view.capture == CaptureStage::Live
                                            && self.telemetry_visible)
                                            .then(|| self.render_telemetry(cx)),
                                    ),
                            ),
                    ),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(SIDEBAR_WIDTH)
            .h_full()
            .bg(ui::mix(cx.theme().sidebar, cx.theme().background, 0.6))
            .border_l_1()
            .border_color(cx.theme().border)
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_5()
                    .child(section_heading(
                        IconName::MonitorUp,
                        "SCREENSHARE SOURCE",
                        cx,
                    ))
                    .child(self.render_room_card(cx))
                    .children(
                        self.sources
                            .as_ref()
                            .map(|sources| self.render_sources(sources, cx)),
                    )
                    .child(self.render_capture_card(cx)),
            )
            .child(self.render_audio_card(cx))
    }

    fn render_room_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(room) = self.view.room.as_ref() else {
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    button(
                        "create-room",
                        if self.view.creating_room {
                            "Connecting Room…"
                        } else if self.view.retryable_replacement {
                            "Retry connection"
                        } else {
                            "Create Live Room"
                        },
                    )
                    .primary()
                    .w_full()
                    .font_bold()
                    .loading(self.view.creating_room)
                    .disabled(!self.view.retryable_replacement && !self.view.can_create_room())
                    .on_click(cx.listener(|app, _, _, _| {
                        app.send(if app.view.retryable_replacement {
                            Command::RetryRoomConnection
                        } else {
                            Command::CreateRoom
                        })
                    })),
                )
                .children(
                    self.view
                        .room_create_disabled_reason()
                        .map(|reason| ui::body(reason, cx)),
                )
                .into_any_element();
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .font_family("JetBrains Mono")
                            .font_semibold()
                            .child(room.code.clone()),
                    )
                    .children((room.spectators > 0).then(|| {
                        ui::caption(
                            format!(
                                "{} spectator{}",
                                room.spectators,
                                if room.spectators == 1 { "" } else { "s" }
                            ),
                            cx,
                        )
                    }))
                    .children(
                        (room.connection != RoomConnection::Connected)
                            .then(|| status_chip(room.connection, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "copy-code",
                            if self.copied == Some("code") {
                                "Copied"
                            } else {
                                "Copy code"
                            },
                        )
                        .secondary()
                        .h(px(36.))
                        .flex_1()
                        .on_click(cx.listener(|app, _, _, cx| app.copy_room("code", cx))),
                    )
                    .child(
                        button(
                            "copy-link",
                            if self.copied == Some("link") {
                                "Link copied"
                            } else {
                                "Copy link"
                            },
                        )
                        .primary()
                        .icon(IconName::Copy)
                        .h(px(36.))
                        .flex_1()
                        .on_click(cx.listener(|app, _, _, cx| app.copy_room("link", cx))),
                    ),
            )
            .child(
                button(
                    "close-room",
                    if self.view.closing_room {
                        "Closing…"
                    } else {
                        "Close room"
                    },
                )
                .outline()
                .icon(IconName::Link2Off)
                .h(px(36.))
                .w_full()
                .border_color(cx.theme().danger.opacity(0.5))
                .text_color(cx.theme().danger)
                .disabled(self.view.room_transitioning || self.view.closing_room)
                .on_click(
                    cx.listener(|app, _, _, _| app.send(Command::Action(DesktopAction::CloseRoom))),
                ),
            )
            .into_any_element()
    }

    fn render_capture_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.view.is_busy();
        let mut content = div().flex().flex_col().gap_2();
        match self.view.capture {
            CaptureStage::Idle => {
                content = content
                    .child(
                        button("start-share", "Start Screenshare")
                            .primary()
                            .icon(IconName::Video)
                            .w_full()
                            .font_bold()
                            .disabled(!self.view.can_start_share())
                            .on_click(cx.listener(|app, _, _, _| app.send(Command::StartShare))),
                    )
                    .child(if self.view.room.is_none() {
                        ui::body("Create a live room to start sharing.", cx)
                    } else {
                        ui::caption("Preview first — Go Live when the frame is ready", cx)
                    });
            }
            CaptureStage::Previewing => {
                content = content
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button("go-live", "Go Live")
                                    .primary()
                                    .flex_1()
                                    .font_bold()
                                    .disabled(!(self.preview.is_some() && !self.view.is_busy()))
                                    .on_click(
                                        cx.listener(|app, _, _, _| app.send(Command::GoLive)),
                                    ),
                            )
                            .child(
                                button("stop-share", "Cancel")
                                    .secondary()
                                    .flex_1()
                                    .disabled(busy)
                                    .on_click(
                                        cx.listener(|app, _, _, _| app.send(Command::StopShare)),
                                    ),
                            ),
                    )
                    .child(ui::body(
                        if self.preview.is_some() && !self.view.is_busy() {
                            "Previewing your capture — click Go Live to start broadcasting."
                        } else {
                            "Choose what to share in the portal dialog to preview it, then go live."
                        },
                        cx,
                    ));
            }
            CaptureStage::Live => {}
        }
        if self.view.capture != CaptureStage::Idle {
            content = content.child(
                button("switch-source", "Switch screenshare source…")
                    .outline()
                    .w_full()
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, _| app.send(Command::SwitchSource))),
            );
        }
        if self.view.capture == CaptureStage::Live {
            if self.stop_confirm {
                content = content.child(ui::body("Stop the stream?", cx)).child(
                    div()
                        .flex()
                        .gap_2()
                        .child(button("stop-share", "Stop").danger().flex_1().on_click(
                            cx.listener(|app, _, _, cx| {
                                app.stop_confirm = false;
                                app.send(Command::StopShare);
                                cx.notify();
                            }),
                        ))
                        .child(
                            button("cancel-stop", "Cancel")
                                .secondary()
                                .flex_1()
                                .on_click(cx.listener(|app, _, _, cx| {
                                    app.stop_confirm = false;
                                    cx.notify();
                                })),
                        ),
                );
            } else {
                content = content.child(
                    button("stop-share", "Stop Screenshare")
                        .danger()
                        .w_full()
                        .font_bold()
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.stop_confirm = true;
                            cx.notify();
                        })),
                );
            }
        }
        content.into_any_element()
    }

    fn render_audio_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let audio = &self.view.audio;
        let mut choices = vec![
            ("No Audio".to_string(), NO_AUDIO, None, true),
            (
                "Desktop Audio (All System Sound)".to_string(),
                SYSTEM_AUDIO,
                Some("All system audio".to_string()),
                false,
            ),
        ];
        for group in &audio.groups {
            let app = &group.representative;
            let subtitle = app
                .media_title
                .clone()
                .or(app.window_title.clone())
                .or_else(|| {
                    (group.members.len() > 1)
                        .then(|| format!("{} audio streams", group.members.len()))
                });
            let selected = desktop_core::audio::is_group_selected(&group.members, audio.selected);
            choices.push((app.name.clone(), app.id, subtitle, selected));
        }
        let rows = choices
            .into_iter()
            .map(|(name, id, subtitle, group_selected)| {
                let selected = Some(id) == audio.selected || (id > 0 && group_selected);
                let mut columns = vec![0.0_f64; 96];
                let ids: Vec<i32> = audio
                    .groups
                    .iter()
                    .find(|g| g.representative.id == id)
                    .map_or_else(|| vec![id], |g| g.members.iter().map(|m| m.id).collect());
                for member in &ids {
                    if let Some(wave) = self.audio_waves.get(member) {
                        for (index, value) in wave.iter().copied().take(columns.len()).enumerate() {
                            if index % 2 == 0 {
                                columns[index] = columns[index].min(value);
                            } else {
                                columns[index] = columns[index].max(value);
                            }
                        }
                    }
                }
                let color = if selected {
                    cx.theme().primary
                } else {
                    cx.theme().foreground
                };
                div()
                    .id(SharedString::from(format!("audio-{id}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(if selected {
                        cx.theme().primary.opacity(0.3)
                    } else {
                        cx.theme().border
                    })
                    .bg(if selected {
                        cx.theme().primary.opacity(0.15)
                    } else {
                        ui::mix(cx.theme().background, cx.theme().sidebar, 0.6)
                    })
                    .cursor_pointer()
                    .hover(|style| style.border_color(cx.theme().input))
                    .text_color(color)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .truncate()
                                            .min_w_0()
                                            .child(name),
                                    )
                                    .children(
                                        columns
                                            .iter()
                                            .any(|value| value.abs() > 0.001)
                                            .then(|| ui::waveform(columns, color)),
                                    ),
                            )
                            .children(subtitle.map(|value| {
                                div().text_xs().text_color(color.opacity(0.6)).child(value)
                            })),
                    )
                    .children(selected.then(|| icon(IconName::Check, 16., color)))
                    .on_click(cx.listener(move |app, _, _, _| app.send(Command::SelectAudio(id))))
            });
        let hint = if self.view.preferences.auto_select_audio {
            "Audio is matched to your shared application when possible. Unmatched sources stay video-only."
        } else {
            "Automatic audio selection is off. No Audio is selected until you choose an app."
        };
        div().flex().flex_col().gap_4().p_5().border_t_1().border_color(cx.theme().border)
            .child(div().flex().items_center().justify_between()
                .child(section_heading(IconName::AudioLines, "WINDOW AUDIO CAPTURE", cx))
                .child(button("refresh-audio", "Refresh").ghost().text_xs().px_2().h_auto()
                    .on_click(cx.listener(|app, _, _, _| app.send(Command::RefreshAudioApps)))))
            .child(ui::body(format!("{hint} Select an app to share only its sound, or choose Desktop Audio explicitly to capture all system sound."), cx))
            .child(button("mute", if self.view.muted { "Unmute audio source" } else { "Mute audio source" }).outline().w_full().justify_start()
                .icon(if self.view.muted { IconName::VolumeX } else { IconName::Volume2 })
                .on_click(cx.listener(|app, _, _, _| app.send(Command::ToggleMute))))
            .child(ui::caption("Mute silences the shared audio without changing your selected source.", cx))
            .child(div().flex().flex_col().gap_1p5().children(rows))
    }

    fn render_telemetry(&self, cx: &App) -> AnyElement {
        let t = &self.view.telemetry;
        let mut cells = vec![
            ("CODEC", t.video_codec.clone().unwrap_or_else(|| "—".into())),
            (
                "RESOLUTION",
                t.width
                    .zip(t.height)
                    .map_or_else(|| "—".into(), |(w, h)| format!("{w}×{h}")),
            ),
            (
                "FRAME RATE",
                t.frame_rate
                    .map_or_else(|| "—".into(), |v| format!("{v:.0} fps")),
            ),
            (
                "CAPTURE",
                t.capture_fps
                    .map_or_else(|| "—".into(), |v| format!("{v:.0} fps")),
            ),
            (
                "CURSOR",
                if t.cursor_missing && t.cursor_frames == 0 {
                    "not in stream"
                } else if t.cursor_frames > 0 {
                    "tracked"
                } else {
                    "—"
                }
                .into(),
            ),
            (
                "BITRATE",
                t.video_bitrate.map_or_else(|| "—".into(), megabits),
            ),
            (
                "LOSS",
                t.packet_loss_pct
                    .map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
            ),
            (
                "AUDIO",
                if t.has_audio {
                    t.audio_codec.clone().unwrap_or_else(|| "—".into())
                } else {
                    "video only".into()
                },
            ),
        ];
        let seconds = t.elapsed_ms / 1000;
        cells.push((
            "ELAPSED",
            format!("{:02}:{:02}", seconds / 60, seconds % 60),
        ));
        div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .px_4()
            .pb_3()
            .pt_12()
            .bg(linear_gradient(
                0.,
                linear_color_stop(black().opacity(0.95), 0.),
                linear_color_stop(black().opacity(0.), 1.),
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap_x_5()
                    .gap_y_3()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().primary)
                            .child("● ON AIR"),
                    )
                    .children(cells.into_iter().map(|(label, value)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(rgb(0xa1a9b8))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .font_family("JetBrains Mono")
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(rgb(0xf5f6f8))
                                    .child(value),
                            )
                    })),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .mt_2()
                    .child(ui::sparkline(t.bitrate_history.clone(), cx.theme().primary)),
            )
            .into_any_element()
    }

    fn render_notices(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .bottom_4()
            .right_4()
            .w(px(356.))
            .flex()
            .flex_col()
            .gap_2()
            .children(self.notices.iter().rev().take(3).map(|notice| {
                let color = match notice.level {
                    NoticeLevel::Info => cx.theme().muted_foreground,
                    NoticeLevel::Success => SUCCESS,
                    NoticeLevel::Error => cx.theme().danger,
                };
                div()
                    .p_4()
                    .rounded_lg()
                    .shadow_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(color)
                            .child(notice.title.clone()),
                    )
                    .child(ui::body(notice.message.clone(), cx))
            }))
    }

    fn render_platform_notice(&self, cx: &App) -> AnyElement {
        let linux = self.view.platform.platform == "linux";
        div().flex_1().flex().items_center().justify_center().px_6()
            .child(div().max_w(px(512.)).w_full().p_8().rounded_lg().border_1().border_color(cx.theme().border).bg(cx.theme().secondary.opacity(0.8))
                .flex().flex_col().items_center().text_center()
                .child(div().p_3().mb_4().rounded_xl().bg(cx.theme().primary.opacity(0.1)).child(icon(IconName::MonitorX,24.,cx.theme().primary)))
                .child(div().text_lg().font_bold().child(if linux { "Wayland session required" } else { "Screen capture not supported" }))
                .child(div().mt_2().child(ui::body(if linux { "Slopcast requires a Wayland session (KDE/GNOME). Screen sharing is not available on X11." } else { "Screen sharing is not available on this platform yet." },cx)))
                .child(div().mt_4().child(ui::caption(if linux { "Log out and choose a Wayland session in your display manager, then start Slopcast again." } else { "Run Slopcast on Linux (Wayland) or Windows to share your screen." },cx))))
            .into_any_element()
    }

    fn render_sources(&self, sources: &[CaptureSource], cx: &mut Context<Self>) -> AnyElement {
        let mut content = div()
            .flex()
            .flex_col()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(ui::caption("CHOOSE WHAT TO SHARE", cx))
                    .child(
                        button("cancel-picker", "Cancel")
                            .ghost()
                            .h(px(28.))
                            .px_2()
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.sources = None;
                                app.selected_source = None;
                                app.send(Command::CancelSourcePicker);
                                cx.notify();
                            })),
                    ),
            );
        for (kind, label, glyph) in [
            (SourceKind::Screen, "Screens", IconName::Monitor),
            (SourceKind::Window, "Windows", IconName::AppWindow),
        ] {
            let matching: Vec<_> = sources
                .iter()
                .filter(|source| source.kind == kind)
                .collect();
            if matching.is_empty() {
                continue;
            }
            content = content.child(ui::caption(label, cx));
            for source in matching {
                let selection = media::SourceSelection {
                    kind: source.kind,
                    id: source.id,
                };
                let selected = self
                    .selected_source
                    .as_ref()
                    .is_some_and(|s| s.kind == selection.kind && s.id == selection.id);
                content = content.child(
                    button(
                        SharedString::from(format!(
                            "source-{}-{}",
                            source_kind_label(kind),
                            source.id
                        )),
                        source.title.clone(),
                    )
                    .outline()
                    .h(px(36.))
                    .w_full()
                    .justify_start()
                    .icon(glyph)
                    .toggled(selected)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.selected_source = Some(selection);
                        cx.notify();
                    })),
                );
            }
        }
        if sources.is_empty() {
            content = content.child(ui::body(
                "No capture sources found — open a window or connect a display, then retry.",
                cx,
            ));
        }
        content
            .child(
                button("share-selected", "Share")
                    .primary()
                    .w_full()
                    .font_bold()
                    .disabled(self.selected_source.is_none())
                    .on_click(cx.listener(|app, _, _, cx| {
                        if let Some(source) = app.selected_source.take() {
                            app.sources = None;
                            app.send(Command::SourceSelected(source));
                            cx.notify();
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(confirmation) = self.confirmation else {
            return div().into_any_element();
        };
        let (title, message, confirm, consequence) = match confirmation {
            Confirmation::CloseRoom => (
                "Close this room?",
                "Sharing will stop and spectators will disconnect. Closing the room makes its link unusable.",
                "Close room",
                Command::CloseRoom,
            ),
            Confirmation::Quit => (
                "Quit Slopcast?",
                "Sharing will stop and spectators will disconnect. Closing the room makes its link unusable.",
                "Quit",
                Command::Quit { anyway: false },
            ),
            Confirmation::QuitAnyway => (
                "Quit anyway?",
                "The server could not close your room. Capture will stop, but the link may remain until the server expires it. Quit anyway?",
                "Quit anyway",
                Command::Quit { anyway: true },
            ),
        };

        dialog(
            title,
            cx,
            text(message),
            vec![
                button("cancel-confirm", "Cancel")
                    .secondary()
                    .on_click(cx.listener(|app, _, _, _| app.confirmation = None))
                    .into_any_element(),
                button("confirm", confirm)
                    .danger()
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(cx.theme().font_size * (8. / 7.));
        #[cfg(feature = "e2e")]
        crate::e2e::record_dialog(self.confirmation.is_some());
        div()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    if app.sources.take().is_some() {
                        app.send(Command::CancelSourcePicker);
                    }
                    app.confirmation = None;
                    app.stop_confirm = false;
                    cx.notify();
                    cx.stop_propagation();
                } else if event.keystroke.modifiers.control && event.keystroke.key == "," {
                    app.open_settings(cx);
                    cx.stop_propagation();
                } else if telemetry_key(&event.keystroke, &app.view.preferences.telemetry_shortcut)
                {
                    app.telemetry_visible = !app.telemetry_visible;
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_sm()
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .text_size(cx.theme().font_size)
            .font_weight(FontWeight(f32::from(
                self.view.preferences.typography.font_weight.unwrap_or(400),
            )))
            .font_features(if self.view.preferences.typography.ligatures {
                FontFeatures::default()
            } else {
                FontFeatures::disable_ligatures()
            })
            .child(self.render_header(window, cx))
            .child(if self.view.platform.video_capture_available {
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_preview(window, cx))
                    .child(self.render_sidebar(cx))
                    .into_any_element()
            } else {
                self.render_platform_notice(cx)
            })
            .child(self.render_notices(cx))
            .child(self.render_overlay(cx))
    }
}

fn preview_image(frame: media::PreviewFrame) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    // GPUI's renderer consumes BGRA; the native preview payload is already in that order.
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

fn text(value: impl Into<SharedString>) -> AnyElement {
    div().text_sm().child(value.into()).into_any_element()
}

fn status_chip(connection: RoomConnection, cx: &App) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .text_color(cx.theme().muted_foreground)
        .text_xs()
        .child(connection_label(connection))
        .into_any_element()
}

fn dialog(title: &'static str, cx: &App, body: AnyElement, actions: Vec<AnyElement>) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(black().opacity(0.8))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .w(px(512.))
                .p_6()
                .rounded_lg()
                .bg(cx.theme().background)
                .border_1()
                .border_color(cx.theme().border)
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

fn telemetry_key(key: &Keystroke, stored: &str) -> bool {
    let Ok(chord) = desktop_core::shortcuts::Chord::parse(stored) else {
        return false;
    };
    let code = chord
        .code
        .strip_prefix("Key")
        .or_else(|| chord.code.strip_prefix("Digit"))
        .unwrap_or(&chord.code);
    let code = match code {
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "Space" => "space",
        other => other,
    };
    key.key.eq_ignore_ascii_case(code)
        && key.modifiers.control == chord.modifiers.control
        && key.modifiers.shift == chord.modifiers.shift
        && key.modifiers.alt == chord.modifiers.alt
        && key.modifiers.platform == chord.modifiers.command
}

#[cfg(test)]
mod tests {
    use super::telemetry_key;
    use gpui_kit::Keystroke;

    #[test]
    fn telemetry_key_honors_saved_key_and_exact_modifiers() {
        assert!(telemetry_key(
            &Keystroke::parse("ctrl-shift-t").unwrap(),
            "Control+Shift+KeyT"
        ));
        assert!(!telemetry_key(
            &Keystroke::parse("ctrl-t").unwrap(),
            "Control+Shift+KeyT"
        ));
        assert!(!telemetry_key(
            &Keystroke::parse("ctrl-alt-t").unwrap(),
            "Control+KeyT"
        ));
        assert!(!telemetry_key(&Keystroke::parse("ctrl-t").unwrap(), ""));
    }
}
