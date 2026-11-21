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
const BAR_HEIGHT: f32 = if cfg!(windows) { 32. } else { 34. };
const ITEM_WIDTH: f32 = 40.;

fn window_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT)),
    )
}

fn bar_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(WIDTH), gpui::px(BAR_HEIGHT)),
    )
}

fn body_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(BAR_HEIGHT)),
        gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT - BAR_HEIGHT)),
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
            "yard-shell-title-bar-{}-{}-{nanos:x}",
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
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let card = gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full();
        match self.width {
            Some(width) => card.w(width),
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

/// The drawn bounds of the title text `Tour`, if the bar shows it. The bar's
/// title label carries the selector `WINDOW-TITLE-<title>` so the drawn text is
/// checked on screen.
fn drawn_title(visual: &mut gpui::VisualTestContext) -> Option<gpui::Bounds<gpui::Pixels>> {
    visual.debug_bounds("WINDOW-TITLE-Tour")
}

#[test]
fn titlebar_options_open_a_transparent_bar_with_moved_traffic_lights() {
    let options = yard_ui::titlebar_options("yard");
    assert_eq!(options.title.as_deref(), Some("yard"));
    assert!(options.appears_transparent);
    assert_eq!(
        options.traffic_light_position,
        Some(gpui::point(gpui::px(9.), gpui::px(9.)))
    );
}

#[gpui::test]
fn the_bar_sits_on_top_and_the_body_and_root_fill_below(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(node_bounds(&mut visual, "window"), Some(window_frame()));
    assert_eq!(node_bounds(&mut visual, "title_bar"), Some(bar_frame()));
    assert_eq!(node_bounds(&mut visual, "body"), Some(body_frame()));
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame()),
        "the extension's root fills the body"
    );
}

#[gpui::test]
fn the_window_title_is_drawn_in_the_bar_only_while_left_is_empty(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        yard_shell::set_window_title(cx, "Tour");
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.card", "title_bar", "left");
        ext.set_hidden(cx, "item", true);
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let bar = node_bounds(&mut visual, "title_bar").expect("the bar is drawn");
    let title = drawn_title(&mut visual).expect("the text Tour is drawn in the bar");
    assert_inside(bar, title);
    assert!(
        title.center().x < bar.center().x,
        "the title is in the bar's left part: {title:?} in {bar:?}"
    );
    cx.update(|cx| ext.set_hidden(cx, "item", false));
    draw(window, cx);
    assert_eq!(
        drawn_title(&mut visual),
        None,
        "a shown item replaces the title"
    );
    cx.update(|cx| ext.set_hidden(cx, "item", true));
    draw(window, cx);
    assert!(
        drawn_title(&mut visual).is_some(),
        "a hidden item shows the title again"
    );
    cx.update(|cx| ext.set_hidden(cx, "item", false));
    draw(window, cx);
    assert_eq!(
        drawn_title(&mut visual),
        None,
        "a shown item hides the title"
    );
}

#[gpui::test]
fn bar_items_are_drawn_inside_the_bar_in_slot_order(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.register(cx, "tour.item", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                width: Some(gpui::px(ITEM_WIDTH)),
            })
            .into()
        });
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "bar_left", "tour.item", "title_bar", "left");
        ext.place(cx, "bar_center", "tour.item", "title_bar", "center");
    });
    // `add` places a bar item after the window is open, as `place` does before.
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| {
            ext.add(window, cx, "bar_right", "tour.item", "title_bar", "right");
        })
        .unwrap();
    draw(window, cx);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let bar = node_bounds(&mut visual, "title_bar").expect("title bar is drawn");
    let left = node_bounds(&mut visual, "bar_left").expect("left item is drawn");
    let center = node_bounds(&mut visual, "bar_center").expect("center item is drawn");
    let right = node_bounds(&mut visual, "bar_right").expect("right item is drawn");
    for (slot, item) in [("left", left), ("center", center), ("right", right)] {
        assert_inside(bar, item);
        assert_eq!(
            item.size.width,
            gpui::px(ITEM_WIDTH),
            "the {slot} item keeps its own width"
        );
    }
    assert!(
        left.right() <= center.left(),
        "{left:?} overlaps or is right of {center:?}"
    );
    assert!(
        center.right() <= right.left(),
        "{center:?} overlaps or is right of {right:?}"
    );
    let middle = bar.center().x;
    assert!(
        center.center().x >= middle - gpui::px(10.) && center.center().x <= middle + gpui::px(10.),
        "{center:?} is in the middle of {bar:?}"
    );
    if cfg!(target_os = "macos") {
        // The left of the bar keeps room for the traffic lights.
        assert!(
            left.left() >= gpui::px(70.),
            "{left:?} keeps room for the traffic lights"
        );
    }
}

#[gpui::test]
fn a_click_on_a_button_in_the_bar_calls_its_on_click(cx: &mut TestAppContext) {
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
        ext.place(cx, "ping", "tour.button", "title_bar", "right");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    click(&mut visual, "BUTTON-ping");
    assert_eq!(clicks.get(), 1, "the button's on_click ran once");
    draw(window, cx);
    click(&mut visual, "BUTTON-ping");
    assert_eq!(clicks.get(), 2, "each click counts once");
}

#[gpui::test]
fn a_zoomed_card_fills_the_body_and_the_bar_stays(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "b", "tour.card", "root", "children");
        ext.set_zoomed(cx, "a");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        visual.update(|_, cx| yard_shell::zoomed(cx)).as_deref(),
        Some("a")
    );
    assert_eq!(
        node_bounds(&mut visual, "title_bar"),
        Some(bar_frame()),
        "the title bar is still drawn"
    );
    assert_eq!(node_bounds(&mut visual, "body"), Some(body_frame()));
    assert_eq!(
        node_bounds(&mut visual, "a"),
        Some(body_frame()),
        "the zoomed card fills the body"
    );
    assert_eq!(node_bounds(&mut visual, "root"), None);
    assert_eq!(node_bounds(&mut visual, "b"), None);
}

#[gpui::test]
fn zooming_a_bar_item_is_refused(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.card", "title_bar", "left");
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
        "the item stays drawn in the bar, once"
    );
    assert_eq!(node_bounds(&mut visual, "title_bar"), Some(bar_frame()));
    assert_eq!(node_bounds(&mut visual, "root"), Some(body_frame()));
}

#[gpui::test]
fn a_bar_item_moves_between_the_bar_and_the_body(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "item", "tour.card", "title_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let in_bar = node_bounds(&mut visual, "item").expect("the item is drawn in the bar");
    assert_inside(bar_frame(), in_bar);
    cx.update(|cx| ext.move_node(cx, "item", "root", "children", 0));
    draw(window, cx);
    let in_body = node_bounds(&mut visual, "item").expect("the item is drawn in the body");
    assert_inside(body_frame(), in_body);
    cx.update(|cx| ext.move_node(cx, "item", "title_bar", "left", 0));
    draw(window, cx);
    assert_eq!(
        node_bounds(&mut visual, "item"),
        Some(in_bar),
        "the item is back in the bar"
    );
}

#[gpui::test]
fn the_frame_and_the_root_cannot_be_changed(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let before = drawn_bounds(&mut visual, &["window", "title_bar", "body", "root", "a"]);
    assert!(before.values().all(Option::is_some));
    for node in ["window", "title_bar", "body"] {
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
    cx.update(|cx| ext.move_node(cx, "root", "title_bar", "left", 0));
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
        ext.place(cx, "p_in_bar", "tour.card", "window", "title_bar");
        ext.place(cx, "p_in_body", "tour.card", "window", "body");
        ext.place(cx, "p_in_children", "tour.card", "body", "children");
        ext.place(cx, "p_window", "yard.window", "root", "children");
        ext.place(cx, "p_bar", "yard.title_bar", "root", "children");
        ext.place(cx, "p_body", "yard.body", "root", "children");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    for refused in [
        "p_in_bar",
        "p_in_body",
        "p_in_children",
        "p_window",
        "p_bar",
        "p_body",
    ] {
        assert_eq!(
            node_bounds(&mut visual, refused),
            None,
            "startup placement {refused} was refused"
        );
    }
    let before = drawn_bounds(&mut visual, &["window", "title_bar", "body", "root", "a"]);
    assert!(before.values().all(Option::is_some));
    for (node, component, parent, slot) in [
        ("a_in_bar", "tour.card", "window", "title_bar"),
        ("a_in_body", "tour.card", "window", "body"),
        ("a_in_children", "tour.card", "body", "children"),
        ("a_window", "yard.window", "root", "children"),
        ("a_bar", "yard.title_bar", "root", "children"),
        ("a_body", "yard.body", "root", "children"),
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
fn the_root_stays_the_root(cx: &mut TestAppContext) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "inner", "yard.row", "root", "children");
        ext.place(cx, "a", "tour.card", "inner", "children");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let before = drawn_bounds(
        &mut visual,
        &["window", "title_bar", "body", "root", "inner", "a"],
    );
    assert!(before.values().all(Option::is_some));
    assert_eq!(
        before["root"],
        Some(body_frame()),
        "the root fills the body"
    );
    cx.update(|cx| ext.move_node(cx, "a", "body", "children", 0));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "move a into body.children");
    cx.update(|cx| ext.remove(cx, "root"));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "remove root");
    assert_eq!(node_bounds(&mut visual, "root"), Some(body_frame()));
    cx.update(|cx| ext.set_hidden(cx, "root", true));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "hide root");
    assert_eq!(node_bounds(&mut visual, "root"), Some(body_frame()));
    cx.update(|cx| ext.move_node(cx, "root", "title_bar", "left", 0));
    draw(window, cx);
    assert_unchanged(&mut visual, &before, "move root");
    assert_eq!(node_bounds(&mut visual, "root"), Some(body_frame()));
}

#[gpui::test]
fn the_frame_ids_are_taken_and_a_second_root_is_refused(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "window", "tour.card", "root", "children");
        ext.place(cx, "title_bar", "tour.card", "root", "children");
        ext.place(cx, "body", "tour.card", "root", "children");
        ext.place_root(cx, "second", "tour.card");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    for selector in ["CARD-window", "CARD-title_bar", "CARD-body", "CARD-second"] {
        assert!(
            visual.debug_bounds(selector).is_none(),
            "{selector} was refused"
        );
    }
    assert_eq!(node_bounds(&mut visual, "window"), Some(window_frame()));
    assert_eq!(node_bounds(&mut visual, "title_bar"), Some(bar_frame()));
    assert_eq!(node_bounds(&mut visual, "body"), Some(body_frame()));
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
    {"op": "move", "node": "a", "parent": "window", "slot": "title_bar", "index": 0},
    {"op": "add", "node": "planted", "component": "tour.card", "extension": "tour", "parent": "body", "slot": "children"},
    {"op": "hide", "node": "title_bar"},
    {"op": "hide", "node": "window"},
    {"op": "remove", "node": "body"}
  ]
}"#,
    )
    .unwrap();
    let (_, window) = open_app(cx, |cx, ext| {
        yard_shell::set_layout_file(cx, layout_path);
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(node_bounds(&mut visual, "window"), Some(window_frame()));
    assert_eq!(node_bounds(&mut visual, "title_bar"), Some(bar_frame()));
    assert_eq!(node_bounds(&mut visual, "body"), Some(body_frame()));
    assert_eq!(node_bounds(&mut visual, "root"), Some(body_frame()));
    let a = node_bounds(&mut visual, "a").expect("a stays in the root");
    assert_inside(body_frame(), a);
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
fn a_saved_layout_of_extension_changes_restores(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let layout_path = temp.0.join("layout.json");
    fs::write(
        &layout_path,
        r#"{
  "version": 1,
  "operations": [
    {"op": "hide", "node": "a"},
    {"op": "move", "node": "c", "parent": "root", "slot": "children", "index": 0}
  ]
}"#,
    )
    .unwrap();
    let (_, window) = open_app(cx, |cx, ext| {
        yard_shell::set_layout_file(cx, layout_path);
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
        ext.place(cx, "b", "tour.card", "root", "children");
        ext.place(cx, "c", "tour.card", "root", "children");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        cx.update(|cx| yard_shell::user_layer_len(cx)),
        2,
        "the extension's changes are restored"
    );
    assert!(
        cx.update(|cx| yard_shell::is_hidden(cx, "a")),
        "the saved hide is applied"
    );
    assert_eq!(node_bounds(&mut visual, "a"), None, "a stays hidden");
    let b = node_bounds(&mut visual, "b").expect("b is drawn");
    let c = node_bounds(&mut visual, "c").expect("c is drawn");
    assert!(
        c.right() <= b.left(),
        "c was moved ahead of b: {c:?} vs {b:?}"
    );
    assert_inside(body_frame(), b);
    assert_inside(body_frame(), c);
}

#[gpui::test]
fn the_frame_nodes_have_no_focus_handle(cx: &mut TestAppContext) {
    let (_, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.card", "title_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    for node in ["window", "title_bar", "body"] {
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
        ext.register(cx, "tour.wide_card", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                width: Some(gpui::px(500.)),
            })
            .into()
        });
        ext.place_root(cx, "root", "tour.card");
        ext.place(cx, "item", "tour.wide_card", "title_bar", "left");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    click(&mut visual, "CARD-item");
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("item"));
    click(&mut visual, "CARD-root");
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("root"));
    visual.dispatch_action(FocusUp);
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("item"));
    visual.dispatch_action(FocusDown);
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
    visual.dispatch_action(FocusUp);
    draw(window, cx);
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("root"),
        "the frame is never focused"
    );
}
