//! Native settings window observing the same presenter as the main window.

use crate::app::PresenterApp;
use crate::ui::{self, button};
use desktop_core::preferences::{
    ACTIONS, AppearanceMode, ColorTheme, DesktopPreferences, FontFamily,
};
use desktop_core::presenter::{Command, Snapshot};
use gpui_kit::assets::IconName;
use gpui_kit::component::IndexPath;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, Theme, ThemeMode};
use gpui_kit::*;
use std::collections::BTreeMap;

pub fn open(
    owner: Entity<PresenterApp>,
    view: Snapshot,
    snapshots: Option<tokio::sync::watch::Receiver<Snapshot>>,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(180.), px(140.)),
            size(px(720.), px(760.)),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Slopcast Settings".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        window_min_size: Some(size(px(420.), px(420.))),
        window_decorations: Some(WindowDecorations::Client),
        app_owns_titlebar_drag: true,
        app_id: Some("org.slopcast.Slopcast".into()),
        ..Default::default()
    };
    match open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsView::new(owner, view, snapshots, window, cx))
    }) {
        Ok((handle, _)) => Some(handle),
        Err(error) => {
            log::error!("Cannot open settings: {error}");
            None
        }
    }
}

type SelectOptions = (&'static str, String, Vec<(String, String)>);

struct SettingsView {
    owner: Entity<PresenterApp>,
    view: Snapshot,
    endpoint: Entity<InputState>,
    bitrate: Entity<InputState>,
    directory: Entity<InputState>,
    telemetry: Entity<InputState>,
    shortcuts: BTreeMap<String, Entity<InputState>>,
    error: Option<String>,
    tab: usize,
    selects: BTreeMap<&'static str, Entity<SelectState<Vec<SharedString>>>>,
    _controls: Vec<Subscription>,
    focus: FocusHandle,
    title_drag: ui::TitleDrag,
    editing_shortcut: Option<&'static str>,
    bitrate_slider: Entity<SliderState>,
    _snapshot: Task<()>,
}

impl SettingsView {
    fn new(
        owner: Entity<PresenterApp>,
        view: Snapshot,
        snapshots: Option<tokio::sync::watch::Receiver<Snapshot>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |value: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        let endpoint = field(view.source_endpoint().into(), window, cx);
        let bitrate = field(
            format!("{}", view.settings.bitrate_limit / 1_000_000.),
            window,
            cx,
        );
        let directory = field(view.settings_directory.clone(), window, cx);
        let telemetry = field(view.preferences.telemetry_shortcut.clone(), window, cx);
        let shortcuts = ACTIONS
            .iter()
            .map(|(id, _, _)| {
                (
                    (*id).into(),
                    field(
                        view.preferences
                            .shortcuts
                            .get(*id)
                            .cloned()
                            .unwrap_or_default(),
                        window,
                        cx,
                    ),
                )
            })
            .collect();
        let bitrate_input = bitrate.clone();
        let mut selects = BTreeMap::new();
        let mut controls = Vec::new();
        controls.push(cx.subscribe(&bitrate_input, |this, input, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. })
                && !this.view.settings.auto_bitrate
            {
                match input.read(cx).value().parse::<f64>() {
                    Ok(mbps) if mbps.is_finite() && (1.0..=200.0).contains(&mbps) => {
                        this.error = None;
                        this.stream_change("bitrateLimit", (mbps * 1_000_000.).into(), cx);
                    }
                    _ => {
                        this.error = Some("Enter a bitrate between 1 and 200 Mbps.".into());
                        cx.notify();
                    }
                }
            }
        }));

        let codecs: Vec<(String, String)> = view
            .codecs
            .iter()
            .map(|codec| (codec.codec.clone(), codec.label.clone()))
            .collect();
        let mut options: Vec<SelectOptions> = vec![
            ("videoCodec", view.settings.video_codec.clone(), codecs),
            (
                "resolution",
                view.settings.resolution.clone(),
                [
                    ("480p", "480p (SD)"),
                    ("720p", "720p (HD)"),
                    ("1080p", "1080p (Full HD)"),
                    ("1440p", "1440p (QHD)"),
                    ("2160p", "4K (Ultra HD)"),
                ]
                .map(|(v, l)| (v.into(), l.into()))
                .to_vec(),
            ),
            (
                "fps",
                view.settings.fps.to_string(),
                [15, 24, 30, 60]
                    .map(|v| (v.to_string(), format!("{v} fps")))
                    .to_vec(),
            ),
            (
                "motionMode",
                view.settings.motion_mode.clone(),
                [
                    ("auto", "Auto-detect"),
                    ("static", "Static (documents, slides)"),
                    ("mixed", "Mixed (browsing, demos)"),
                    ("dynamic", "Dynamic (gaming, video)"),
                ]
                .map(|(v, l)| (v.into(), l.into()))
                .to_vec(),
            ),
        ];
        options.extend([
            (
                "fontFamily",
                preferences_font_key(view.preferences.typography.font_family).into(),
                FontFamily::ALL
                    .map(|font| (preferences_font_key(font).into(), font.label().into()))
                    .to_vec(),
            ),
            (
                "fontSize",
                view.preferences
                    .typography
                    .font_size
                    .map_or(String::new(), |v| v.to_string()),
                std::iter::once((String::new(), "Theme default (14 px)".into()))
                    .chain((12..=18).map(|v| (v.to_string(), format!("{v} px"))))
                    .collect(),
            ),
            (
                "fontWeight",
                view.preferences
                    .typography
                    .font_weight
                    .map_or(String::new(), |v| v.to_string()),
                [
                    ("", "Theme default (regular)"),
                    ("400", "Regular"),
                    ("500", "Medium"),
                    ("600", "Semibold"),
                ]
                .map(|(v, l)| (v.into(), l.into()))
                .to_vec(),
            ),
        ]);
        let bitrate_slider = cx.new(|_| {
            SliderState::new()
                .min(1.)
                .max(200.)
                .step(0.5)
                .default_value((view.settings.bitrate_limit / 1_000_000.) as f32)
        });
        controls.push(
            cx.subscribe_in(&bitrate_slider, window, |this, _, event, window, cx| {
                if let SliderEvent::Change(SliderValue::Single(value)) = event {
                    this.bitrate.update(cx, |input, cx| {
                        input.set_value(format!("{value}"), window, cx)
                    });
                    this.stream_change("bitrateLimit", (f64::from(*value) * 1_000_000.).into(), cx);
                }
            }),
        );
        for (key, current, options) in options {
            let index = options.iter().position(|(value, _)| value == &current);
            let items: Vec<SharedString> = options
                .iter()
                .map(|(_, label)| label.clone().into())
                .collect();
            let select =
                cx.new(|cx| SelectState::new(items, index.map(IndexPath::new), window, cx));
            controls.push(cx.subscribe(&select, move |this, _, event, cx| {
                if let SelectEvent::Confirm(Some(label)) = event
                    && let Some((value, _)) =
                        options.iter().find(|(_, l)| l.as_str() == label.as_ref())
                {
                    if key.starts_with("font") {
                        let mut typography = this.view.preferences.typography.clone();
                        match key {
                            "fontFamily" => {
                                typography.font_family = FontFamily::ALL
                                    .into_iter()
                                    .find(|font| preferences_font_key(*font) == value)
                                    .unwrap_or(FontFamily::Theme)
                            }
                            "fontSize" => typography.font_size = value.parse().ok(),
                            _ => typography.font_weight = value.parse().ok(),
                        }
                        this.save(cx, |p| p.save_typography(typography));
                    } else {
                        this.stream_change(
                            key,
                            if key == "fps" {
                                value.parse::<f64>().unwrap_or(60.).into()
                            } else {
                                value.clone().into()
                            },
                            cx,
                        );
                    }
                }
            }));
            selects.insert(key, select);
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let task = cx.spawn(async move |this, cx| {
            if let Some(mut snapshots) = snapshots {
                while snapshots.changed().await.is_ok() {
                    let snapshot = snapshots.borrow_and_update().clone();
                    if this
                        .update(cx, |settings, cx| {
                            settings.view = snapshot;
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        Self {
            owner,
            view,
            endpoint,
            bitrate,
            directory,
            telemetry,
            shortcuts,
            error: None,
            tab: 0,
            selects,
            _controls: controls,
            focus,
            title_drag: Default::default(),
            editing_shortcut: None,
            bitrate_slider,
            _snapshot: task,
        }
    }

    fn dispatch(&self, command: Command, cx: &mut Context<Self>) {
        self.owner.update(cx, |app, _| app.send(command));
    }

    fn save(
        &mut self,
        cx: &mut Context<Self>,
        save: impl FnOnce(&desktop_core::Presenter) -> Result<(), String>,
    ) {
        self.error = self
            .owner
            .update(cx, |app, _| {
                app.presenter
                    .as_ref()
                    .ok_or("Presenter unavailable".to_string())
                    .and_then(save)
            })
            .err();
        cx.notify();
    }

    fn stream_change(&self, key: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        let mut changes = serde_json::Map::new();
        changes.insert(key.into(), value);
        self.dispatch(Command::UpdateStreamSettings(changes), cx);
    }

    fn stream(&self, cx: &mut Context<Self>) -> AnyElement {
        let field = |key: &'static str, title: &'static str| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_w_0()
                .gap_1p5()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child(title),
                )
                .child(
                    Select::new(self.selects.get(key).expect("stream select"))
                        .h(px(40.))
                        .w_full(),
                )
        };
        let mut block = section("Stream & encoder", cx)
            .child(ui::body("Choose video quality and your API server.", cx))
            .child(div().flex().gap_4().child(field("videoCodec", "VIDEO CODEC")).child(field("resolution", "RESOLUTION")))
            .child(div().flex().gap_4().child(field("fps", "FRAME RATE"))
                .child(div().flex().flex_col().flex_1().min_w_0().gap_1p5()
                    .child(label("BITRATE LIMIT"))
                    .child(div().flex().items_center().gap_3()
                        .child(Input::new(&self.bitrate).w(px(80.)).large().disabled(self.view.settings.auto_bitrate))
                        .child(ui::body("Mbps", cx))
                        .child(Slider::new(&self.bitrate_slider).disabled(self.view.settings.auto_bitrate)))))
            .child(div().flex().items_center().justify_between().gap_4()
                .child(div().flex_1().min_w_0().child(label("AUTOMATIC BITRATE")).child(ui::caption("Derive the bitrate from codec, resolution, framerate and content motion.", cx)))
                .child(Switch::new("auto-bitrate").checked(self.view.settings.auto_bitrate)
                    .on_click(cx.listener(|this, value: &bool, _, cx| this.stream_change("autoBitrate", (*value).into(), cx)))))
            .child(field("motionMode", "CONTENT MOTION"))
            .child(div().border_t_1().border_color(cx.theme().border).pt_4().flex().flex_col().gap_2()
                .child(label("API ENDPOINT"))
                .child(Input::new(&self.endpoint).large())
                .child(div().flex().gap_2()
                    .child(button("validate-endpoint", "Check and apply").primary().on_click(cx.listener(|this, _, _, cx| {
                        let endpoint = this.endpoint.read(cx).value().to_string(); this.dispatch(Command::ValidateEndpointDraft(endpoint), cx);
                    })))
                    .child(button("reset-endpoint", "Use default").ghost().on_click(cx.listener(|this, _, _, cx| this.dispatch(Command::EndpointReset, cx))))))
            .children(self.view.endpoint_draft.as_ref().and_then(|draft| draft.message.clone()).or(self.view.endpoint.message.clone()).map(|message| ui::body(message, cx)));
        if let Some(candidate) = &self.view.endpoint_switch_candidate {
            block = block
                .child(ui::body(
                    format!("Switching to {candidate} replaces the current room and its link."),
                    cx,
                ))
                .child(
                    button("switch-endpoint", "Replace room and switch")
                        .danger()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dispatch(Command::ConfirmEndpointSwitch, cx)
                        })),
                )
                .child(
                    button("defer-endpoint", "Use after this room closes").on_click(cx.listener(
                        |this, _, _, cx| this.dispatch(Command::DeferEndpointSwitch, cx),
                    )),
                );
        }
        block.into_any_element()
    }

    fn general(&self, cx: &mut Context<Self>) -> AnyElement {
        section("Settings storage", cx)
            .child(ui::body("Keep your preferences, stream settings and onboarding state in a folder you choose. Existing settings are copied safely; originals are kept as a backup.", cx))
            .child(div().flex().flex_wrap().gap_2().child(Input::new(&self.directory).large().min_w_0().flex_1().flex_basis(px(256.)))
                .child(button("move-settings", "Apply").outline().on_click(cx.listener(|this, _, _, cx| {
                    let directory = std::path::PathBuf::from(this.directory.read(cx).value().to_string()); this.save(cx, |p| p.set_settings_directory(&directory));
                })))
                .child(
                button("choose-directory", "Choose folder…").outline().icon(IconName::FolderOpen)
                    .on_click(cx.listener(|_, _, window, cx| {
                        let selected = cx.prompt_for_paths(PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: Some("Move Slopcast settings".into()),
                        });
                        let mut window_cx = window.to_async(cx);
                        cx.spawn(async move |this, cx| {
                            if let Ok(Ok(Some(paths))) = selected.await
                                && let Some(path) = paths.first()
                            {
                                let path = path.display().to_string();
                                let _ = this.update(cx, |settings, cx| {
                                    let _ = window_cx.update(|window, cx| {
                                        settings.directory.update(cx, |input, cx| {
                                            input.set_value(path, window, cx)
                                        })
                                    });
                                    cx.notify();
                                });
                            }
                        })
                        .detach();
                    })),
            )
            )
            .child(ui::caption("Choose a folder without existing Slopcast settings. Packaged server configuration, logs and browser data are not moved.", cx))
            .child(div().border_t_1().border_color(cx.theme().border).mt_4().pt_6().flex().gap_6()
                .child(div().flex_1().min_w_0().child(div().font_semibold().child("Tray icon"))
                    .child(ui::body("Control sharing, mute audio or quit from the tray. When enabled, closing the main window keeps Slopcast running. Quit ends sharing and closes your room.", cx)))
                .child(Switch::new("tray-enabled").checked(self.view.preferences.tray_enabled).on_click(cx.listener(|this, value: &bool, _, cx| {
                    let auto = this.view.preferences.auto_select_audio; this.save(cx, |p| p.save_general(*value, auto));
                }))))
            .child(div().border_t_1().border_color(cx.theme().border).mt_4().pt_6().flex().gap_6()
                .child(div().flex_1().min_w_0().child(div().font_semibold().child("Select application audio automatically"))
                    .child(ui::body("Match audio to the application you share. If no single application matches, share video only. Turn this off to start with No Audio; you can always choose a source manually.", cx))
                    .child(ui::caption("Your explicit audio choice is preserved. All system sound is never selected automatically.", cx)))
                .child(Switch::new("auto-audio").checked(self.view.preferences.auto_select_audio).on_click(cx.listener(|this, value: &bool, _, cx| {
                    let tray = this.view.preferences.tray_enabled; this.save(cx, |p| p.save_general(tray, *value));
                }))))
            .child(button("open-logs", "Open log folder").ghost().on_click(|_, _, cx| {
                if let Some(directory) = crate::logging::directory() { cx.reveal_path(&directory); }
            }))
            .children(self.view.preferences_error.clone().map(label)).into_any_element()
    }

    fn appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        section("Color mode", cx)
            .child(ui::body("System follows your device’s light or dark appearance.", cx))
            .child(div().flex().gap_2().children(AppearanceMode::ALL.into_iter().map(|mode| {
                let selected = self.view.preferences.appearance_mode == mode;
                button(SharedString::from(format!("mode-{mode:?}")), mode.label()).outline().flex_1().h(px(46.))
                    .icon(match mode { AppearanceMode::System => IconName::Monitor, AppearanceMode::Light => IconName::Sun, AppearanceMode::Dark => IconName::Moon })
                    .border_color(if selected { cx.theme().primary } else { cx.theme().border })
                    .bg(if selected { cx.theme().primary.opacity(0.1) } else { transparent_black() })
                    .on_click(cx.listener(move |this, _, _, cx| { let theme = this.view.preferences.color_theme; this.save(cx, |p| p.save_appearance(mode, theme)); }))
            })))
            .child(div().mt_4().font_semibold().child("Theme"))
            .child(ui::body("Each theme includes a light and dark palette. Previews show both.", cx))
            .child(div().flex().flex_col().children(ColorTheme::ALL.into_iter().zip([
                "Default · warm bronze", "Light / Dark · amber and slate", "Dawn / Pine · soft, muted tones", "Lotus / Wave · ink and parchment", "Alucard / Classic · purple accents"
            ]).map(|(theme, description)| {
                let selected = self.view.preferences.color_theme == theme;
                div().id(SharedString::from(format!("theme-{theme:?}"))).flex().items_center().gap_3().px_2().py_4().cursor_pointer()
                    .border_t_1().border_color(cx.theme().border).hover(|style| style.bg(cx.theme().secondary.opacity(0.5)))
                    .child(div().size(px(16.)).rounded_full().border_1().border_color(if selected { cx.theme().primary } else { cx.theme().muted_foreground })
                        .flex().items_center().justify_center().children(selected.then(|| div().size(px(8.)).rounded_full().bg(cx.theme().primary))))
                    .child(div().flex_1().child(div().font_medium().child(theme.label())).child(ui::caption(description, cx)))
                    .children([false,true].map(|dark| {
                        let [bg,fg,_,primary,secondary,_,_,border] = palette(theme, dark);
                        div().flex().gap_1().p_1p5().rounded_md().border_1().border_color(rgb(border)).bg(rgb(bg))
                            .children([fg,secondary,primary].map(|color| div().size(px(12.)).rounded_sm().bg(rgb(color))))
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| { let mode = this.view.preferences.appearance_mode; this.save(cx, |p| p.save_appearance(mode, theme)); }))
            })))
            .child(div().mt_4().font_semibold().child("Typography"))
            .child(ui::body("Theme defaults pair a font with each palette. Your overrides stay when you switch themes.", cx))
            .child(label("Font family"))
            .child(Select::new(self.selects.get("fontFamily").expect("font select")).h(px(40.)).w_full())
            .child(div().flex().gap_4()
                .child(div().flex_1().flex().flex_col().gap_1p5().child(label("Base text size"))
                    .child(Select::new(self.selects.get("fontSize").expect("font size select")).h(px(40.)).w_full()))
                .child(div().flex_1().flex().flex_col().gap_1p5().child(label("Body weight"))
                    .child(Select::new(self.selects.get("fontWeight").expect("font weight select")).h(px(40.)).w_full())))
            .child(div().flex().items_center().justify_between().gap_4()
                .child(div().flex_1().min_w_0().child(label("Font ligatures")).child(ui::caption("Use joined characters when supported by your font.", cx)))
                .child(Switch::new("ligatures").checked(self.view.preferences.typography.ligatures)
                    .on_click(cx.listener(|this, value: &bool, _, cx| { let mut typography = this.view.preferences.typography.clone(); typography.ligatures = *value; this.save(cx, |p| p.save_typography(typography)); }))))
            .into_any_element()
    }

    fn keyboard(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut block =
            section("Global keyboard shortcuts", cx)
                .child(ui::body("Use these even when Slopcast is not focused. Start opens a preview; press it again once the frame is ready to go live. Stop ends capture immediately, leaving your room open.", cx))
                .child(ui::body(self.view.shortcuts.status.clone(), cx));
        for (id, description, default) in ACTIONS {
            if let Some(input) = self.shortcuts.get(id) {
                let mut row = div()
                    .py_4()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .flex_col()
                    .gap_2();
                let mut controls = div().flex().items_center().gap_2();
                if self.editing_shortcut == Some(id) {
                    controls = controls
                        .child(Input::new(input).h(px(36.)).w(px(220.)))
                        .child(
                            button(SharedString::from(format!("done-{id}")), "Done")
                                .ghost()
                                .h(px(36.))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editing_shortcut = None;
                                    cx.notify();
                                })),
                        );
                } else {
                    controls = controls
                        .child(
                            button(
                                SharedString::from(format!("edit-{id}")),
                                format_shortcut(input.read(cx).value().as_ref()),
                            )
                            .outline()
                            .font_family("JetBrains Mono")
                            .text_xs()
                            .h(px(40.))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.editing_shortcut = Some(id);
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            button(SharedString::from(format!("clear-{id}")), "Clear")
                                .ghost()
                                .h(px(36.))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.shortcuts[id]
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                })),
                        )
                        .child(
                            button(SharedString::from(format!("reset-{id}")), "Reset")
                                .ghost()
                                .h(px(36.))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.shortcuts[id].update(cx, |input, cx| {
                                        input.set_value(default, window, cx)
                                    });
                                })),
                        );
                }
                row = row
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(label(description))
                            .child(controls),
                    )
                    .child(ui::caption(
                        format!(
                            "Active: {}",
                            format_shortcut(
                                self.view
                                    .shortcuts
                                    .bindings
                                    .get(id)
                                    .map_or("", String::as_str)
                            )
                        ),
                        cx,
                    ));
                block = block.child(row);
            }
        }
        block
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("save-shortcuts", "Apply shortcuts").on_click(cx.listener(
                            |this, _, _, cx| {
                                let bindings = this
                                    .shortcuts
                                    .iter()
                                    .map(|(id, input)| {
                                        (id.clone(), input.read(cx).value().to_string())
                                    })
                                    .collect();
                                this.owner
                                    .update(cx, |app, cx| app.request_shortcuts(bindings, cx));
                            },
                        )),
                    )
                    .child(
                        button("retry-shortcuts", "Retry registration").on_click(cx.listener(
                            |this, _, _, cx| {
                                let bindings = this.view.preferences.shortcuts.clone();
                                this.owner
                                    .update(cx, |app, cx| app.request_shortcuts(bindings, cx));
                            },
                        )),
                    ),
            )
            .child(label("Toggle stream telemetry (in the main window)"))
            .child(Input::new(&self.telemetry))
            .child(
                button("save-telemetry-key", "Apply telemetry shortcut").on_click(cx.listener(
                    |this, _, _, cx| {
                        let chord = this.telemetry.read(cx).value().to_string();
                        this.save(cx, |p| p.save_telemetry_shortcut(Some(chord)));
                    },
                )),
            )
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(cx.theme().font_size * (8. / 7.));
        let body = match self.tab {
            0 => self.general(cx),
            1 => self.appearance(cx),
            2 => self.stream(cx),
            _ => self.keyboard(cx),
        };
        div()
            .id("settings")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .on_key_down(|event, window, _| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            })
            .bg(cx.theme().background)
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
            .child(
                ui::title_bar(Some("Settings"), false, self.title_drag.clone(), cx)
                    .child(div().flex_1())
                    .child(ui::window_buttons(
                        cx,
                        window.is_maximized(),
                        |_, window, _| window.remove_window(),
                    )),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .px_6()
                    .pt_5()
                    .child(ui::body("Changes take effect without restarting", cx))
                    .child(
                        div().mt_4().flex().flex_wrap().gap_1().children(
                            [
                                ("General", IconName::Settings2),
                                ("Appearance", IconName::Palette),
                                ("Stream", IconName::Video),
                                ("Shortcuts", IconName::Keyboard),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(tab, (title, glyph))| {
                                button(SharedString::from(format!("tab-{tab}")), title)
                                    .ghost()
                                    .icon(glyph)
                                    .rounded_b_none()
                                    .bg(if self.tab == tab {
                                        cx.theme().secondary
                                    } else {
                                        transparent_black()
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.tab = tab;
                                        cx.notify();
                                    }))
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .id(("settings-body", self.tab))
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_6()
                    .child(body),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .min_h(px(48.))
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .px_6()
                    .py_3()
                    .children(
                        self.error.clone().map(|error| {
                            div().text_sm().text_color(cx.theme().danger).child(error)
                        }),
                    ),
            )
    }
}

fn label(value: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .font_medium()
        .child(value.into())
        .into_any_element()
}
fn section(title: &'static str, _: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(div().font_semibold().child(title))
}

/// CSS palettes from the final Tauri renderer, in sRGB (including its OKLCH colors).
fn palette(theme: ColorTheme, dark: bool) -> [u32; 8] {
    match (theme, dark) {
        (ColorTheme::Slopcast, true) => [
            0x090d16, 0xf3f4f6, 0x111827, 0xc4804a, 0x1f2937, 0x374151, 0xe11d48, 0x1f2937,
        ],
        (ColorTheme::Slopcast, false) => [
            0xf6f4f0, 0x20242d, 0xffffff, 0x875728, 0xebe7e0, 0xded8ce, 0xb82945, 0xd7d1c8,
        ],
        (ColorTheme::Ayu, true) => [
            0x0d1017, 0xbfbdb6, 0x141821, 0xe6b450, 0x1b1f29, 0x273244, 0xf07178, 0x303b4e,
        ],
        (ColorTheme::Ayu, false) => [
            0xf8f9fa, 0x5c6166, 0xfcfcfc, 0x9d620f, 0xebeef0, 0xd9dfe5, 0xad3232, 0xced4db,
        ],
        (ColorTheme::RosePine, true) => [
            0x191724, 0xe0def4, 0x1f1d2e, 0xc4a7e7, 0x26233a, 0x403d52, 0xeb6f92, 0x524f67,
        ],
        (ColorTheme::RosePine, false) => [
            0xfaf4ed, 0x464261, 0xfffaf3, 0x286983, 0xf2e9e1, 0xdfdad9, 0xa34e65, 0xcecacd,
        ],
        (ColorTheme::Kanagawa, true) => [
            0x1f1f28, 0xdcd7ba, 0x2a2a37, 0x957fb8, 0x363646, 0x54546d, 0xff5d62, 0x54546d,
        ],
        (ColorTheme::Kanagawa, false) => [
            0xf2ecbc, 0x545464, 0xe7dba0, 0x624c83, 0xe5ddb0, 0xdcd5ac, 0xb52d40, 0xb6ae89,
        ],
        (ColorTheme::Dracula, true) => [
            0x282a36, 0xf8f8f2, 0x343746, 0xbd93f9, 0x44475a, 0x53576f, 0xff5555, 0x6272a4,
        ],
        (ColorTheme::Dracula, false) => [
            0xfffbeb, 0x1f1f1f, 0xefeddc, 0x644ac9, 0xece9df, 0xcfcfde, 0xcb3a2a, 0xbcbab3,
        ],
    }
}

struct SystemFont(SharedString);
impl Global for SystemFont {}

pub fn apply_appearance(preferences: &DesktopPreferences, cx: &mut App) {
    if !cx.has_global::<SystemFont>() {
        cx.set_global(SystemFont(cx.theme().font_family.clone()));
    }
    let system_font = cx.global::<SystemFont>().0.clone();
    let mode = match preferences.appearance_mode {
        AppearanceMode::System => cx.window_appearance().into(),
        AppearanceMode::Light => ThemeMode::Light,
        AppearanceMode::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        let dark = mode == ThemeMode::Dark;
        let [
            background,
            foreground,
            card,
            primary,
            secondary,
            accent,
            danger,
            border,
        ] = palette(preferences.color_theme, dark).map(|v| Hsla::from(rgb(v)));
        theme.background = background;
        theme.foreground = foreground;
        theme.sidebar = card;
        theme.sidebar_foreground = foreground;
        theme.popover = card;
        theme.popover_foreground = foreground;
        theme.secondary = secondary;
        theme.secondary_foreground = foreground;
        theme.accent = accent;
        theme.accent_foreground = foreground;
        theme.border = border;
        theme.input = accent;
        theme.primary = primary;
        theme.primary_hover = crate::ui::mix(primary, foreground, 0.85);
        theme.primary_active = crate::ui::mix(primary, background, 0.8);
        theme.primary_foreground = background;
        theme.ring = primary;
        theme.caret = primary;
        theme.muted = secondary;
        theme.muted_foreground = if preferences.color_theme == ColorTheme::Slopcast && dark {
            rgb(0x9ca3af).into()
        } else {
            crate::ui::mix(foreground, background, 0.75)
        };
        theme.danger = danger;
        theme.danger_foreground = if preferences.color_theme == ColorTheme::Slopcast && dark {
            white()
        } else {
            background
        };
        theme.danger_hover = crate::ui::mix(danger, background, 0.9);
        theme.danger_active = crate::ui::mix(danger, background, 0.8);
        theme.button = background;
        theme.button_foreground = foreground;
        theme.button_hover = accent;
        theme.button_active = secondary;
        theme.button_primary = primary;
        theme.button_primary_foreground = background;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.button_secondary = secondary;
        theme.button_secondary_foreground = foreground;
        theme.button_secondary_hover = crate::ui::mix(secondary, background, 0.8);
        theme.button_secondary_active = accent;
        theme.button_danger = danger;
        theme.button_danger_foreground = theme.danger_foreground;
        theme.button_danger_hover = theme.danger_hover;
        theme.button_danger_active = theme.danger_active;

        theme.list_hover = accent;
        theme.list_active = secondary;
        theme.selection = primary.opacity(0.25);
        theme.radius = px(6.);
        theme.font_family = match preferences.typography.font_family {
            FontFamily::System => system_font.clone(),
            FontFamily::Theme => match preferences.color_theme {
                // The former renderer's `system-ui` resolved to an Inter-metric UI face. GPUI's
                // `.SystemUIFont` falls back to DejaVu Sans, so name the bundled face instead.
                ColorTheme::Slopcast | ColorTheme::Ayu | ColorTheme::Dracula => "Inter".into(),
                ColorTheme::RosePine | ColorTheme::Kanagawa => "Lora".into(),
            },
            FontFamily::Lora => "Lora".into(),
            FontFamily::JetbrainsMono => "JetBrains Mono".into(),
            FontFamily::Inter => "Inter".into(),
        };
        theme.font_size = px(f32::from(preferences.typography.font_size.unwrap_or(14)));
    });
}

fn preferences_font_key(font: FontFamily) -> &'static str {
    match font {
        FontFamily::Theme => "theme",
        FontFamily::System => "system",
        FontFamily::Inter => "inter",
        FontFamily::Lora => "lora",
        FontFamily::JetbrainsMono => "jetbrains-mono",
    }
}

fn format_shortcut(chord: &str) -> String {
    if chord.is_empty() {
        return "Not set".into();
    }
    chord
        .replace("Control", "Ctrl")
        .replace("Command", "Cmd")
        .replace("Key", "")
        .replace("Digit", "")
        .replace('+', " + ")
}
