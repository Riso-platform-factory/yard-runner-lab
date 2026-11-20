//! Adapts Zed's `workspace` crate at revision
//! ddb90e5e89d346f21847281727e1644b8bb2cc37, licensed GPL-3.0-or-later.
//! Copyright Zed Industries, Inc.

use crate::h_flex;
use gpui::{
    AnyElement, App, Decorations, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, Styled, Window, prelude::FluentBuilder, px,
};
use theme::{ActiveTheme, CLIENT_SIDE_DECORATION_ROUNDING};
use ui::DynamicSpacing;

/// A status bar with ordered left and right item groups.
#[derive(IntoElement)]
pub struct StatusBar {
    id: ElementId,
    left: Vec<AnyElement>,
    right: Vec<AnyElement>,
}

impl StatusBar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    pub fn left(mut self, child: impl IntoElement) -> Self {
        self.left.push(child.into_any_element());
        self
    }

    pub fn right(mut self, child: impl IntoElement) -> Self {
        self.right.push(child.into_any_element());
        self
    }
}

impl RenderOnce for StatusBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let background = cx.theme().colors().status_bar_background;
        h_flex()
            .id(self.id)
            .w_full()
            .flex_none()
            .justify_between()
            .gap(DynamicSpacing::Base08.rems(cx))
            .p(DynamicSpacing::Base04.rems(cx))
            .bg(background)
            .map(|bar| match window.window_decorations() {
                Decorations::Server => bar,
                Decorations::Client { tiling } => bar
                    .when(!(tiling.bottom || tiling.right), |bar| {
                        bar.rounded_br(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    .when(!(tiling.bottom || tiling.left), |bar| {
                        bar.rounded_bl(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    // The bar covers the frame's border, so that the rounded
                    // corners leave no gap.
                    .mb(px(-1.))
                    .border_b(px(1.))
                    .border_color(background),
            })
            .child(
                h_flex()
                    .gap_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .children(self.left),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .overflow_x_hidden()
                    .children(self.right),
            )
    }
}
