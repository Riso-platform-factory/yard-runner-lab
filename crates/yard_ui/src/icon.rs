//! Wraps Zed's `ui` crate at revision
//! ddb90e5e89d346f21847281727e1644b8bb2cc37, licensed GPL-3.0-or-later.
//! Copyright Zed Industries, Inc.

use crate::TextColor;
use gpui::{App, AssetSource, IntoElement, Rems, RenderOnce, SharedString, Window, rems};
use std::borrow::Cow;

/// The bundled icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconName {
    Check,
    ChevronDown,
    ChevronRight,
    Close,
    GenericClose,
    GenericMaximize,
    GenericMinimize,
    GenericRestore,
    Info,
    Plus,
    Warning,
    XCircle,
}

impl IconName {
    pub const ALL: &[IconName] = &[
        Self::Check,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::Close,
        Self::GenericClose,
        Self::GenericMaximize,
        Self::GenericMinimize,
        Self::GenericRestore,
        Self::Info,
        Self::Plus,
        Self::Warning,
        Self::XCircle,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Self::Check => "icons/check.svg",
            Self::ChevronDown => "icons/chevron_down.svg",
            Self::ChevronRight => "icons/chevron_right.svg",
            Self::Close => "icons/close.svg",
            Self::GenericClose => "icons/generic_close.svg",
            Self::GenericMaximize => "icons/generic_maximize.svg",
            Self::GenericMinimize => "icons/generic_minimize.svg",
            Self::GenericRestore => "icons/generic_restore.svg",
            Self::Info => "icons/info.svg",
            Self::Plus => "icons/plus.svg",
            Self::Warning => "icons/warning.svg",
            Self::XCircle => "icons/x_circle.svg",
        }
    }

    pub(crate) fn zed(self) -> ui::IconName {
        match self {
            Self::Check => ui::IconName::Check,
            Self::ChevronDown => ui::IconName::ChevronDown,
            Self::ChevronRight => ui::IconName::ChevronRight,
            Self::Close => ui::IconName::Close,
            Self::GenericClose => ui::IconName::GenericClose,
            Self::GenericMaximize => ui::IconName::GenericMaximize,
            Self::GenericMinimize => ui::IconName::GenericMinimize,
            Self::GenericRestore => ui::IconName::GenericRestore,
            Self::Info => ui::IconName::Info,
            Self::Plus => ui::IconName::Plus,
            Self::Warning => ui::IconName::Warning,
            Self::XCircle => ui::IconName::XCircle,
        }
    }
}

/// The size of an [`Icon`], in rems.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconSize {
    Indicator,
    XSmall,
    Small,
    #[default]
    Medium,
    XLarge,
}

impl IconSize {
    pub fn rems(self) -> Rems {
        match self {
            Self::Indicator => rems(0.625),
            Self::XSmall => rems(0.75),
            Self::Small => rems(0.875),
            Self::Medium => rems(1.),
            Self::XLarge => rems(3.),
        }
    }

    fn zed(self) -> ui::IconSize {
        match self {
            Self::Indicator => ui::IconSize::Indicator,
            Self::XSmall => ui::IconSize::XSmall,
            Self::Small => ui::IconSize::Small,
            Self::Medium => ui::IconSize::Medium,
            Self::XLarge => ui::IconSize::XLarge,
        }
    }
}

fn zed_color(color: TextColor) -> ui::Color {
    match color {
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

/// An icon by name, in Zed's sizes and colors.
#[derive(IntoElement)]
pub struct Icon {
    inner: ui::Icon,
    name: IconName,
    size: IconSize,
    color: TextColor,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            inner: ui::Icon::new(name.zed()),
            name,
            size: IconSize::default(),
            color: TextColor::default(),
        }
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self.inner = self.inner.size(size.zed());
        self
    }

    pub fn color(mut self, color: TextColor) -> Self {
        self.color = color;
        self.inner = self.inner.color(zed_color(color));
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        tracing::trace!(
            name = ?self.name,
            size = ?self.size,
            color = ?self.color,
            "rendering icon",
        );
        self.inner
    }
}

/// The bundled icon files, given to GPUI by the startup.
pub struct Assets;

const FILES: &[(&str, &[u8])] = &[
    (
        "icons/arrow_down.svg",
        include_bytes!("../assets/icons/arrow_down.svg"),
    ),
    (
        "icons/arrow_left.svg",
        include_bytes!("../assets/icons/arrow_left.svg"),
    ),
    (
        "icons/arrow_right.svg",
        include_bytes!("../assets/icons/arrow_right.svg"),
    ),
    (
        "icons/arrow_up.svg",
        include_bytes!("../assets/icons/arrow_up.svg"),
    ),
    (
        "icons/backspace.svg",
        include_bytes!("../assets/icons/backspace.svg"),
    ),
    (
        "icons/command.svg",
        include_bytes!("../assets/icons/command.svg"),
    ),
    (
        "icons/control.svg",
        include_bytes!("../assets/icons/control.svg"),
    ),
    (
        "icons/delete.svg",
        include_bytes!("../assets/icons/delete.svg"),
    ),
    (
        "icons/escape.svg",
        include_bytes!("../assets/icons/escape.svg"),
    ),
    (
        "icons/option.svg",
        include_bytes!("../assets/icons/option.svg"),
    ),
    (
        "icons/page_down.svg",
        include_bytes!("../assets/icons/page_down.svg"),
    ),
    (
        "icons/page_up.svg",
        include_bytes!("../assets/icons/page_up.svg"),
    ),
    (
        "icons/return.svg",
        include_bytes!("../assets/icons/return.svg"),
    ),
    (
        "icons/shift.svg",
        include_bytes!("../assets/icons/shift.svg"),
    ),
    (
        "icons/space.svg",
        include_bytes!("../assets/icons/space.svg"),
    ),
    ("icons/tab.svg", include_bytes!("../assets/icons/tab.svg")),
    (
        "icons/check.svg",
        include_bytes!("../assets/icons/check.svg"),
    ),
    (
        "icons/chevron_down.svg",
        include_bytes!("../assets/icons/chevron_down.svg"),
    ),
    (
        "icons/chevron_right.svg",
        include_bytes!("../assets/icons/chevron_right.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../assets/icons/close.svg"),
    ),
    (
        "icons/generic_close.svg",
        include_bytes!("../assets/icons/generic_close.svg"),
    ),
    (
        "icons/generic_maximize.svg",
        include_bytes!("../assets/icons/generic_maximize.svg"),
    ),
    (
        "icons/generic_minimize.svg",
        include_bytes!("../assets/icons/generic_minimize.svg"),
    ),
    (
        "icons/generic_restore.svg",
        include_bytes!("../assets/icons/generic_restore.svg"),
    ),
    ("icons/info.svg", include_bytes!("../assets/icons/info.svg")),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    (
        "icons/warning.svg",
        include_bytes!("../assets/icons/warning.svg"),
    ),
    (
        "icons/x_circle.svg",
        include_bytes!("../assets/icons/x_circle.svg"),
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match FILES
            .iter()
            .find_map(|&(bundled, bytes)| (bundled == path).then_some(bytes))
        {
            Some(bytes) => {
                tracing::trace!(path, "asset loaded");
                Ok(Some(Cow::Borrowed(bytes)))
            }
            None => {
                log::warn!("asset {path} is not bundled");
                Ok(None)
            }
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(bundled, _)| bundled.starts_with(path))
            .map(|&(bundled, _)| SharedString::new_static(bundled))
            .collect())
    }
}

#[cfg(test)]
mod tests;
