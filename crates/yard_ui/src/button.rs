//! Wraps Zed's `ui` crate at revision
//! ddb90e5e89d346f21847281727e1644b8bb2cc37, licensed GPL-3.0-or-later.
//! Copyright Zed Industries, Inc.

use crate::{IconName, IconSize, TextColor};
use gpui::{AnyView, App, ElementId, IntoElement, Rems, RenderOnce, SharedString, Window, rems};
use ui::{ButtonCommon, Clickable, Disableable, SelectableButton, Toggleable};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonStyle {
    Filled,
    Tinted(TintColor),
    Outlined,
    #[default]
    Subtle,
    Transparent,
}

impl ButtonStyle {
    fn zed(self) -> ui::ButtonStyle {
        match self {
            Self::Filled => ui::ButtonStyle::Filled,
            Self::Tinted(tint) => ui::ButtonStyle::Tinted(tint.zed()),
            Self::Outlined => ui::ButtonStyle::Outlined,
            Self::Subtle => ui::ButtonStyle::Subtle,
            Self::Transparent => ui::ButtonStyle::Transparent,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TintColor {
    #[default]
    Accent,
    Error,
    Warning,
    Success,
}

impl TintColor {
    fn zed(self) -> ui::TintColor {
        match self {
            Self::Accent => ui::TintColor::Accent,
            Self::Error => ui::TintColor::Error,
            Self::Warning => ui::TintColor::Warning,
            Self::Success => ui::TintColor::Success,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    Large,
    Medium,
    #[default]
    Default,
    Compact,
    None,
}

impl ButtonSize {
    pub fn rems(self) -> Rems {
        match self {
            Self::Large => rems(2.),
            Self::Medium => rems(1.75),
            Self::Default => rems(1.375),
            Self::Compact => rems(1.125),
            Self::None => rems(1.),
        }
    }

    fn zed(self) -> ui::ButtonSize {
        match self {
            Self::Large => ui::ButtonSize::Large,
            Self::Medium => ui::ButtonSize::Medium,
            Self::Default => ui::ButtonSize::Default,
            Self::Compact => ui::ButtonSize::Compact,
            Self::None => ui::ButtonSize::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconButtonShape {
    Square,
    #[default]
    Wide,
}

impl IconButtonShape {
    fn zed(self) -> ui::IconButtonShape {
        match self {
            Self::Square => ui::IconButtonShape::Square,
            Self::Wide => ui::IconButtonShape::Wide,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElevationIndex {
    Background,
    Surface,
    ElevatedSurface,
    ModalSurface,
}

impl ElevationIndex {
    fn zed(self) -> ui::ElevationIndex {
        match self {
            Self::Background => ui::ElevationIndex::Background,
            Self::Surface => ui::ElevationIndex::Surface,
            Self::ElevatedSurface => ui::ElevationIndex::ElevatedSurface,
            Self::ModalSurface => ui::ElevationIndex::ModalSurface,
        }
    }
}

fn zed_icon_name(value: IconName) -> ui::IconName {
    match value {
        IconName::Check => ui::IconName::Check,
        IconName::ChevronDown => ui::IconName::ChevronDown,
        IconName::ChevronRight => ui::IconName::ChevronRight,
        IconName::Close => ui::IconName::Close,
        IconName::GenericClose => ui::IconName::GenericClose,
        IconName::GenericMaximize => ui::IconName::GenericMaximize,
        IconName::GenericMinimize => ui::IconName::GenericMinimize,
        IconName::GenericRestore => ui::IconName::GenericRestore,
        IconName::Info => ui::IconName::Info,
        IconName::Plus => ui::IconName::Plus,
        IconName::Warning => ui::IconName::Warning,
        IconName::XCircle => ui::IconName::XCircle,
    }
}

fn zed_icon_size(value: IconSize) -> ui::IconSize {
    match value {
        IconSize::Indicator => ui::IconSize::Indicator,
        IconSize::XSmall => ui::IconSize::XSmall,
        IconSize::Small => ui::IconSize::Small,
        IconSize::Medium => ui::IconSize::Medium,
        IconSize::XLarge => ui::IconSize::XLarge,
    }
}

fn zed_color(value: TextColor) -> ui::Color {
    match value {
        TextColor::Default => ui::Color::Default,
        TextColor::Muted => ui::Color::Muted,
        TextColor::Accent => ui::Color::Accent,
        TextColor::Disabled => ui::Color::Disabled,
        TextColor::Placeholder => ui::Color::Placeholder,
        TextColor::Error => ui::Color::Error,
        TextColor::Warning => ui::Color::Warning,
        TextColor::Success => ui::Color::Success,
    }
}

/// A labelled button in Zed's styles and sizes.
#[derive(IntoElement)]
pub struct Button {
    inner: ui::Button,
    id: ElementId,
    style: ButtonStyle,
    size: ButtonSize,
    layer: Option<ElevationIndex>,
    disabled: bool,
    toggled: bool,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let id = id.into();
        Self {
            inner: ui::Button::new(id.clone(), label),
            id,
            style: ButtonStyle::default(),
            size: ButtonSize::default(),
            layer: None,
            disabled: false,
            toggled: false,
        }
    }

    pub fn style(mut self, value: ButtonStyle) -> Self {
        self.style = value;
        self.inner = self.inner.style(value.zed());
        self
    }

    pub fn size(mut self, value: ButtonSize) -> Self {
        self.size = value;
        self.inner = self.inner.size(value.zed());
        self
    }

    pub fn layer(mut self, value: ElevationIndex) -> Self {
        self.layer = Some(value);
        self.inner = self.inner.layer(value.zed());
        self
    }

    pub fn disabled(mut self, value: bool) -> Self {
        self.disabled = value;
        self.inner = self.inner.disabled(value);
        self
    }

    pub fn toggle_state(mut self, value: bool) -> Self {
        self.toggled = value;
        self.inner = self.inner.toggle_state(value);
        self
    }

    pub fn selected_style(mut self, value: ButtonStyle) -> Self {
        self.inner = self.inner.selected_style(value.zed());
        self
    }

    pub fn start_icon(mut self, icon: IconName) -> Self {
        self.inner = self.inner.start_icon(ui::Icon::new(zed_icon_name(icon)));
        self
    }

    pub fn end_icon(mut self, icon: IconName) -> Self {
        self.inner = self.inner.end_icon(ui::Icon::new(zed_icon_name(icon)));
        self
    }

    pub fn tooltip(mut self, tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.inner = self.inner.tooltip(tooltip);
        self
    }

    pub fn on_click(mut self, callback: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        let id = self.id.clone();
        self.inner = self.inner.on_click(move |_, window, cx| {
            let _group = tracing::debug_span!("button-click", id = %id).entered();
            callback(window, cx)
        });
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        tracing::trace!(
            id = %self.id,
            style = ?self.style,
            size = ?self.size,
            layer = self.layer.map(tracing::field::debug),
            disabled = self.disabled,
            toggled = self.toggled,
            "rendering button",
        );
        self.inner
    }
}

/// A compact icon button in Zed's styles and sizes.
#[derive(IntoElement)]
pub struct IconButton {
    inner: ui::IconButton,
    id: ElementId,
    style: ButtonStyle,
    size: ButtonSize,
    layer: Option<ElevationIndex>,
    disabled: bool,
    toggled: bool,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName) -> Self {
        let id = id.into();
        Self {
            inner: ui::IconButton::new(id.clone(), zed_icon_name(icon)),
            id,
            style: ButtonStyle::default(),
            size: ButtonSize::default(),
            layer: None,
            disabled: false,
            toggled: false,
        }
    }

    pub fn style(mut self, value: ButtonStyle) -> Self {
        self.style = value;
        self.inner = self.inner.style(value.zed());
        self
    }

    pub fn size(mut self, value: ButtonSize) -> Self {
        self.size = value;
        self.inner = self.inner.size(value.zed());
        self
    }

    pub fn layer(mut self, value: ElevationIndex) -> Self {
        self.layer = Some(value);
        self.inner = self.inner.layer(value.zed());
        self
    }

    pub fn disabled(mut self, value: bool) -> Self {
        self.disabled = value;
        self.inner = self.inner.disabled(value);
        self
    }

    pub fn toggle_state(mut self, value: bool) -> Self {
        self.toggled = value;
        self.inner = self.inner.toggle_state(value);
        self
    }

    pub fn selected_style(mut self, value: ButtonStyle) -> Self {
        self.inner = self.inner.selected_style(value.zed());
        self
    }

    pub fn icon_size(mut self, value: IconSize) -> Self {
        self.inner = self.inner.icon_size(zed_icon_size(value));
        self
    }

    pub fn icon_color(mut self, value: TextColor) -> Self {
        self.inner = self.inner.icon_color(zed_color(value));
        self
    }

    pub fn shape(mut self, value: IconButtonShape) -> Self {
        self.inner = self.inner.shape(value.zed());
        self
    }

    pub fn tooltip(mut self, tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.inner = self.inner.tooltip(tooltip);
        self
    }

    pub fn on_click(mut self, callback: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        let id = self.id.clone();
        self.inner = self.inner.on_click(move |_, window, cx| {
            let _group = tracing::debug_span!("button-click", id = %id).entered();
            callback(window, cx)
        });
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        tracing::trace!(
            id = %self.id,
            style = ?self.style,
            size = ?self.size,
            layer = self.layer.map(tracing::field::debug),
            disabled = self.disabled,
            toggled = self.toggled,
            "rendering icon button",
        );
        self.inner
    }
}

#[cfg(test)]
mod tests;
