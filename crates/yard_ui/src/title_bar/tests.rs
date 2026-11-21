use super::*;
use crate::{default_button_layout, parse_button_layout, v_flex};
use gpui::{TestAppContext, Tiling, WindowButtonLayout, div};

const ITEM_WIDTH: f32 = 40.;

struct Bar {
    host: (Platform, Decorations, Option<WindowButtonLayout>),
}

fn item(selector: &'static str) -> impl IntoElement {
    div()
        .debug_selector(move || selector.into())
        .w(px(ITEM_WIDTH))
        .h_4()
}

impl gpui::Render for Bar {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let mut bar = TitleBar::new("title-bar")
            .left(item("LEFT"))
            .center(item("CENTER"))
            .right(item("RIGHT"));
        bar.host = Some(self.host);
        v_flex()
            .debug_selector(|| "BAR".into())
            .w(px(800.))
            .child(bar)
    }
}

fn client() -> Decorations {
    Decorations::Client {
        tiling: Tiling::default(),
    }
}

/// Draws the bar with its window buttons as `host` has them, and checks that
/// the center item stays in the middle and every item in order.
fn assert_center_stays_in_the_middle(
    cx: &mut TestAppContext,
    host: (Platform, Decorations, Option<WindowButtonLayout>),
) {
    cx.update(crate::init);
    let (_, cx) = cx.add_window_view(|_, _| Bar { host });
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let bar = cx.debug_bounds("BAR").expect("the bar is drawn");
    let left = cx.debug_bounds("LEFT").expect("the left item is drawn");
    let center = cx.debug_bounds("CENTER").expect("the center item is drawn");
    let right = cx.debug_bounds("RIGHT").expect("the right item is drawn");
    let offset = (center.center().x - bar.center().x).abs();
    assert!(
        offset < px(0.5),
        "{host:?}: {center:?} is {offset:?} off the middle of {bar:?}"
    );
    assert!(
        left.right() <= center.left() && center.right() <= right.left(),
        "{host:?}: {left:?} {center:?} {right:?}"
    );
}

#[gpui::test]
fn the_center_stays_in_the_middle_beside_the_windows_buttons(cx: &mut TestAppContext) {
    assert_center_stays_in_the_middle(cx, (Platform::Windows, Decorations::Server, None));
}

#[gpui::test]
fn the_center_stays_in_the_middle_beside_linux_buttons_on_the_right(cx: &mut TestAppContext) {
    assert_center_stays_in_the_middle(
        cx,
        (Platform::Linux, client(), Some(default_button_layout())),
    );
}

#[gpui::test]
fn the_center_stays_in_the_middle_beside_linux_buttons_on_the_left(cx: &mut TestAppContext) {
    assert_center_stays_in_the_middle(
        cx,
        (
            Platform::Linux,
            client(),
            parse_button_layout("close,minimize,maximize:"),
        ),
    );
}

#[gpui::test]
fn the_center_stays_in_the_middle_beside_linux_buttons_on_both_sides(cx: &mut TestAppContext) {
    assert_center_stays_in_the_middle(
        cx,
        (
            Platform::Linux,
            client(),
            parse_button_layout("close:minimize,maximize"),
        ),
    );
}

#[gpui::test]
fn the_center_stays_in_the_middle_without_buttons(cx: &mut TestAppContext) {
    assert_center_stays_in_the_middle(cx, (Platform::Mac, Decorations::Server, None));
}
