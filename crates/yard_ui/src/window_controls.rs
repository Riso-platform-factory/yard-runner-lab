//! Adapts Zed's `platform_title_bar` crate at revision
//! ddb90e5e89d346f21847281727e1644b8bb2cc37, licensed GPL-3.0-or-later.
//! Copyright Zed Industries, Inc.
//!
//! `WindowsWindowButtons` adapts `WindowsWindowControls` and
//! `WindowsCaptionButton`, from `platforms/platform_windows.rs`.
//!
//! `parse_button_layout` adapts GPUI's `WindowButtonLayout::parse`, from the
//! same revision, licensed Apache-2.0. Copyright Zed Industries, Inc.

use crate::{IconName, h_flex};
use gpui::{
    App, Decorations, ElementId, Hsla, InteractiveElement, IntoElement, MAX_BUTTONS_PER_SIDE,
    MouseButton, ParentElement, Pixels, Point, RenderOnce, ResizeEdge, Rgba,
    StatefulInteractiveElement, Styled, Window, WindowButton, WindowButtonLayout,
    WindowControlArea, WindowControls, div, prelude::FluentBuilder, px, svg,
};
use theme::ActiveTheme;

/// The buttons of one side of a title bar, in their order.
pub type ButtonSide = [Option<WindowButton>; MAX_BUTTONS_PER_SIDE];

/// The layout drawn when the desktop names none: minimize, maximize, and
/// close on the right, as GPUI's Linux default.
pub fn default_button_layout() -> WindowButtonLayout {
    WindowButtonLayout {
        left: [None; MAX_BUTTONS_PER_SIDE],
        right: [
            Some(WindowButton::Minimize),
            Some(WindowButton::Maximize),
            Some(WindowButton::Close),
        ],
    }
}

/// Parses a GNOME-style button layout, such as `close,minimize:maximize`:
/// the buttons before `:` go left, the others right. A name it does not know
/// is skipped, and so is a button already placed. `None` when the string
/// names buttons but none it knows.
pub fn parse_button_layout(layout: &str) -> Option<WindowButtonLayout> {
    fn index(button: WindowButton) -> usize {
        match button {
            WindowButton::Minimize => 0,
            WindowButton::Maximize => 1,
            WindowButton::Close => 2,
        }
    }

    fn parse_side(
        side: &str,
        seen: &mut [bool; MAX_BUTTONS_PER_SIDE],
        unknown: &mut usize,
    ) -> ButtonSide {
        let mut result = [None; MAX_BUTTONS_PER_SIDE];
        let mut next = 0;
        for name in side
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let button = match name {
                "minimize" => WindowButton::Minimize,
                "maximize" => WindowButton::Maximize,
                "close" => WindowButton::Close,
                _ => {
                    *unknown += 1;
                    continue;
                }
            };
            if seen[index(button)] {
                continue;
            }
            if let Some(slot) = result.get_mut(next) {
                *slot = Some(button);
                seen[index(button)] = true;
                next += 1;
            }
        }
        result
    }

    let (left, right) = layout.split_once(':').unwrap_or(("", layout));
    let mut seen = [false; MAX_BUTTONS_PER_SIDE];
    let mut unknown = 0;
    let parsed = WindowButtonLayout {
        left: parse_side(left, &mut seen, &mut unknown),
        right: parse_side(right, &mut seen, &mut unknown),
    };
    let empty = parsed.left.iter().chain(&parsed.right).all(Option::is_none);
    (unknown == 0 || !empty).then_some(parsed)
}

/// The system YARD runs on, as far as the title bar cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Mac,
    Linux,
    Windows,
}

impl Platform {
    pub(crate) const CURRENT: Platform = if cfg!(target_os = "macos") {
        Platform::Mac
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    };
}

/// The buttons a title bar draws on each side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrawnSides {
    pub(crate) left: Option<ButtonSide>,
    pub(crate) right: Option<ButtonSide>,
}

/// The buttons a title bar draws: on Linux with client decorations, each side
/// of the desktop's layout that has a button, and nothing anywhere else.
pub(crate) fn drawn_sides(
    platform: Platform,
    decorations: Decorations,
    layout: Option<WindowButtonLayout>,
) -> DrawnSides {
    let client = matches!(decorations, Decorations::Client { .. });
    match layout {
        Some(layout) if platform == Platform::Linux && client => DrawnSides {
            left: layout.left[0].is_some().then_some(layout.left),
            right: layout.right[0].is_some().then_some(layout.right),
        },
        _ => DrawnSides::default(),
    }
}

/// Whether the title bar draws the Windows caption buttons: on Windows,
/// whenever the window is not fullscreen. Windows has no choice of
/// decorations, so they do not depend on it.
pub(crate) fn draws_windows_buttons(platform: Platform, fullscreen: bool) -> bool {
    platform == Platform::Windows && !fullscreen
}

/// The buttons of `side` that the platform supports, in order. Close is
/// always supported.
pub(crate) fn shown_buttons(side: &ButtonSide, supported: WindowControls) -> Vec<WindowButton> {
    side.iter()
        .flatten()
        .copied()
        .filter(|button| match button {
            WindowButton::Minimize => supported.minimize,
            WindowButton::Maximize => supported.maximize,
            WindowButton::Close => true,
        })
        .collect()
}

/// What the title bar asks of the window. Each one runs in its own
/// `window-control{action}` group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowControlAction {
    Minimize,
    Maximize,
    Restore,
    Close,
    /// Lets the window manager move the window with the pointer.
    Move,
    /// Lets the window manager resize the window from an edge or a corner.
    Resize(ResizeEdge),
    /// Opens the window manager's menu for the window, at a point in it.
    Menu(Point<Pixels>),
}

impl WindowControlAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Maximize => "maximize",
            Self::Restore => "restore",
            Self::Close => "close",
            Self::Move => "move",
            Self::Resize(_) => "resize",
            Self::Menu(_) => "menu",
        }
    }

    /// Logs an action that the system does itself: on Windows a caption
    /// button is a control area of the window, so Windows minimizes,
    /// maximizes, restores, or closes it once the click ends.
    pub fn leave_to_system(self) {
        let _group = tracing::info_span!("window-control", action = self.name()).entered();
        tracing::debug!("left to the system");
    }

    /// Asks the window to do it.
    pub fn perform(self, window: &mut Window) {
        let _group = tracing::info_span!("window-control", action = self.name()).entered();
        match self {
            Self::Minimize => {
                tracing::debug!("minimizing");
                window.minimize_window();
            }
            Self::Maximize => {
                tracing::debug!("maximizing");
                window.zoom_window();
            }
            Self::Restore => {
                tracing::debug!("restoring");
                window.zoom_window();
            }
            Self::Close => {
                tracing::debug!("closing");
                window.remove_window();
            }
            Self::Move => {
                tracing::debug!("move started");
                window.start_window_move();
            }
            Self::Resize(edge) => {
                tracing::debug!(edge = ?edge, "resize started");
                window.start_window_resize(edge);
            }
            Self::Menu(position) => {
                tracing::debug!(
                    x = f32::from(position.x),
                    y = f32::from(position.y),
                    "window menu opened"
                );
                window.show_window_menu(position);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Minimize,
    Restore,
    Maximize,
    Close,
}

impl Kind {
    fn icon(self) -> IconName {
        match self {
            Kind::Minimize => IconName::GenericMinimize,
            Kind::Restore => IconName::GenericRestore,
            Kind::Maximize => IconName::GenericMaximize,
            Kind::Close => IconName::GenericClose,
        }
    }

    /// The element ID of the Windows caption button.
    fn windows_id(self) -> &'static str {
        match self {
            Kind::Minimize => "minimize",
            Kind::Restore => "restore",
            Kind::Maximize => "maximize",
            Kind::Close => "close",
        }
    }

    /// The glyph of the Windows caption button, in Segoe Fluent Icons and
    /// Segoe MDL2 Assets alike.
    fn glyph(self) -> &'static str {
        match self {
            Kind::Minimize => "\u{e921}",
            Kind::Restore => "\u{e923}",
            Kind::Maximize => "\u{e922}",
            Kind::Close => "\u{e8bb}",
        }
    }

    /// The part of the window that Windows hit-tests the caption button as.
    fn control_area(self) -> WindowControlArea {
        match self {
            Kind::Minimize => WindowControlArea::Min,
            Kind::Restore | Kind::Maximize => WindowControlArea::Max,
            Kind::Close => WindowControlArea::Close,
        }
    }

    fn action(self) -> WindowControlAction {
        match self {
            Kind::Minimize => WindowControlAction::Minimize,
            Kind::Restore => WindowControlAction::Restore,
            Kind::Maximize => WindowControlAction::Maximize,
            Kind::Close => WindowControlAction::Close,
        }
    }
}

/// Minimize, maximize or restore, and close buttons, drawn as Zed draws them
/// on Linux. By default it follows the window it is in; `maximized`,
/// `supported`, and `hovered` set the state it draws instead, so that it can
/// be shown in any window.
#[derive(IntoElement)]
pub struct WindowButtons {
    id: ElementId,
    buttons: ButtonSide,
    maximized: Option<bool>,
    supported: Option<WindowControls>,
    hovered: Option<WindowButton>,
}

impl WindowButtons {
    pub fn new(id: impl Into<ElementId>, buttons: ButtonSide) -> Self {
        Self {
            id: id.into(),
            buttons,
            maximized: None,
            supported: None,
            hovered: None,
        }
    }

    /// Draws restore in place of maximize when `true`.
    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = Some(maximized);
        self
    }

    /// The buttons the platform supports; the others are not drawn.
    pub fn supported(mut self, supported: WindowControls) -> Self {
        self.supported = Some(supported);
        self
    }

    /// Draws `button` as if the pointer were on it.
    pub fn hovered(mut self, button: WindowButton) -> Self {
        self.hovered = Some(button);
        self
    }
}

impl RenderOnce for WindowButtons {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let maximized = self.maximized.unwrap_or_else(|| window.is_maximized());
        let supported = self.supported.unwrap_or_else(|| window.window_controls());
        let shown = shown_buttons(&self.buttons, supported);
        tracing::trace!(buttons = shown.len(), maximized, "rendering window buttons");
        let colors = cx.theme().colors();
        let (icon, icon_hover, icon_disabled, background_hover) = (
            colors.icon,
            colors.icon_muted,
            colors.icon_disabled,
            colors.ghost_element_hover,
        );
        let elements: Vec<_> = shown
            .into_iter()
            .map(|button| {
                let kind = match button {
                    WindowButton::Minimize => Kind::Minimize,
                    WindowButton::Maximize if maximized => Kind::Restore,
                    WindowButton::Maximize => Kind::Maximize,
                    WindowButton::Close => Kind::Close,
                };
                let enabled = match kind {
                    Kind::Minimize => window.is_minimizable(),
                    Kind::Restore | Kind::Maximize => window.is_resizable(),
                    Kind::Close => true,
                };
                let hovered = enabled && self.hovered == Some(button);
                let svg = svg()
                    .size_4()
                    .flex_none()
                    .path(kind.icon().path())
                    .text_color(match (enabled, hovered) {
                        (false, _) => icon_disabled,
                        (true, true) => icon_hover,
                        (true, false) => icon,
                    })
                    .when(enabled, |svg| {
                        svg.group_hover("window-button", |svg| svg.text_color(icon_hover))
                    });
                h_flex()
                    .id(button.id())
                    .group("window-button")
                    .justify_center()
                    .content_center()
                    .rounded_2xl()
                    .w_5()
                    .h_5()
                    .when(hovered, |this| this.bg(background_hover))
                    .when(enabled, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.bg(background_hover))
                            .active(|this| this.bg(background_hover))
                    })
                    .child(svg)
                    .on_mouse_move(|_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        if enabled {
                            kind.action().perform(window);
                        }
                    })
            })
            .collect();

        h_flex().id(self.id).when(!elements.is_empty(), |this| {
            this.gap_3()
                .px_3()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(elements)
        })
    }
}

/// The Windows caption buttons, in order: minimize, maximize or restore, and
/// close.
fn windows_buttons(maximized: bool) -> [Kind; 3] {
    [
        Kind::Minimize,
        if maximized {
            Kind::Restore
        } else {
            Kind::Maximize
        },
        Kind::Close,
    ]
}

/// The height of Zed's title bar on Windows, which the caption buttons fill.
const WINDOWS_CAPTION_HEIGHT: Pixels = px(32.);

/// The font of the caption glyphs: Segoe Fluent Icons from Windows 11's
/// build 22000, and Segoe MDL2 Assets before it. Elsewhere, where only the
/// gallery draws them, Segoe Fluent Icons, which may not be installed.
#[cfg(not(target_os = "windows"))]
fn caption_font() -> &'static str {
    "Segoe Fluent Icons"
}

#[cfg(target_os = "windows")]
fn caption_font() -> &'static str {
    use windows::Wdk::System::SystemServices::RtlGetVersion;

    // SAFETY: `RtlGetVersion` fills the zeroed structure it is given.
    let mut version = unsafe { std::mem::zeroed() };
    let status = unsafe { RtlGetVersion(&mut version) };

    if status.is_ok() && version.dwBuildNumber >= 22000 {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

/// Minimize, maximize or restore, and close, drawn as Zed draws them on
/// Windows. Each button is a control area of the window, so Windows does what
/// it asks, and shows Snap layouts on maximize; the button only logs the
/// click. By default it follows the window it is in; `maximized` and
/// `hovered` set the state it draws instead, so that it can be shown in any
/// window.
#[derive(IntoElement)]
pub struct WindowsWindowButtons {
    id: ElementId,
    maximized: Option<bool>,
    hovered: Option<WindowButton>,
}

impl WindowsWindowButtons {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            maximized: None,
            hovered: None,
        }
    }

    /// Draws restore in place of maximize when `true`.
    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = Some(maximized);
        self
    }

    /// Draws `button` as if the pointer were on it.
    pub fn hovered(mut self, button: WindowButton) -> Self {
        self.hovered = Some(button);
        self
    }
}

impl RenderOnce for WindowsWindowButtons {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let maximized = self.maximized.unwrap_or_else(|| window.is_maximized());
        tracing::trace!(maximized, "rendering windows caption buttons");
        let colors = cx.theme().colors();
        let close_red: Hsla = Rgba {
            r: 232.0 / 255.0,
            g: 17.0 / 255.0,
            b: 32.0 / 255.0,
            a: 1.0,
        }
        .into();
        let buttons = windows_buttons(maximized).map(|kind| {
            let enabled = match kind {
                Kind::Minimize => window.is_minimizable(),
                Kind::Restore | Kind::Maximize => window.is_resizable(),
                Kind::Close => true,
            };
            let (hover_bg, hover_fg, active_bg, active_fg) = match kind {
                Kind::Close => (
                    close_red,
                    gpui::white(),
                    close_red.opacity(0.8),
                    gpui::white().opacity(0.8),
                ),
                _ => (
                    colors.ghost_element_hover,
                    colors.text,
                    colors.ghost_element_active,
                    colors.text,
                ),
            };
            let button = match kind {
                Kind::Minimize => WindowButton::Minimize,
                Kind::Restore | Kind::Maximize => WindowButton::Maximize,
                Kind::Close => WindowButton::Close,
            };
            let hovered = enabled && self.hovered == Some(button);
            h_flex()
                .id(kind.windows_id())
                .justify_center()
                .content_center()
                .occlude()
                .w(px(36.))
                .h_full()
                .text_size(px(10.0))
                .when(!enabled, |style| style.text_color(colors.text_disabled))
                .when(hovered, |style| style.bg(hover_bg).text_color(hover_fg))
                .when(enabled, |style| {
                    style
                        .hover(|style| style.bg(hover_bg).text_color(hover_fg))
                        .active(|style| style.bg(active_bg).text_color(active_fg))
                        // Windows still sends the press and the release
                        // here, and acts after them unless an element stops
                        // them, so this only logs.
                        .on_click(move |_, _, _| kind.action().leave_to_system())
                })
                .window_control_area(kind.control_area())
                .child(kind.glyph())
        });

        div()
            .id(self.id)
            .font_family(caption_font())
            .flex()
            .flex_row()
            .justify_center()
            .content_stretch()
            .max_h(WINDOWS_CAPTION_HEIGHT)
            .min_h(WINDOWS_CAPTION_HEIGHT)
            .children(buttons)
    }
}

#[cfg(test)]
mod tests;
