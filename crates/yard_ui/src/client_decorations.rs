//! Adapts Zed's `workspace` crate at revision
//! ddb90e5e89d346f21847281727e1644b8bb2cc37, licensed GPL-3.0-or-later.
//! Copyright Zed Industries, Inc.

use crate::{Platform, WindowControlAction};
use gpui::{
    App, Bounds, Corners, CursorStyle, Decorations, Div, Global, HitboxBehavior, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, ResizeEdge,
    Stateful, Styled, Tiling, Window, WindowBackgroundAppearance, canvas, div, point,
    prelude::FluentBuilder, px, size, transparent_black,
};
use theme::{
    ActiveTheme, CLIENT_SIDE_DECORATION_BORDER, CLIENT_SIDE_DECORATION_ROUNDING,
    CLIENT_SIDE_DECORATION_SHADOW, ClientDecorationsExt,
};

struct ShownResizeEdge(ResizeEdge);

impl Global for ShownResizeEdge {}

/// The background a window asks of the platform. On Linux it is never
/// opaque, so that the shadow and the rounded corners YARD draws around the
/// window show what is behind it: GPUI does the same for every
/// client-decorated window on Wayland, and Zed asks it for its own
/// client-decorated windows. Elsewhere it stays opaque.
pub fn window_background(cx: &App) -> WindowBackgroundAppearance {
    match cx.theme().window_background_appearance() {
        WindowBackgroundAppearance::Opaque if Platform::CURRENT == Platform::Linux => {
            WindowBackgroundAppearance::Transparent
        }
        appearance if Platform::CURRENT == Platform::Linux => appearance,
        _ => WindowBackgroundAppearance::Opaque,
    }
}

/// Wraps a window's root. With client decorations it draws the window's
/// frame, as Zed does: a shadow and rounded corners on the edges that are not
/// tiled, a border, and resizing from the edges and corners, with their
/// cursors. With server decorations it only paints the background.
pub fn client_decorations(
    element: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let decorations = window.window_decorations();
    let is_resizable = window.is_resizable();
    let tiling = match decorations {
        Decorations::Server => Tiling::default(),
        Decorations::Client { tiling } => tiling,
    };
    tracing::trace!(?decorations, "rendering client decorations");

    match decorations {
        Decorations::Client { .. } => {
            window.set_client_inset(CLIENT_SIDE_DECORATION_SHADOW);
            let radius = |rounded| {
                if rounded {
                    CLIENT_SIDE_DECORATION_ROUNDING
                } else {
                    px(0.0)
                }
            };
            window.set_client_corner_radii(Corners {
                top_left: radius(!tiling.top && !tiling.left),
                top_right: radius(!tiling.top && !tiling.right),
                bottom_left: radius(!tiling.bottom && !tiling.left),
                bottom_right: radius(!tiling.bottom && !tiling.right),
            });
        }
        Decorations::Server => {
            window.set_client_inset(px(0.0));
            window.set_client_corner_radii(Corners::default());
        }
    }

    div()
        .id("window-backdrop")
        .bg(transparent_black())
        .map(|div| match decorations {
            Decorations::Server => div,
            Decorations::Client { .. } => div
                .rounded_client_corners(tiling)
                .when(!tiling.top, |div| div.pt(CLIENT_SIDE_DECORATION_SHADOW))
                .when(!tiling.bottom, |div| div.pb(CLIENT_SIDE_DECORATION_SHADOW))
                .when(!tiling.left, |div| div.pl(CLIENT_SIDE_DECORATION_SHADOW))
                .when(!tiling.right, |div| div.pr(CLIENT_SIDE_DECORATION_SHADOW))
                .when(is_resizable, |div| {
                    div.on_mouse_move(move |event, window, cx| {
                        let size = window.window_bounds().get_bounds().size;
                        let edge = resize_edge(
                            event.position,
                            CLIENT_SIDE_DECORATION_SHADOW,
                            size,
                            tiling,
                        );
                        let shown = cx.try_global::<ShownResizeEdge>().map(|shown| shown.0);
                        if edge != shown {
                            window.refresh();
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        move |event, window, _| {
                            let size = window.window_bounds().get_bounds().size;
                            if let Some(edge) = resize_edge(
                                event.position,
                                CLIENT_SIDE_DECORATION_SHADOW,
                                size,
                                tiling,
                            ) {
                                WindowControlAction::Resize(edge).perform(window);
                            }
                        },
                    )
                }),
        })
        .size_full()
        .child(
            div()
                .cursor(CursorStyle::Arrow)
                .bg(cx.theme().colors().background)
                .map(|div| match decorations {
                    Decorations::Server => div,
                    Decorations::Client { .. } => div
                        .border_color(cx.theme().colors().border)
                        .rounded_client_corners(tiling)
                        .when(!tiling.top, |div| {
                            div.border_t(CLIENT_SIDE_DECORATION_BORDER)
                        })
                        .when(!tiling.bottom, |div| {
                            div.border_b(CLIENT_SIDE_DECORATION_BORDER)
                        })
                        .when(!tiling.left, |div| {
                            div.border_l(CLIENT_SIDE_DECORATION_BORDER)
                        })
                        .when(!tiling.right, |div| {
                            div.border_r(CLIENT_SIDE_DECORATION_BORDER)
                        })
                        .when(!tiling.is_tiled(), |div| {
                            div.shadow(vec![
                                gpui::BoxShadow::new(
                                    px(0.),
                                    px(0.),
                                    Hsla {
                                        h: 0.,
                                        s: 0.,
                                        l: 0.,
                                        a: 0.4,
                                    },
                                )
                                .blur_radius(CLIENT_SIDE_DECORATION_SHADOW / 2.),
                            ])
                        }),
                })
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .size_full()
                .child(element),
        )
        .map(|div| match decorations {
            Decorations::Client { tiling } if is_resizable => div.child(
                canvas(
                    |_, window, _| {
                        window.insert_hitbox(
                            Bounds::new(
                                point(px(0.0), px(0.0)),
                                window.window_bounds().get_bounds().size,
                            ),
                            HitboxBehavior::Normal,
                        )
                    },
                    move |_, hitbox, window, cx| {
                        let mouse = window.mouse_position();
                        let size = window.window_bounds().get_bounds().size;
                        let Some(edge) =
                            resize_edge(mouse, CLIENT_SIDE_DECORATION_SHADOW, size, tiling)
                        else {
                            return;
                        };
                        tracing::trace!(?edge, "resize cursor");
                        cx.set_global(ShownResizeEdge(edge));
                        window.set_cursor_style(
                            match edge {
                                ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                                ResizeEdge::Left | ResizeEdge::Right => {
                                    CursorStyle::ResizeLeftRight
                                }
                                ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                    CursorStyle::ResizeUpLeftDownRight
                                }
                                ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                    CursorStyle::ResizeUpRightDownLeft
                                }
                            },
                            &hitbox,
                        );
                    },
                )
                .size_full()
                .absolute(),
            ),
            _ => div,
        })
}

/// The edge or corner a point at `pos` resizes, in a window of `window_size`
/// whose shadow is `shadow_size` wide. Corners reach one and a half shadows
/// in; tiled edges do not resize.
pub(crate) fn resize_edge(
    pos: Point<Pixels>,
    shadow_size: Pixels,
    window_size: gpui::Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let bounds = Bounds::new(Point::default(), window_size).inset(shadow_size * 1.5);
    if bounds.contains(&pos) {
        return None;
    }

    let corner_size = size(shadow_size * 1.5, shadow_size * 1.5);
    let top_left_bounds = Bounds::new(Point::new(px(0.), px(0.)), corner_size);
    if !tiling.top && top_left_bounds.contains(&pos) {
        return Some(ResizeEdge::TopLeft);
    }

    let top_right_bounds = Bounds::new(
        Point::new(window_size.width - corner_size.width, px(0.)),
        corner_size,
    );
    if !tiling.top && top_right_bounds.contains(&pos) {
        return Some(ResizeEdge::TopRight);
    }

    let bottom_left_bounds = Bounds::new(
        Point::new(px(0.), window_size.height - corner_size.height),
        corner_size,
    );
    if !tiling.bottom && bottom_left_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomLeft);
    }

    let bottom_right_bounds = Bounds::new(
        Point::new(
            window_size.width - corner_size.width,
            window_size.height - corner_size.height,
        ),
        corner_size,
    );
    if !tiling.bottom && bottom_right_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && pos.y < shadow_size {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - shadow_size {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < shadow_size {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - shadow_size {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
