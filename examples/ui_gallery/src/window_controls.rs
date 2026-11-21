use gpui::{
    App, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, WindowButton,
    rems,
};
use yard_ui::{Label, LabelSize, TextColor, WindowButtons, WindowsWindowButtons, h_flex, v_flex};

pub(super) fn register(cx: &mut App, ext: &yard_shell::Extension) {
    ext.register(cx, "ui_gallery.window_controls", &[], |_, _, _, cx| {
        cx.new(|_| WindowControlsPage).into()
    });
}

pub(super) fn place(cx: &mut App, ext: &yard_shell::Extension) {
    ext.place(
        cx,
        "window-controls",
        "ui_gallery.window_controls",
        "root",
        "children",
    );
    tracing::debug!(page = "window-controls", "page placed");
}

/// Places the page alone as the root, for a capture that shows only it.
pub(super) fn place_alone(cx: &mut App, ext: &yard_shell::Extension) {
    ext.place_root(cx, "root", "ui_gallery.window_controls");
    tracing::debug!(page = "window-controls", "page placed");
}

struct WindowControlsPage;

fn row(name: &str, buttons: impl IntoElement) -> impl IntoElement {
    h_flex()
        .gap(rems(0.75))
        .child(
            h_flex().w(rems(8.)).child(
                Label::new(name.to_string())
                    .size(LabelSize::Small)
                    .color(TextColor::Muted),
            ),
        )
        .child(buttons)
}

impl Render for WindowControlsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::trace!(page = "window-controls", "rendering page");
        let palette = yard_ui::palette(cx);
        let default = yard_ui::default_button_layout();
        let left = yard_ui::parse_button_layout("close,minimize,maximize:")
            .expect("the layout names known buttons");
        v_flex()
            .size_full()
            .p(rems(1.))
            .gap(rems(0.75))
            .bg(palette.background)
            .child(crate::group(
                "Layout",
                v_flex()
                    .gap(rems(0.5))
                    .child(row(
                        "Default",
                        WindowButtons::new("default", default.right).maximized(false),
                    ))
                    .child(row(
                        "Left",
                        WindowButtons::new("left", left.left).maximized(false),
                    )),
            ))
            .child(crate::group(
                "State",
                v_flex()
                    .gap(rems(0.5))
                    .child(row(
                        "Maximized",
                        WindowButtons::new("maximized", default.right).maximized(true),
                    ))
                    .child(row(
                        "Hover minimize",
                        WindowButtons::new("hover-minimize", default.right)
                            .maximized(false)
                            .hovered(WindowButton::Minimize),
                    ))
                    .child(row(
                        "Hover maximize",
                        WindowButtons::new("hover-maximize", default.right)
                            .maximized(false)
                            .hovered(WindowButton::Maximize),
                    ))
                    .child(row(
                        "Hover close",
                        WindowButtons::new("hover-close", default.right)
                            .maximized(false)
                            .hovered(WindowButton::Close),
                    )),
            ))
            .child(crate::group(
                "Windows",
                v_flex()
                    .gap(rems(0.5))
                    .child(row(
                        "Normal",
                        WindowsWindowButtons::new("windows-normal")
                            .preview()
                            .maximized(false),
                    ))
                    .child(row(
                        "Maximized",
                        WindowsWindowButtons::new("windows-maximized")
                            .preview()
                            .maximized(true),
                    ))
                    .child(row(
                        "Hover close",
                        WindowsWindowButtons::new("windows-hover-close")
                            .preview()
                            .maximized(false)
                            .hovered(WindowButton::Close),
                    )),
            ))
    }
}
