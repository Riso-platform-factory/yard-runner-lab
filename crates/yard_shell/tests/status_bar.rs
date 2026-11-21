use gpui::{AppContext, InteractiveElement, IntoElement, ParentElement, Styled, TestAppContext};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use yard_shell::{Extension, FocusDown, FocusUp};

const WIDTH: f32 = 800.;
const HEIGHT: f32 = 600.;
// The title bar's height on each platform, as Zed's `platform_title_bar_height`
// gives it: 32 px on Windows, and at least 34 px elsewhere.
const TITLE_BAR: f32 = if cfg!(windows) { 32. } else { 34. };
const ITEM_WIDTH: f32 = 40.;
const ITEM_HEIGHT: f32 = 20.;
// Zed's `Base04` is 4px a side at the default density: the bar's height is its
// tallest item plus the padding.
const STATUS_BAR: f32 = ITEM_HEIGHT + 8.;

fn window_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT)),
    )
}

fn title_bar_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(WIDTH), gpui::px(TITLE_BAR)),
    )
}

fn status_bar_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(HEIGHT - STATUS_BAR)),
        gpui::size(gpui::px(WIDTH), gpui::px(STATUS_BAR)),
    )
}

/// The body reaching the bottom of the window: no status bar takes room.
fn body_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(TITLE_BAR)),
        gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT - TITLE_BAR)),
    )
}

/// The body shrunk above a drawn status bar.
fn body_frame_with_bar() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(TITLE_BAR)),
        gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT - TITLE_BAR - STATUS_BAR)),
    )
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-status-bar-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Card {
    node: String,
    width: Option<gpui::Pixels>,
    height: Option<gpui::Pixels>,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let card = gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full();
        let card = match self.width {
            Some(width) => card.w(width),
            None => card,
        };
        match self.height {
            Some(height) => card.h(height),
            None => card,
        }
    }
}

struct Ping {
    node: String,
    clicks: Rc<Cell<usize>>,
}

impl gpui::Render for Ping {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let clicks = self.clicks.clone();
        gpui::div()
            .debug_selector(|| format!("BUTTON-{}", self.node))
            .child(
                yard_ui::Button::new("ping", "ping")
                    .size(yard_ui::ButtonSize::Compact)
                    .on_click(move |_, _| clicks.set(clicks.get() + 1)),
            )
    }
}

fn draw(window: gpui::WindowHandle<yard_shell::Shell>, cx: &mut TestAppContext) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
}

/// Opens an 800×600 dev-shell window over the layout that `place` builds.
fn open_app(
    cx: &mut TestAppContext,
    place: impl FnOnce(&mut gpui::App, &Extension),
) -> (Extension, gpui::WindowHandle<yard_shell::Shell>) {
    cx.update(yard_ui::init);
    let ext = cx.update(|cx| {
        let ext = Extension::new(cx, "tour").unwrap();
        ext.register(cx, "tour.card", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                width: None,
                height: None,
            })
            .into()
        });
        ext.register(cx, "tour.item", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                width: Some(gpui::px(ITEM_WIDTH)),
                height: Some(gpui::px(ITEM_HEIGHT)),
            })
            .into()
        });
        place(cx, &ext);
        ext
    });
    let window = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
            .unwrap()
    });
    cx.simulate_window_resize(window.into(), gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT)));
    draw(window, cx);
    (ext, window)
}

/// A node's bounds in the last draw, if it was drawn.
fn node_bounds(
    visual: &mut gpui::VisualTestContext,
    node: &str,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    visual.update(|_, cx| yard_shell::node_bounds(cx, node))
}

/// The bounds of every `nodes` item in the last draw.
fn drawn_bounds(
    visual: &mut gpui::VisualTestContext,
    nodes: &[&'static str],
) -> BTreeMap<&'static str, Option<gpui::Bounds<gpui::Pixels>>> {
    nodes
        .iter()
        .map(|node| (*node, node_bounds(visual, node)))
        .collect()
}

/// The status bar is not drawn: absent from the draw, or takes no room.
fn assert_bar_absent(visual: &mut gpui::VisualTestContext, when: &str) {
    let bounds = node_bounds(visual, "status_bar");
    assert!(
        bounds.is_none_or(|bounds| bounds.size.height == gpui::px(0.)),
        "{when}: the status bar is drawn: {bounds:?}"
    );
}

/// Every node keeps the bounds it had, and nothing is zoomed, after `op`.
fn assert_unchanged(
    visual: &mut gpui::VisualTestContext,
    before: &BTreeMap<&'static str, Option<gpui::Bounds<gpui::Pixels>>>,
    op: &str,
) {
    for (node, expected) in before {
        assert_eq!(node_bounds(visual, node), *expected, "{op} changed {node}");
    }
    assert_eq!(
        visual.update(|_, cx| yard_shell::zoomed(cx)),
        None,
        "{op} zoomed a node"
    );
}

/// The node the dev shell says has the focus, if any.
fn focused(visual: &mut gpui::VisualTestContext) -> Option<String> {
    visual.update(|window, cx| yard_shell::focused_node(window, cx).map(|node| node.to_string()))
}

/// Clicks the center of the element with `selector`.
fn click(visual: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} has bounds"));
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
}

fn assert_inside(outer: gpui::Bounds<gpui::Pixels>, inner: gpui::Bounds<gpui::Pixels>) {
    assert!(
        inner.left() >= outer.left()
            && inner.top() >= outer.top()
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom(),
        "{inner:?} is not inside {outer:?}"
    );
}

#[gpui::test]
fn with_no_item_the_body_reaches_the_window_bottom(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(node_bounds(&mut visual, "window"), Some(window_frame()));
    assert_eq!(
        node_bounds(&mut visual, "title_bar"),
        Some(title_bar_frame())
    );
    assert_eq!(node_bounds(&mut visual, "body"), Some(body_frame()));
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame()),
        "the extension's root fills the body"
    );
    assert_bar_absent(&mut visual, "no item is placed");
}

#[gpui::test]
fn an_item_in_left_draws_the_bar_at_the_bottom_below_the_body(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let bar = node_bounds(&mut visual, "status_bar").expect("the bar is drawn");
    assert_eq!(bar, status_bar_frame());
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame_with_bar()),
        "the body ends where the bar starts"
    );
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame_with_bar())
    );
    let item = node_bounds(&mut visual, "item").expect("the item is drawn");
    assert_inside(bar, item);
    assert_eq!(
        item.center().y,
        bar.center().y,
        "the item is vertically centred in the bar"
    );
}

#[gpui::test]
fn items_are_drawn_in_slot_order_from_each_edge(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "left1", "tour.item", "status_bar", "left");
        ext.place(cx, "left2", "tour.item", "status_bar", "left");
        ext.place(cx, "right1", "tour.item", "status_bar", "right");
    });
    // `add` places a bar item after the window is open, as `place` does before.
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| {
            ext.add(window, cx, "right2", "tour.item", "status_bar", "right");
        })
        .unwrap();
    draw(window, cx);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let bar = node_bounds(&mut visual, "status_bar").expect("the bar is drawn");
    let left1 = node_bounds(&mut visual, "left1").expect("left1 is drawn");
    let left2 = node_bounds(&mut visual, "left2").expect("left2 is drawn");
    let right1 = node_bounds(&mut visual, "right1").expect("right1 is drawn");
    let right2 = node_bounds(&mut visual, "right2").expect("right2 is drawn");
    for (node, item) in [
        ("left1", left1),
        ("left2", left2),
        ("right1", right1),
        ("right2", right2),
    ] {
        assert_inside(bar, item);
        assert_eq!(
            item.size.width,
            gpui::px(ITEM_WIDTH),
            "{node} keeps its own width"
        );
        assert_eq!(item.center().y, bar.center().y, "{node} is centred");
    }
    assert!(
        left1.left() >= bar.left() && left1.left() - bar.left() <= gpui::px(8.),
        "{left1:?} starts at the left edge of {bar:?}"
    );
    assert!(
        left1.right() <= left2.left(),
        "{left1:?} overlaps or is right of {left2:?}"
    );
    assert!(
        right1.right() <= right2.left(),
        "{right1:?} overlaps or is right of {right2:?}"
    );
    assert!(
        right2.right() <= bar.right() && bar.right() - right2.right() <= gpui::px(8.),
        "{right2:?} ends at the right edge of {bar:?}"
    );
    assert!(
        left2.right() <= right1.left(),
        "the left items {left1:?} {left2:?} overlap the right items {right1:?} {right2:?}"
    );
}

#[gpui::test]
fn items_that_overflow_clip_and_leave_the_right_item_at_the_edge(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.register(cx, "tour.wide", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                width: Some(gpui::px(400.)),
                height: Some(gpui::px(ITEM_HEIGHT)),
            })
            .into()
        });
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "left1", "tour.wide", "status_bar", "left");
        ext.place(cx, "left2", "tour.wide", "status_bar", "left");
        ext.place(cx, "left3", "tour.wide", "status_bar", "left");
        ext.place(cx, "right1", "tour.item", "status_bar", "right");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let bar = node_bounds(&mut visual, "status_bar").expect("the bar is drawn");
    assert_eq!(bar, status_bar_frame());
    let right1 = node_bounds(&mut visual, "right1").expect("the right item is drawn");
    assert_eq!(
        right1.size.width,
        gpui::px(ITEM_WIDTH),
        "the right item keeps its full width"
    );
    assert!(
        right1.right() <= bar.right() && bar.right() - right1.right() <= gpui::px(8.),
        "{right1:?} ends at the right edge of {bar:?}"
    );
    let left1 = node_bounds(&mut visual, "left1").expect("left1 is drawn");
    assert_eq!(
        left1.size.width,
        gpui::px(400.),
        "the left items keep their own width: the row clips, it does not squeeze"
    );
    assert!(
        left1.left() >= bar.left() && left1.left() - bar.left() <= gpui::px(8.),
        "{left1:?} starts at the left edge of {bar:?}"
    );
    // Three 400px items do not fit next to the right item: the left row shrinks
    // and clips, so a later item runs past the right item instead of pushing it
    // out of the bar.
    let left2 = node_bounds(&mut visual, "left2").expect("left2 is laid out");
    assert!(
        left2.right() > right1.left(),
        "the left row clips {left2:?} at the right item {right1:?}"
    );
    let left3 = node_bounds(&mut visual, "left3");
    assert!(
        left3.is_none_or(|bounds| bounds.left() >= right1.left()),
        "left3 is clipped away or drawn past the right item: {left3:?}"
    );
}

#[gpui::test]
fn the_bars_height_is_the_tallest_items_plus_its_padding(cx: &mut TestAppContext) {
    const TALL_ITEM: f32 = 30.;
    let (_, window) = open_app(cx, |cx, ext| {
        ext.register(cx, "tour.tall", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                width: Some(gpui::px(ITEM_WIDTH)),
                height: Some(gpui::px(TALL_ITEM)),
            })
            .into()
        });
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "short", "tour.item", "status_bar", "left");
        ext.place(cx, "tall", "tour.tall", "status_bar", "right");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    // The bar's padding on both sides together, from the 20px item's bar.
    let padding = STATUS_BAR - ITEM_HEIGHT;
    let bar_height = TALL_ITEM + padding;
    let bar = node_bounds(&mut visual, "status_bar").expect("the bar is drawn");
    assert_eq!(
        bar,
        gpui::Bounds::new(
            gpui::point(gpui::px(0.), gpui::px(HEIGHT - bar_height)),
            gpui::size(gpui::px(WIDTH), gpui::px(bar_height)),
        ),
        "the bar's height is the tallest item's plus the padding"
    );
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(gpui::Bounds::new(
            gpui::point(gpui::px(0.), gpui::px(TITLE_BAR)),
            gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT - TITLE_BAR - bar_height)),
        )),
        "the body ends where the bar starts"
    );
    let tall = node_bounds(&mut visual, "tall").expect("the tall item is drawn");
    assert_eq!(tall.size.height, gpui::px(TALL_ITEM));
    assert_eq!(
        tall.top() - bar.top(),
        gpui::px(padding / 2.),
        "the padding above the tallest item"
    );
    assert_eq!(
        bar.bottom() - tall.bottom(),
        gpui::px(padding / 2.),
        "the padding below the tallest item"
    );
    let short = node_bounds(&mut visual, "short").expect("the short item is drawn");
    assert_eq!(short.size.height, gpui::px(ITEM_HEIGHT));
    assert_eq!(
        short.center().y,
        bar.center().y,
        "the shorter item is vertically centred in the bar"
    );
}

#[gpui::test]
fn hiding_the_only_shown_item_removes_the_bar(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame())
    );
    cx.update(|cx| ext.set_hidden(cx, "item", true));
    draw(window, cx);
    assert_bar_absent(&mut visual, "the only item is hidden");
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame()),
        "the body grows to the bottom"
    );
    cx.update(|cx| ext.set_hidden(cx, "item", false));
    draw(window, cx);
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame()),
        "the bar is back"
    );
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame_with_bar())
    );
    cx.update(|cx| ext.remove(cx, "item"));
    draw(window, cx);
    assert_bar_absent(&mut visual, "the only item is removed");
    assert_eq!(node_bounds(&mut visual, "body"), Some(body_frame()));
}

#[gpui::test]
fn the_bar_stays_while_an_item_of_either_slot_is_shown(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "in_left", "tour.item", "status_bar", "left");
        ext.place(cx, "in_right", "tour.item", "status_bar", "right");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame())
    );
    cx.update(|cx| ext.set_hidden(cx, "in_left", true));
    draw(window, cx);
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame()),
        "an item of right keeps the bar drawn"
    );
    cx.update(|cx| ext.set_hidden(cx, "in_right", true));
    draw(window, cx);
    assert_bar_absent(&mut visual, "no item of either slot is shown");
    assert_eq!(node_bounds(&mut visual, "body"), Some(body_frame()));
    cx.update(|cx| ext.set_hidden(cx, "in_right", false));
    draw(window, cx);
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame()),
        "the bar is back with the shown item"
    );
}

#[gpui::test]
fn with_the_left_item_hidden_the_right_item_stays_at_the_edge(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "left1", "tour.item", "status_bar", "left");
        ext.place(cx, "right1", "tour.item", "status_bar", "right");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let bar = status_bar_frame();
    let before = node_bounds(&mut visual, "right1").expect("the right item is drawn");
    cx.update(|cx| ext.set_hidden(cx, "left1", true));
    draw(window, cx);
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(bar),
        "the shown right item keeps the bar drawn"
    );
    assert_eq!(node_bounds(&mut visual, "left1"), None);
    let right1 = node_bounds(&mut visual, "right1").expect("the right item is drawn");
    assert_eq!(right1, before, "the right item did not move");
    assert!(
        right1.right() <= bar.right() && bar.right() - right1.right() <= gpui::px(8.),
        "{right1:?} ends at the right edge of {bar:?}"
    );
}

#[gpui::test]
fn a_click_on_a_button_in_the_status_bar_calls_its_on_click(cx: &mut TestAppContext) {
    let clicks = Rc::new(Cell::new(0));
    let (_, window) = open_app(cx, |cx, ext| {
        let clicks = clicks.clone();
        ext.register(cx, "tour.button", &[], move |node, _, _, cx| {
            let clicks = clicks.clone();
            cx.new(|_| Ping {
                node: node.to_string(),
                clicks,
            })
            .into()
        });
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "ping", "tour.button", "status_bar", "right");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    click(&mut visual, "BUTTON-ping");
    assert_eq!(clicks.get(), 1, "the button's on_click ran once");
    draw(window, cx);
    click(&mut visual, "BUTTON-ping");
    assert_eq!(clicks.get(), 2, "each click counts once");
}

#[gpui::test]
fn a_zoomed_card_fills_the_body_and_the_status_bar_stays(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "b", "tour.card", "root", "children");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
        ext.set_zoomed(cx, "a");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        visual.update(|_, cx| yard_shell::zoomed(cx)).as_deref(),
        Some("a")
    );
    let bar = node_bounds(&mut visual, "status_bar").expect("the bar is still drawn");
    assert_eq!(
        bar,
        status_bar_frame(),
        "the status bar is still drawn below the zoomed node"
    );
    let item = node_bounds(&mut visual, "item").expect("the item is drawn");
    assert_inside(bar, item);
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame_with_bar())
    );
    assert_eq!(
        node_bounds(&mut visual, "a"),
        Some(body_frame_with_bar()),
        "the zoomed card fills the body"
    );
    assert_eq!(node_bounds(&mut visual, "root"), None);
    assert_eq!(node_bounds(&mut visual, "b"), None);
}

#[gpui::test]
fn zooming_a_status_bar_item_is_refused(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let item_before = node_bounds(&mut visual, "item").expect("the item is drawn in the bar");
    cx.update(|cx| ext.set_zoomed(cx, "item"));
    draw(window, cx);
    assert_eq!(
        visual.update(|_, cx| yard_shell::zoomed(cx)),
        None,
        "only a node in the body can be zoomed"
    );
    assert_eq!(
        node_bounds(&mut visual, "item"),
        Some(item_before),
        "the item stays drawn in the bar"
    );
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame())
    );
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame_with_bar())
    );
}

#[gpui::test]
fn an_item_moves_between_the_status_bar_the_title_bar_and_the_body(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let in_bar = node_bounds(&mut visual, "item").expect("the item is drawn in the bar");
    assert_inside(status_bar_frame(), in_bar);
    cx.update(|cx| ext.move_node(cx, "item", "title_bar", "left", 0));
    draw(window, cx);
    let in_title = node_bounds(&mut visual, "item").expect("the item is drawn in the title bar");
    assert_inside(title_bar_frame(), in_title);
    cx.update(|cx| ext.move_node(cx, "item", "status_bar", "left", 0));
    draw(window, cx);
    assert_eq!(
        node_bounds(&mut visual, "item"),
        Some(in_bar),
        "the item is back in the status bar"
    );
    cx.update(|cx| ext.move_node(cx, "item", "root", "children", 0));
    draw(window, cx);
    let in_body = node_bounds(&mut visual, "item").expect("the item is drawn in the body");
    assert_inside(body_frame(), in_body);
    assert_bar_absent(&mut visual, "the only item is in the body");
    cx.update(|cx| ext.move_node(cx, "item", "status_bar", "left", 0));
    draw(window, cx);
    assert_eq!(
        node_bounds(&mut visual, "item"),
        Some(in_bar),
        "the item is back in the status bar"
    );
}

#[gpui::test]
fn the_frame_and_the_root_cannot_be_changed(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let before = drawn_bounds(
        &mut visual,
        &[
            "window",
            "title_bar",
            "body",
            "status_bar",
            "root",
            "a",
            "item",
        ],
    );
    assert!(before.values().all(Option::is_some));
    for node in ["window", "title_bar", "body", "status_bar"] {
        cx.update(|cx| ext.remove(cx, node));
        draw(window, cx);
        assert_unchanged(&mut visual, &before, &format!("remove {node}"));
        cx.update(|cx| ext.move_node(cx, node, "root", "children", 0));
        draw(window, cx);
        assert_unchanged(&mut visual, &before, &format!("move {node}"));
        cx.update(|cx| ext.set_hidden(cx, node, true));
        draw(window, cx);
        assert_unchanged(&mut visual, &before, &format!("hide {node}"));
        cx.update(|cx| ext.set_hidden(cx, node, false));
        draw(window, cx);
        assert_unchanged(&mut visual, &before, &format!("show {node}"));
        cx.update(|cx| ext.set_zoomed(cx, node));
        draw(window, cx);
        assert_unchanged(&mut visual, &before, &format!("zoom {node}"));
    }
    cx.update(|cx| ext.remove(cx, "root"));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "remove root");
    cx.update(|cx| ext.move_node(cx, "root", "status_bar", "left", 0));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "move root");
    cx.update(|cx| ext.set_hidden(cx, "root", true));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "hide root");
    cx.update(|cx| ext.set_hidden(cx, "root", false));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "show root");
    cx.update(|cx| ext.set_zoomed(cx, "root"));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "zoom root");
}

#[gpui::test]
fn the_frames_slots_and_components_are_closed(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
        ext.place(cx, "p_in_bar", "tour.card", "window", "title_bar");
        ext.place(cx, "p_in_body", "tour.card", "window", "body");
        ext.place(cx, "p_in_status", "tour.card", "window", "status_bar");
        ext.place(cx, "p_in_children", "tour.card", "body", "children");
        ext.place(cx, "p_window", "yard.window", "root", "children");
        ext.place(cx, "p_bar", "yard.title_bar", "root", "children");
        ext.place(cx, "p_body", "yard.body", "root", "children");
        ext.place(cx, "p_status", "yard.status_bar", "root", "children");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    for refused in [
        "p_in_bar",
        "p_in_body",
        "p_in_status",
        "p_in_children",
        "p_window",
        "p_bar",
        "p_body",
        "p_status",
    ] {
        assert_eq!(
            node_bounds(&mut visual, refused),
            None,
            "startup placement {refused} was refused"
        );
    }
    let before = drawn_bounds(
        &mut visual,
        &[
            "window",
            "title_bar",
            "body",
            "status_bar",
            "root",
            "a",
            "item",
        ],
    );
    assert!(before.values().all(Option::is_some));
    for (node, component, parent, slot) in [
        ("a_in_bar", "tour.card", "window", "title_bar"),
        ("a_in_body", "tour.card", "window", "body"),
        ("a_in_status", "tour.card", "window", "status_bar"),
        ("a_in_children", "tour.card", "body", "children"),
        ("a_window", "yard.window", "root", "children"),
        ("a_bar", "yard.title_bar", "root", "children"),
        ("a_body", "yard.body", "root", "children"),
        ("a_status", "yard.status_bar", "root", "children"),
    ] {
        gpui::AnyWindowHandle::from(window)
            .update(cx, |_, window, cx| {
                ext.add(window, cx, node, component, parent, slot);
            })
            .unwrap();
        draw(window, cx);
        assert_eq!(
            node_bounds(&mut visual, node),
            None,
            "add {node} was refused"
        );
        assert_unchanged(&mut visual, &before, &format!("add {node}"));
    }
    for (parent, slot) in [
        ("window", "title_bar"),
        ("window", "body"),
        ("window", "status_bar"),
        ("body", "children"),
    ] {
        cx.update(|cx| ext.move_node(cx, "a", parent, slot, 0));
        draw(window, cx);
        assert_unchanged(
            &mut visual,
            &before,
            &format!("move a into {parent}.{slot}"),
        );
    }
}

#[gpui::test]
fn the_frame_ids_are_taken_and_a_second_root_is_refused(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
        ext.place(cx, "window", "tour.card", "root", "children");
        ext.place(cx, "title_bar", "tour.card", "root", "children");
        ext.place(cx, "body", "tour.card", "root", "children");
        ext.place(cx, "status_bar", "tour.card", "root", "children");
        ext.place_root(cx, "second", "tour.card");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    for selector in [
        "CARD-window",
        "CARD-title_bar",
        "CARD-body",
        "CARD-status_bar",
        "CARD-second",
    ] {
        assert!(
            visual.debug_bounds(selector).is_none(),
            "{selector} was refused"
        );
    }
    assert_eq!(node_bounds(&mut visual, "window"), Some(window_frame()));
    assert_eq!(
        node_bounds(&mut visual, "title_bar"),
        Some(title_bar_frame())
    );
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame())
    );
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame_with_bar())
    );
    assert_eq!(node_bounds(&mut visual, "second"), None);
}

#[gpui::test]
fn a_saved_layout_cannot_change_the_frame(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let layout_path = temp.0.join("layout.json");
    fs::write(
        &layout_path,
        r#"{
  "version": 1,
  "operations": [
    {"op": "move", "node": "a", "parent": "body", "slot": "children", "index": 0},
    {"op": "move", "node": "a", "parent": "window", "slot": "status_bar", "index": 0},
    {"op": "add", "node": "planted", "component": "tour.card", "extension": "tour", "parent": "window", "slot": "status_bar"},
    {"op": "hide", "node": "status_bar"},
    {"op": "hide", "node": "window"},
    {"op": "remove", "node": "status_bar"},
    {"op": "remove", "node": "body"}
  ]
}"#,
    )
    .unwrap();
    let (_, window) = open_app(cx, |cx, ext| {
        yard_shell::set_layout_file(cx, layout_path);
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(node_bounds(&mut visual, "window"), Some(window_frame()));
    assert_eq!(
        node_bounds(&mut visual, "title_bar"),
        Some(title_bar_frame())
    );
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame())
    );
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame_with_bar())
    );
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame_with_bar())
    );
    let a = node_bounds(&mut visual, "a").expect("a stays in the root");
    assert_inside(body_frame_with_bar(), a);
    assert_eq!(
        node_bounds(&mut visual, "planted"),
        None,
        "an add into the frame is skipped"
    );
    assert_eq!(
        cx.update(|cx| yard_shell::user_layer_len(cx)),
        0,
        "every frame change was skipped"
    );
}

#[gpui::test]
fn a_saved_layout_cannot_move_show_or_recreate_the_status_bar(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let layout_path = temp.0.join("layout.json");
    fs::write(
        &layout_path,
        r#"{
  "version": 1,
  "operations": [
    {"op": "move", "node": "status_bar", "parent": "root", "slot": "children", "index": 0},
    {"op": "show", "node": "status_bar"},
    {"op": "add", "node": "planted", "component": "yard.status_bar", "extension": "tour", "parent": "root", "slot": "children"}
  ]
}"#,
    )
    .unwrap();
    let (_, window) = open_app(cx, |cx, ext| {
        yard_shell::set_layout_file(cx, layout_path);
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(node_bounds(&mut visual, "window"), Some(window_frame()));
    assert_eq!(
        node_bounds(&mut visual, "status_bar"),
        Some(status_bar_frame()),
        "the status bar was not moved, shown, or replaced"
    );
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame_with_bar())
    );
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame_with_bar())
    );
    let a = node_bounds(&mut visual, "a").expect("a stays in the root");
    assert_inside(body_frame_with_bar(), a);
    let item = node_bounds(&mut visual, "item").expect("the item is drawn in the bar");
    assert_inside(status_bar_frame(), item);
    assert_eq!(
        node_bounds(&mut visual, "planted"),
        None,
        "an add of a frame component is skipped"
    );
    assert_eq!(
        cx.update(|cx| yard_shell::user_layer_len(cx)),
        0,
        "every frame change was skipped"
    );
}

#[gpui::test]
fn a_saved_layout_of_extension_changes_restores(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let layout_path = temp.0.join("layout.json");
    fs::write(
        &layout_path,
        r#"{
  "version": 1,
  "operations": [
    {"op": "hide", "node": "item"}
  ]
}"#,
    )
    .unwrap();
    let (_, window) = open_app(cx, |cx, ext| {
        yard_shell::set_layout_file(cx, layout_path);
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        cx.update(|cx| yard_shell::user_layer_len(cx)),
        1,
        "the extension's change is restored"
    );
    assert!(
        cx.update(|cx| yard_shell::is_hidden(cx, "item")),
        "the saved hide is applied"
    );
    assert_eq!(node_bounds(&mut visual, "item"), None);
    assert_bar_absent(&mut visual, "the only item is restored hidden");
    assert_eq!(
        node_bounds(&mut visual, "body"),
        Some(body_frame()),
        "the body reaches the bottom"
    );
}

#[gpui::test]
fn the_frame_nodes_have_no_focus_handle(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    for node in ["window", "title_bar", "body", "status_bar"] {
        assert!(
            visual.update(|_, cx| yard_shell::focus_handle(cx, node).is_none()),
            "{node} has a focus handle"
        );
    }
    for node in ["root", "item"] {
        assert!(
            visual.update(|_, cx| yard_shell::focus_handle(cx, node).is_some()),
            "{node} lost its focus handle"
        );
    }
}

#[gpui::test]
fn focus_by_direction_reaches_a_bar_item_and_never_the_frame(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.register(cx, "tour.wide_item", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                width: Some(gpui::px(500.)),
                height: Some(gpui::px(ITEM_HEIGHT)),
            })
            .into()
        });
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.wide_item", "status_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    click(&mut visual, "CARD-item");
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("item"));
    click(&mut visual, "CARD-root");
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("root"));
    visual.dispatch_action(FocusDown);
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("item"));
    visual.dispatch_action(FocusUp);
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("root"));
}

#[gpui::test]
fn focus_by_direction_never_focuses_the_empty_frame(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    click(&mut visual, "CARD-root");
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("root"));
    visual.dispatch_action(FocusDown);
    draw(window, cx);
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("root"),
        "the frame is never focused"
    );
    visual.dispatch_action(FocusUp);
    draw(window, cx);
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("root"),
        "the frame is never focused"
    );
}
