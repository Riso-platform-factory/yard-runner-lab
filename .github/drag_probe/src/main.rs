use gpui::{
    App, Bounds, Context, TitlebarOptions, Window, WindowBounds, WindowControlArea, WindowOptions,
    div, point, prelude::*, px, rgb, size,
};

struct Probe;

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xdddddd))
            .child(
                div()
                    .id("bar")
                    .h(px(32.))
                    .w_full()
                    .bg(rgb(0xbbbbbb))
                    .window_control_area(WindowControlArea::Drag)
                    .child("drag probe"),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::new(point(px(100.), px(80.)), size(px(800.), px(600.)));
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("drag probe".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Probe),
        )
        .unwrap();
    });
}
