//! Native settings window observing the same presenter as the main window.

use crate::app::PresenterApp;
use desktop_core::preferences::{
    ACTIONS, AppearanceMode, ColorTheme, DesktopPreferences, FontFamily,
};
use desktop_core::presenter::{Command, Snapshot};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
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
            ..Default::default()
        }),
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

struct SettingsView {
    owner: Entity<PresenterApp>,
    view: Snapshot,
    endpoint: Entity<InputState>,
    bitrate: Entity<InputState>,
    directory: Entity<InputState>,
    telemetry: Entity<InputState>,
    shortcuts: BTreeMap<String, Entity<InputState>>,
    error: Option<String>,
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
        let mut section = section("Stream", cx)
            .child(label("API endpoint"))
            .child(Input::new(&self.endpoint))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("validate-endpoint")
                            .label("Check and apply")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let endpoint = this.endpoint.read(cx).value().to_string();
                                this.dispatch(Command::ValidateEndpointDraft(endpoint), cx);
                            })),
                    )
                    .child(
                        Button::new("reset-endpoint")
                            .ghost()
                            .label("Use default")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dispatch(Command::EndpointReset, cx)
                            })),
                    ),
            )
            .children(
                self.view
                    .endpoint_draft
                    .as_ref()
                    .and_then(|draft| draft.message.clone())
                    .or(self.view.endpoint.message.clone())
                    .map(label),
            );
        if let Some(candidate) = &self.view.endpoint_switch_candidate {
            section = section
                .child(label(format!(
                    "Switching to {candidate} replaces the current room and its link."
                )))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("switch-endpoint")
                                .danger()
                                .label("Replace room and switch")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dispatch(Command::ConfirmEndpointSwitch, cx)
                                })),
                        )
                        .child(
                            Button::new("defer-endpoint")
                                .label("Use after this room closes")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dispatch(Command::DeferEndpointSwitch, cx)
                                })),
                        ),
                );
        }
        section = section.child(label("Resolution"));
        let resolutions = ["720p", "1080p", "1440p", "2160p"];
        section = section
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(resolutions.into_iter().map(|resolution| {
                        Button::new(SharedString::from(format!("resolution-{resolution}")))
                            .small()
                            .label(resolution)
                            .toggled(self.view.settings.resolution == resolution)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stream_change("resolution", resolution.into(), cx)
                            }))
                    })),
            )
            .child(label("Frame rate"))
            .child(div().flex().flex_wrap().gap_2().children(
                [24, 30, 60, 90, 120, 144, 165, 240].into_iter().map(|fps| {
                    Button::new(SharedString::from(format!("fps-{fps}")))
                        .small()
                        .label(fps.to_string())
                        .toggled(self.view.settings.fps == f64::from(fps))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stream_change("fps", fps.into(), cx)
                        }))
                }),
            ))
            .child(label("Video codec"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(self.view.codecs.iter().map(|codec| {
                        let id = codec.codec.clone();
                        Button::new(SharedString::from(format!("codec-{id}")))
                            .small()
                            .label(codec.label.clone())
                            .toggled(self.view.settings.video_codec == id)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stream_change("videoCodec", id.clone().into(), cx)
                            }))
                    })),
            )
            .child(
                Checkbox::new("auto-bitrate")
                    .label("Adapt bitrate to the connection")
                    .checked(self.view.settings.auto_bitrate)
                    .on_click(cx.listener(|this, value: &bool, _, cx| {
                        this.stream_change("autoBitrate", (*value).into(), cx)
                    })),
            )
            .child(label("Bitrate limit (Mbps)"))
            .child(
                div().flex().gap_2().child(Input::new(&self.bitrate)).child(
                    Button::new("save-bitrate")
                        .label("Apply")
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.bitrate.read(cx).value().parse::<f64>() {
                                Ok(mbps) if mbps.is_finite() && (1.0..=300.0).contains(&mbps) => {
                                    this.error = None;
                                    this.stream_change(
                                        "bitrateLimit",
                                        (mbps * 1_000_000.).into(),
                                        cx,
                                    );
                                }
                                _ => {
                                    this.error =
                                        Some("Enter a bitrate between 1 and 300 Mbps.".into());
                                    cx.notify();
                                }
                            }
                        })),
                ),
            );
        section.into_any_element()
    }

    fn general(&self, cx: &mut Context<Self>) -> AnyElement {
        section("Desktop", cx)
            .child(
                Checkbox::new("tray-enabled")
                    .label("Keep Slopcast in the system tray when closing the main window")
                    .checked(self.view.preferences.tray_enabled)
                    .on_click(cx.listener(|this, value: &bool, _, cx| {
                        let auto = this.view.preferences.auto_select_audio;
                        this.save(cx, |p| p.save_general(*value, auto));
                    })),
            )
            .child(
                Checkbox::new("auto-audio")
                    .label("Automatically follow the shared application's audio")
                    .checked(self.view.preferences.auto_select_audio)
                    .on_click(cx.listener(|this, value: &bool, _, cx| {
                        let tray = this.view.preferences.tray_enabled;
                        this.save(cx, |p| p.save_general(tray, *value));
                    })),
            )
            .child(label("Settings directory"))
            .child(Input::new(&self.directory))
            .child(
                Button::new("choose-directory")
                    .label("Choose folder…")
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
            .child(
                Button::new("open-logs")
                    .label("Open log folder")
                    .on_click(|_, _, cx| {
                        if let Some(directory) = crate::logging::directory() {
                            cx.reveal_path(&directory);
                        }
                    }),
            )
            .child(
                Button::new("move-settings")
                    .label("Move settings here")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let directory =
                            std::path::PathBuf::from(this.directory.read(cx).value().to_string());
                        this.save(cx, |p| p.set_settings_directory(&directory));
                    })),
            )
            .children(self.view.preferences_error.clone().map(label))
            .into_any_element()
    }

    fn appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        section("Appearance", cx)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .children(AppearanceMode::ALL.into_iter().map(|mode| {
                        Button::new(SharedString::from(format!("mode-{mode:?}")))
                            .small()
                            .label(mode.label())
                            .toggled(self.view.preferences.appearance_mode == mode)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let theme = this.view.preferences.color_theme;
                                this.save(cx, |p| p.save_appearance(mode, theme));
                            }))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(ColorTheme::ALL.into_iter().map(|theme| {
                        Button::new(SharedString::from(format!("theme-{theme:?}")))
                            .small()
                            .label(theme.label())
                            .toggled(self.view.preferences.color_theme == theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mode = this.view.preferences.appearance_mode;
                                this.save(cx, |p| p.save_appearance(mode, theme));
                            }))
                    })),
            )
            .child(label("Font"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(FontFamily::ALL.into_iter().map(|font| {
                        Button::new(SharedString::from(format!("font-{font:?}")))
                            .small()
                            .label(font.label())
                            .toggled(self.view.preferences.typography.font_family == font)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut typography = this.view.preferences.typography.clone();
                                typography.font_family = font;
                                this.save(cx, |p| p.save_typography(typography));
                            }))
                    })),
            )
            .child(label("Text size"))
            .child(div().flex().gap_2().children((12..=18u8).map(|size| {
                Button::new(SharedString::from(format!("size-{size}")))
                    .small()
                    .label(size.to_string())
                    .toggled(self.view.preferences.typography.font_size.unwrap_or(14) == size)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut typography = this.view.preferences.typography.clone();
                        typography.font_size = Some(size);
                        this.save(cx, |p| p.save_typography(typography));
                    }))
            })))
            .child(label("Text weight"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .children([400u16, 500, 600].into_iter().map(|weight| {
                        Button::new(SharedString::from(format!("weight-{weight}")))
                            .small()
                            .label(weight.to_string())
                            .toggled(
                                self.view.preferences.typography.font_weight.unwrap_or(400)
                                    == weight,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut typography = this.view.preferences.typography.clone();
                                typography.font_weight = Some(weight);
                                this.save(cx, |p| p.save_typography(typography));
                            }))
                    })),
            )
            .child(
                Checkbox::new("ligatures")
                    .label("Font ligatures")
                    .checked(self.view.preferences.typography.ligatures)
                    .on_click(cx.listener(|this, value: &bool, _, cx| {
                        let mut typography = this.view.preferences.typography.clone();
                        typography.ligatures = *value;
                        this.save(cx, |p| p.save_typography(typography));
                    })),
            )
            .into_any_element()
    }

    fn keyboard(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut block =
            section("Keyboard shortcuts", cx).child(label(self.view.shortcuts.status.clone()));
        for (id, description, _) in ACTIONS {
            if let Some(input) = self.shortcuts.get(id) {
                block = block.child(label(description)).child(Input::new(input));
            }
        }
        block
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("save-shortcuts")
                            .label("Apply shortcuts")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let bindings = this
                                    .shortcuts
                                    .iter()
                                    .map(|(id, input)| {
                                        (id.clone(), input.read(cx).value().to_string())
                                    })
                                    .collect();
                                this.owner
                                    .update(cx, |app, cx| app.request_shortcuts(bindings, cx));
                            })),
                    )
                    .child(
                        Button::new("retry-shortcuts")
                            .label("Retry registration")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let bindings = this.view.preferences.shortcuts.clone();
                                this.owner
                                    .update(cx, |app, cx| app.request_shortcuts(bindings, cx));
                            })),
                    ),
            )
            .child(label("Toggle stream telemetry (in the main window)"))
            .child(Input::new(&self.telemetry))
            .child(
                Button::new("save-telemetry-key")
                    .label("Apply telemetry shortcut")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let chord = this.telemetry.read(cx).value().to_string();
                        this.save(cx, |p| p.save_telemetry_shortcut(Some(chord)));
                    })),
            )
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
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
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_color(cx.theme().danger).child(error)),
            )
            .child(self.stream(cx))
            .child(self.general(cx))
            .child(self.appearance(cx))
            .child(self.keyboard(cx))
    }
}

fn label(value: impl Into<SharedString>) -> AnyElement {
    div().text_sm().child(value.into()).into_any_element()
}
fn section(title: &'static str, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_lg()
        .child(div().font_semibold().child(title))
}

pub fn apply_appearance(preferences: &DesktopPreferences, cx: &mut App) {
    let mode = match preferences.appearance_mode {
        AppearanceMode::System => cx.window_appearance().into(),
        AppearanceMode::Light => ThemeMode::Light,
        AppearanceMode::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        let dark = mode == ThemeMode::Dark;
        let (hue, saturation) = match preferences.color_theme {
            ColorTheme::Slopcast => (0.615, 0.42),
            ColorTheme::Ayu => (0.61, 0.25),
            ColorTheme::RosePine => (0.69, 0.23),
            ColorTheme::Kanagawa => (0.64, 0.20),
            ColorTheme::Dracula => (0.67, 0.16),
        };
        theme.background = hsla(hue, saturation, if dark { 0.061 } else { 0.97 }, 1.);
        theme.foreground = hsla(hue, 0.10, if dark { 0.92 } else { 0.12 }, 1.);
        theme.primary = hsla(0.074, 0.51, if dark { 0.53 } else { 0.42 }, 1.);
        theme.primary_foreground = hsla(hue, 0.42, if dark { 0.06 } else { 0.98 }, 1.);
        theme.ring = theme.primary;
        if preferences.typography.font_family != FontFamily::System {
            theme.font_family = match preferences.typography.font_family {
                FontFamily::Lora => "Lora",
                FontFamily::JetbrainsMono => "JetBrains Mono",
                _ => "Inter",
            }
            .into();
        }
        theme.font_size = px(f32::from(preferences.typography.font_size.unwrap_or(14)));
    });
}
