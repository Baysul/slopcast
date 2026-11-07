//! Shared native rendering of the former desktop CSS tokens and window chrome.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt};
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

pub fn icon(name: IconName, size: f32, color: impl Into<Hsla>) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    let label = label.into();
    let empty = label.is_empty();
    let button = Button::new(id);
    let button = if empty {
        button.px_0()
    } else {
        button.label(label).px_4()
    };
    button
        .h(px(40.))
        .text_sm()
        .font_medium()
        .rounded(px(6.))
        .cursor_pointer()
}

pub fn caption(value: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .line_height(relative(1.625))
        .text_color(cx.theme().muted_foreground)
        .child(value.into())
        .into_any_element()
}

pub fn body(value: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .line_height(relative(1.625))
        .text_color(cx.theme().muted_foreground)
        .child(value.into())
        .into_any_element()
}

pub fn section_heading(glyph: IconName, title: &'static str, cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .child(
            div()
                .size(px(28.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded_md()
                .bg(cx.theme().primary.opacity(0.15))
                .child(icon(glyph, 16., cx.theme().primary)),
        )
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
}

pub fn mix(a: Hsla, b: Hsla, weight: f32) -> Hsla {
    let a = a.to_rgb();
    let b = b.to_rgb();
    Rgba {
        r: a.r * weight + b.r * (1. - weight),
        g: a.g * weight + b.g * (1. - weight),
        b: a.b * weight + b.b * (1. - weight),
        a: 1.,
    }
    .into()
}

pub type TitleDrag = Rc<Cell<Option<Point<Pixels>>>>;

pub fn title_bar(
    title: Option<&'static str>,
    live: bool,
    pressed: TitleDrag,
    cx: &App,
) -> Stateful<Div> {
    let moving = pressed.clone();
    let background = cx.theme().background;
    div()
        .id("titlebar")
        .relative()
        .flex()
        .items_center()
        .flex_shrink_0()
        .h(px(40.))
        .w_full()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(if live {
            mix(cx.theme().primary, background, 0.21)
        } else {
            background
        })
        .on_mouse_down(MouseButton::Left, move |event, window, _| {
            if event.click_count == 2 {
                window.zoom_window();
                pressed.set(None);
            } else {
                pressed.set(Some(event.position));
            }
        })
        .on_mouse_move(move |event, window, _| {
            if event.pressed_button != Some(MouseButton::Left) {
                moving.set(None);
                return;
            }
            if let Some(start) = moving.get()
                && (event.position.x - start.x).abs() + (event.position.y - start.y).abs() >= px(4.)
            {
                moving.set(None);
                window.start_window_move();
            }
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .pl_4()
                .pr_3()
                .child(img(app_icon()).size(px(16.)))
                .child(
                    div()
                        .font_family(cx.theme().font_family.clone())
                        .text_sm()
                        .font_semibold()
                        .child("Slopcast"),
                ),
        )
        .children(title.map(|title| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(div().text_sm().font_semibold().child(title))
        }))
}

pub fn window_buttons(
    cx: &App,
    maximized: bool,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let gnome = cfg!(target_os = "linux") && !desktop.contains("kde");
    let linux = cfg!(target_os = "linux");
    let control = |id: &'static str, glyph: IconName, tooltip: &'static str| {
        use gpui_kit::component::button::ButtonVariants;
        button(id, "")
            .ghost()
            .icon(glyph)
            .tooltip(tooltip)
            .w(px(if linux { 24. } else { 44. }))
            .h(px(if linux { 24. } else { 40. }))
            .rounded(px(if linux { 12. } else { 0. }))
            .px_0()
    };
    div()
        .ml_auto()
        .flex()
        .items_center()
        .gap(px(if gnome {
            8.
        } else if linux {
            4.
        } else {
            0.
        }))
        .pr(px(if gnome {
            12.
        } else if linux {
            10.
        } else {
            0.
        }))
        .child(
            control("minimize", IconName::WindowMinimize, "Minimize")
                .bg(if gnome {
                    cx.theme().foreground.opacity(0.1)
                } else {
                    transparent_black()
                })
                .on_click(|_, window, _| window.minimize_window()),
        )
        .child(
            control(
                "maximize",
                if maximized {
                    IconName::WindowRestore
                } else {
                    IconName::WindowMaximize
                },
                if maximized { "Restore" } else { "Maximize" },
            )
            .bg(if gnome {
                cx.theme().foreground.opacity(0.1)
            } else {
                transparent_black()
            })
            .on_click(|_, window, _| window.zoom_window()),
        )
        .child(
            control("quit", IconName::WindowClose, "Close")
                .bg(if gnome {
                    cx.theme().foreground.opacity(0.1)
                } else {
                    transparent_black()
                })
                .on_click(close),
        )
}

fn app_icon() -> Arc<RenderImage> {
    static ICON: std::sync::OnceLock<Arc<RenderImage>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let mut pixels = image::load_from_memory(include_bytes!("../resources/icons/128x128.png"))
            .expect("bundled icon")
            .to_rgba8();
        for pixel in pixels.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]))
    })
    .clone()
}

/// Native min/max envelope of the same 48 columns used by the React audio visualizer.
pub fn waveform(columns: Vec<f64>, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for (index, pair) in columns.as_chunks::<2>().0.iter().enumerate() {
                let low = pair[0].clamp(-1., 1.) as f32;
                let high = pair[1].clamp(-1., 1.) as f32;
                let x = bounds.left() + px(index as f32 * 2.);
                let y = bounds.top() + px((1. - high) * 10.);
                window.paint_quad(fill(
                    Bounds::new(point(x, y), size(px(1.), px(((high - low) * 10.).max(1.)))),
                    color,
                ));
            }
        },
    )
    .w(px(96.))
    .h(px(20.))
    .flex_shrink_0()
}

pub fn sparkline(values: Vec<f64>, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if values.len() < 2 {
                return;
            }
            let max = values.iter().copied().fold(0.001, f64::max);
            let mut path = PathBuilder::stroke(px(1.5));
            for (index, value) in values.iter().enumerate() {
                let p = point(
                    bounds.left() + bounds.size.width * (index as f32 / (values.len() - 1) as f32),
                    bounds.bottom()
                        - px(1.5)
                        - (bounds.size.height - px(3.)) * (value / max) as f32,
                );
                if index == 0 {
                    path.move_to(p);
                } else {
                    path.line_to(p);
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .w(px(88.))
    .h(px(22.))
}
