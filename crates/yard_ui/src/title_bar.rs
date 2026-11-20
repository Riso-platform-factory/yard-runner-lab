//! Adapts Zed's `platform_title_bar`, `title_bar`, and `ui` crates at revision
//! ddb90e5e89d346f21847281727e1644b8bb2cc37, licensed GPL-3.0-or-later.
//! Copyright Zed Industries, Inc.

use crate::{DrawnSides, Platform, WindowButtons, WindowControlAction, drawn_sides, h_flex};
use gpui::{
    AnyElement, App, Decorations, ElementId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, TitlebarOptions,
    Window, WindowControlArea, point, prelude::FluentBuilder, px,
};
use std::sync::Arc;
use theme::{ActiveTheme, CLIENT_SIDE_DECORATION_BORDER, CLIENT_SIDE_DECORATION_ROUNDING};

/// Window title-bar options for a custom-drawn transparent bar.
pub fn titlebar_options(title: impl Into<SharedString>) -> TitlebarOptions {
    TitlebarOptions {
        title: Some(title.into()),
        appears_transparent: true,
        traffic_light_position: Some(point(px(9.), px(9.))),
    }
}

/// A window title bar with `left`, `center`, and `right` item groups.
#[derive(IntoElement)]
pub struct TitleBar {
    id: ElementId,
    left: Vec<AnyElement>,
    center: Vec<AnyElement>,
    right: Vec<AnyElement>,
}

impl TitleBar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            left: Vec::new(),
            center: Vec::new(),
            right: Vec::new(),
        }
    }

    pub fn left(mut self, child: impl IntoElement) -> Self {
        self.left.push(child.into_any_element());
        self
    }

    pub fn center(mut self, child: impl IntoElement) -> Self {
        self.center.push(child.into_any_element());
        self
    }

    pub fn right(mut self, child: impl IntoElement) -> Self {
        self.right.push(child.into_any_element());
        self
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let height = ui::utils::platform_title_bar_height(window);
        let fullscreen = window.is_fullscreen() || window.is_simple_fullscreen();
        let mac_traffic_lights = cfg!(target_os = "macos") && !fullscreen;
        let linux = Platform::CURRENT == Platform::Linux;
        let decorations = window.window_decorations();
        let supported = window.window_controls();
        let color = cx.theme().colors().title_bar_background;
        let DrawnSides { left, right } = if fullscreen {
            DrawnSides::default()
        } else {
            drawn_sides(Platform::CURRENT, decorations, cx.button_layout())
        };
        // On Linux the window manager moves the window once the pointer moves
        // with the button down, so that a click and a double-click still
        // reach the bar.
        let should_move = linux.then(|| {
            window.use_keyed_state(
                ElementId::NamedChild(Arc::new(self.id.clone()), "should-move".into()),
                cx,
                |_, _| false,
            )
        });

        h_flex()
            .id(self.id)
            .window_control_area(WindowControlArea::Drag)
            .w_full()
            .flex_none()
            .h(height)
            .bg(color)
            .when(cfg!(target_os = "macos"), |bar| {
                bar.on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.titlebar_double_click();
                    }
                })
            })
            .when_some(should_move, |bar, should_move| {
                let up = should_move.clone();
                let down = should_move.clone();
                let out = should_move.clone();
                bar.on_mouse_down_out(move |_, _, cx| out.update(cx, |state, _| *state = false))
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                        up.update(cx, |state, _| *state = false)
                    })
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        tracing::trace!("title bar pressed, a move starts on the next mouse move");
                        down.update(cx, |state, _| *state = true)
                    })
                    .on_mouse_move(move |_, window, cx| {
                        if *should_move.read(cx) {
                            should_move.update(cx, |state, _| *state = false);
                            WindowControlAction::Move.perform(window);
                        }
                    })
            })
            .when(
                linux && supported.maximize && window.is_resizable(),
                |bar| {
                    bar.on_click(|event, window, _| {
                        if event.click_count() == 2 {
                            if window.is_maximized() {
                                WindowControlAction::Restore.perform(window);
                            } else {
                                WindowControlAction::Maximize.perform(window);
                            }
                        }
                    })
                },
            )
            .when(
                linux
                    && !fullscreen
                    && matches!(decorations, Decorations::Client { .. })
                    && supported.window_menu,
                |bar| {
                    bar.on_mouse_down(MouseButton::Right, |event, window, _| {
                        WindowControlAction::Menu(event.position).perform(window);
                    })
                },
            )
            .map(|bar| match decorations {
                Decorations::Server => bar,
                Decorations::Client { tiling } => bar
                    .when(!(tiling.top || tiling.right), |bar| {
                        bar.rounded_tr(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    .when(!(tiling.top || tiling.left), |bar| {
                        bar.rounded_tl(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    // The bar covers the frame's border, so that the rounded
                    // corners leave no gap.
                    .mt(-CLIENT_SIDE_DECORATION_BORDER)
                    .mb(-CLIENT_SIDE_DECORATION_BORDER)
                    .border(CLIENT_SIDE_DECORATION_BORDER)
                    .border_color(color),
            })
            .when_some(left, |bar, buttons| {
                bar.child(WindowButtons::new("left-window-controls", buttons))
            })
            .child(
                h_flex().h_full().flex_1().child(
                    h_flex()
                        .h_full()
                        .map(|bar| {
                            if mac_traffic_lights {
                                bar.pl(px(ui::utils::TRAFFIC_LIGHT_PADDING))
                            } else if left.is_some() {
                                bar
                            } else {
                                bar.pl_2()
                            }
                        })
                        .when(!self.left.is_empty(), |bar| {
                            bar.child(
                                h_flex()
                                    .h_full()
                                    .gap_1()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .children(self.left),
                            )
                        }),
                ),
            )
            .when(!self.center.is_empty(), |bar| {
                bar.child(
                    h_flex()
                        .h_full()
                        .gap_1()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .children(self.center),
                )
            })
            .child(
                h_flex().h_full().flex_1().justify_end().child(
                    h_flex()
                        .h_full()
                        .when(right.is_none(), |bar| bar.pr_2())
                        .when(!self.right.is_empty(), |bar| {
                            bar.child(
                                h_flex()
                                    .h_full()
                                    .gap_1()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .children(self.right),
                            )
                        }),
                ),
            )
            .when_some(right, |bar, buttons| {
                bar.child(WindowButtons::new("right-window-controls", buttons))
            })
    }
}
