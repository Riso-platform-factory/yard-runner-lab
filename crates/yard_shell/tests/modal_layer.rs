use gpui::{AppContext, InteractiveElement, IntoElement, ParentElement, Styled, TestAppContext};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use yard_shell::Extension;

const WIDTH: f32 = 800.;
const HEIGHT: f32 = 600.;
// The title bar's height on each platform, as Zed's `platform_title_bar_height`
// gives it: 32 px on Windows, and at least 34 px elsewhere.
const TITLE_BAR: f32 = if cfg!(windows) { 32. } else { 34. };
// 5 rems at the default 16px rem.
const MODAL_TOP: f32 = 80.;
const MODAL_WIDTH: f32 = 240.;
const MODAL_HEIGHT: f32 = 120.;

fn window_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT)),
    )
}

fn body_frame() -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(gpui::px(0.), gpui::px(TITLE_BAR)),
        gpui::size(gpui::px(WIDTH), gpui::px(HEIGHT - TITLE_BAR)),
    )
}

struct Card {
    node: String,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
    }
}

struct Ping {
    node: String,
    clicks: Rc<Cell<usize>>,
}

impl gpui::Render for Ping {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let clicks = self.clicks.clone();
        gpui::div().flex().flex_row().items_start().child(
            gpui::div()
                .debug_selector(|| format!("BUTTON-{}", self.node))
                .child(
                    yard_ui::Button::new("ping", "ping")
                        .size(yard_ui::ButtonSize::Compact)
                        .on_click(move |_, _| clicks.set(clicks.get() + 1)),
                ),
        )
    }
}

/// A full-size element that counts the left mouse-downs it receives.
struct Pad {
    node: String,
    downs: Rc<Cell<usize>>,
}

impl gpui::Render for Pad {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let downs = self.downs.clone();
        gpui::div()
            .debug_selector(|| format!("PAD-{}", self.node))
            .size_full()
            .on_mouse_down(gpui::MouseButton::Left, move |_, _, _| {
                downs.set(downs.get() + 1)
            })
    }
}

/// The surface every test modal draws: a fixed-size box that is its own
/// focusable element, holding a close button that emits `DismissEvent` on the
/// modal's view. The selector is on the modal's outermost element, and
/// `<selector>-CLOSE` hugs the close button.
fn surface<V: gpui::EventEmitter<gpui::DismissEvent> + 'static>(
    selector: &'static str,
    focus: &gpui::FocusHandle,
    view: gpui::Entity<V>,
) -> gpui::Div {
    gpui::div()
        .debug_selector(move || selector.to_string())
        .track_focus(focus)
        .w(gpui::px(MODAL_WIDTH))
        .h(gpui::px(MODAL_HEIGHT))
        .child(
            gpui::div()
                .debug_selector(move || format!("{selector}-CLOSE"))
                .child(
                    yard_ui::Button::new("close", "close")
                        .size(yard_ui::ButtonSize::Compact)
                        .on_click(move |_, cx| {
                            view.update(cx, |_, cx| cx.emit(gpui::DismissEvent))
                        }),
                ),
        )
}

struct TestModal {
    focus: gpui::FocusHandle,
}

impl TestModal {
    fn new(cx: &mut gpui::Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

impl gpui::EventEmitter<gpui::DismissEvent> for TestModal {}
impl gpui::Focusable for TestModal {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}
impl yard_ui::ModalView for TestModal {}
impl gpui::Render for TestModal {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        surface("MODAL-TEST", &self.focus, cx.entity())
    }
}

struct OtherModal {
    focus: gpui::FocusHandle,
}

impl OtherModal {
    fn new(cx: &mut gpui::Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

impl gpui::EventEmitter<gpui::DismissEvent> for OtherModal {}
impl gpui::Focusable for OtherModal {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}
impl yard_ui::ModalView for OtherModal {}
impl gpui::Render for OtherModal {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        surface("MODAL-OTHER", &self.focus, cx.entity())
    }
}

struct DimmedModal {
    focus: gpui::FocusHandle,
}

impl DimmedModal {
    fn new(cx: &mut gpui::Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

impl gpui::EventEmitter<gpui::DismissEvent> for DimmedModal {}
impl gpui::Focusable for DimmedModal {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}
impl yard_ui::ModalView for DimmedModal {
    fn dims_background(&self) -> bool {
        true
    }
}
impl gpui::Render for DimmedModal {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        surface("MODAL-DIMMED", &self.focus, cx.entity())
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

/// Opens an 800×600 dev-shell window over the layout that `place` builds, with
/// the default keys bound as the startup binds them.
fn open_app(
    cx: &mut TestAppContext,
    place: impl FnOnce(&mut gpui::App, &Extension),
) -> (Extension, gpui::WindowHandle<yard_shell::Shell>) {
    cx.update(yard_ui::init);
    let ext = cx.update(|cx| {
        // The startup binds the default keys in `app-init`, before extensions run.
        yard_shell::bind_default_keys(cx);
        let ext = Extension::new(cx, "tour").unwrap();
        ext.register(cx, "tour.card", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
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

/// The node the dev shell says has the focus, if any.
fn focused(visual: &mut gpui::VisualTestContext) -> Option<String> {
    visual.update(|window, cx| yard_shell::focused_node(window, cx).map(|node| node.to_string()))
}

/// Whether the dev shell's layer shows a modal.
fn has_modal(visual: &mut gpui::VisualTestContext) -> bool {
    visual.update(|window, cx| yard_shell::has_modal(window, cx))
}

/// Clicks the center of the element with `selector`.
fn click(visual: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} has bounds"));
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
}

/// Shows a modal of type `V` through the extension.
fn show<V: yard_ui::ModalView>(
    ext: &Extension,
    window: gpui::WindowHandle<yard_shell::Shell>,
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut gpui::Window, &mut gpui::Context<V>) -> V,
) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| ext.toggle_modal::<V>(window, cx, build))
        .unwrap();
}

/// Closes the open modal through the extension.
fn hide(ext: &Extension, window: gpui::WindowHandle<yard_shell::Shell>, cx: &mut TestAppContext) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| ext.hide_modal(window, cx))
        .unwrap();
}

/// Registers a `tour.button` component that counts its clicks into `clicks`.
fn register_button(ext: &Extension, cx: &mut gpui::App, clicks: Rc<Cell<usize>>) {
    ext.register(cx, "tour.button", &[], move |node, _, _, cx| {
        let clicks = clicks.clone();
        cx.new(|_| Ping {
            node: node.to_string(),
            clicks,
        })
        .into()
    });
}

/// The bounds of the element with `selector` in the last draw.
fn bounds_of(
    visual: &mut gpui::VisualTestContext,
    selector: &'static str,
) -> gpui::Bounds<gpui::Pixels> {
    visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} has bounds"))
}

/// The modal's element sits inside the window, centred horizontally, its top
/// 5 rems below the window's top.
fn assert_positioned(visual: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = bounds_of(visual, selector);
    assert!(
        window_frame().contains(&bounds.origin) && window_frame().contains(&bounds.bottom_right()),
        "{bounds:?} is not inside the window"
    );
    assert_eq!(
        bounds.size,
        gpui::size(gpui::px(MODAL_WIDTH), gpui::px(MODAL_HEIGHT)),
        "the modal keeps its own size"
    );
    assert_eq!(
        bounds.top(),
        gpui::px(MODAL_TOP),
        "the modal's top is 5 rems below the window's top"
    );
    assert_eq!(
        bounds.center().x,
        window_frame().center().x,
        "the modal is centred horizontally"
    );
}

/// Opens a `TestModal`, capturing its focus handle into `slot`.
fn show_test_modal(
    ext: &Extension,
    window: gpui::WindowHandle<yard_shell::Shell>,
    cx: &mut TestAppContext,
    slot: Rc<RefCell<Option<gpui::FocusHandle>>>,
) {
    show::<TestModal>(ext, window, cx, move |_, cx| {
        let modal = TestModal::new(cx);
        *slot.borrow_mut() = Some(modal.focus.clone());
        modal
    });
}

/// Opens a `TestModal` over the focused card `a` of a one-card layout.
fn open_over_focused_card(
    cx: &mut TestAppContext,
) -> (
    Extension,
    gpui::WindowHandle<yard_shell::Shell>,
    gpui::VisualTestContext,
) {
    let (ext, window) = open_app(cx, |cx, ext| {
        ext.place_root(cx, "root", "yard.row");
        ext.place(cx, "a", "tour.card", "root", "children");
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    click(&mut visual, "CARD-a");
    draw(window, cx);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
    (ext, window, visual)
}

#[gpui::test]
fn an_open_modal_is_centred_focused_and_reported(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    let slot = Rc::new(RefCell::new(None));
    show_test_modal(&ext, window, cx, slot.clone());
    draw(window, cx);
    assert!(
        has_modal(&mut visual),
        "has_modal is true while a modal is open"
    );
    assert_eq!(
        focused(&mut visual),
        None,
        "the modal holds the focus: no node is focused"
    );
    let modal_focus = slot.borrow().clone().expect("the modal was built");
    assert_eq!(
        visual.update(|window, cx| window.focused(cx)),
        Some(modal_focus),
        "the modal's own focus handle has the focus"
    );
    assert_positioned(&mut visual, "MODAL-TEST");
    assert_eq!(
        bounds_of(&mut visual, "MODAL-TEST-CLOSE").top(),
        gpui::px(MODAL_TOP)
    );
}

#[gpui::test]
fn the_close_button_closes_the_modal_and_restores_the_focus(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_test_modal(&ext, window, cx, Rc::new(RefCell::new(None)));
    draw(window, cx);
    click(&mut visual, "MODAL-TEST-CLOSE");
    draw(window, cx);
    assert!(
        !has_modal(&mut visual),
        "the close button's DismissEvent closed it"
    );
    assert_eq!(visual.debug_bounds("MODAL-TEST"), None);
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("a"),
        "the node focused before the modal is focused again"
    );
}

#[gpui::test]
fn escape_closes_the_modal_and_restores_the_focus(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_test_modal(&ext, window, cx, Rc::new(RefCell::new(None)));
    draw(window, cx);
    visual.simulate_keystrokes("escape");
    draw(window, cx);
    assert!(!has_modal(&mut visual), "escape closed the modal");
    assert_eq!(visual.debug_bounds("MODAL-TEST"), None);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn a_click_outside_closes_it_and_a_click_inside_does_not(cx: &mut TestAppContext) {
    let clicks = Rc::new(Cell::new(0));
    let (ext, window) = open_app(cx, {
        let clicks = clicks.clone();
        move |cx, ext| {
            register_button(ext, cx, clicks);
            ext.place_root(cx, "root", "tour.button");
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    let button = bounds_of(&mut visual, "BUTTON-root");
    let modal = bounds_of(&mut visual, "MODAL-TEST");
    assert!(
        !modal.contains(&button.center()),
        "the button under the click sits outside the modal"
    );
    click(&mut visual, "BUTTON-root");
    draw(window, cx);
    assert!(!has_modal(&mut visual), "a click outside closed the modal");
    assert_eq!(
        clicks.get(),
        0,
        "the click outside did not reach the button under it"
    );
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    let modal = bounds_of(&mut visual, "MODAL-TEST");
    visual.simulate_click(
        gpui::point(modal.center().x, modal.bottom() - gpui::px(8.)),
        gpui::Modifiers::default(),
    );
    draw(window, cx);
    assert!(
        has_modal(&mut visual),
        "a click inside did not close the modal"
    );
    assert_eq!(clicks.get(), 0);
}

#[gpui::test]
fn hide_modal_closes_it_and_without_one_nothing_changes(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    hide(&ext, window, cx);
    draw(window, cx);
    assert!(!has_modal(&mut visual));
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("a"),
        "hide_modal with no modal changes nothing"
    );
    visual.simulate_keystrokes("escape");
    draw(window, cx);
    assert!(!has_modal(&mut visual));
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("a"),
        "escape with no modal changes nothing"
    );
    show_test_modal(&ext, window, cx, Rc::new(RefCell::new(None)));
    draw(window, cx);
    assert!(has_modal(&mut visual));
    hide(&ext, window, cx);
    draw(window, cx);
    assert!(!has_modal(&mut visual), "hide_modal closed the modal");
    assert_eq!(visual.debug_bounds("MODAL-TEST"), None);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn the_same_type_toggles_and_another_type_replaces(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    assert!(has_modal(&mut visual));
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    assert!(
        !has_modal(&mut visual),
        "opening the same type again toggles it closed"
    );
    assert_eq!(visual.debug_bounds("MODAL-TEST"), None);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    assert!(has_modal(&mut visual));
    show::<OtherModal>(&ext, window, cx, |_, cx| OtherModal::new(cx));
    draw(window, cx);
    assert!(
        has_modal(&mut visual),
        "the other type replaced the open modal"
    );
    assert_eq!(
        visual.debug_bounds("MODAL-TEST"),
        None,
        "the replaced modal is gone"
    );
    assert_positioned(&mut visual, "MODAL-OTHER");
    assert_eq!(focused(&mut visual), None);
    hide(&ext, window, cx);
    draw(window, cx);
    assert!(!has_modal(&mut visual));
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("a"),
        "the focus goes back to what had it before the first modal"
    );
}

#[gpui::test]
fn a_dimming_modal_opens_and_closes_as_the_others_do(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show::<DimmedModal>(&ext, window, cx, |_, cx| DimmedModal::new(cx));
    draw(window, cx);
    assert!(has_modal(&mut visual));
    assert_eq!(focused(&mut visual), None);
    assert_positioned(&mut visual, "MODAL-DIMMED");
    hide(&ext, window, cx);
    draw(window, cx);
    assert!(!has_modal(&mut visual));
    assert_eq!(visual.debug_bounds("MODAL-DIMMED"), None);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn a_dismiss_event_from_a_replaced_modal_closes_nothing(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    let retained: Rc<RefCell<Option<gpui::Entity<TestModal>>>> = Rc::new(RefCell::new(None));
    show::<TestModal>(&ext, window, cx, {
        let retained = retained.clone();
        move |_, cx| {
            let modal = TestModal::new(cx);
            *retained.borrow_mut() = Some(cx.entity());
            modal
        }
    });
    draw(window, cx);
    let stale = retained
        .borrow()
        .clone()
        .expect("the first modal was built");
    let other_focus = Rc::new(RefCell::new(None));
    show::<OtherModal>(&ext, window, cx, {
        let other_focus = other_focus.clone();
        move |_, cx| {
            let modal = OtherModal::new(cx);
            *other_focus.borrow_mut() = Some(modal.focus.clone());
            modal
        }
    });
    draw(window, cx);
    assert!(has_modal(&mut visual), "the other type replaced the first");
    stale.update(cx, |_, cx| cx.emit(gpui::DismissEvent));
    draw(window, cx);
    assert!(
        has_modal(&mut visual),
        "a DismissEvent from the replaced modal closes nothing"
    );
    assert_positioned(&mut visual, "MODAL-OTHER");
    assert_eq!(
        visual.update(|window, cx| window.focused(cx)),
        other_focus.borrow().clone(),
        "the open modal still has the focus"
    );
}

#[gpui::test]
fn without_a_modal_the_layer_takes_no_room_and_blocks_nothing(cx: &mut TestAppContext) {
    let clicks = Rc::new(Cell::new(0));
    let (_, window) = open_app(cx, {
        let clicks = clicks.clone();
        move |cx, ext| {
            register_button(ext, cx, clicks);
            ext.place_root(cx, "root", "tour.button");
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame()),
        "the root fills the body: the layer took no room"
    );
    assert!(!has_modal(&mut visual));
    click(&mut visual, "BUTTON-root");
    draw(window, cx);
    assert_eq!(clicks.get(), 1, "the click reached the button");
    click(&mut visual, "BUTTON-root");
    draw(window, cx);
    assert_eq!(clicks.get(), 2, "each click reaches the button");
}

#[gpui::test]
fn the_modal_is_drawn_over_the_title_bar_and_the_status_bar(cx: &mut TestAppContext) {
    let clicks = Rc::new(Cell::new(0));
    let (ext, window) = open_app(cx, {
        let clicks = clicks.clone();
        move |cx, ext| {
            register_button(ext, cx, clicks);
            ext.place_root(cx, "root", "tour.card");
            ext.place(cx, "ping_title", "tour.button", "title_bar", "right");
            ext.place(cx, "ping_status", "tour.button", "status_bar", "left");
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    click(&mut visual, "BUTTON-ping_title");
    draw(window, cx);
    assert!(
        !has_modal(&mut visual),
        "a click on a title bar item closed the modal"
    );
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    click(&mut visual, "BUTTON-ping_status");
    draw(window, cx);
    assert!(
        !has_modal(&mut visual),
        "a click on a status bar item closed the modal"
    );
    assert_eq!(
        clicks.get(),
        0,
        "no click on a bar item reached the item while the modal was open"
    );
}

#[gpui::test]
fn a_click_outside_reaches_no_mouse_down_handler_below(cx: &mut TestAppContext) {
    let downs = Rc::new(Cell::new(0));
    let (ext, window) = open_app(cx, {
        let downs = downs.clone();
        move |cx, ext| {
            ext.register(cx, "tour.pad", &[], move |node, _, _, cx| {
                let downs = downs.clone();
                cx.new(|_| Pad {
                    node: node.to_string(),
                    downs,
                })
                .into()
            });
            ext.place_root(cx, "root", "tour.pad");
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let outside = gpui::point(gpui::px(10.), gpui::px(560.));
    visual.simulate_click(outside, gpui::Modifiers::default());
    draw(window, cx);
    assert_eq!(
        downs.get(),
        1,
        "with no modal open, the click's mouse-down reached the view below"
    );
    show::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    let modal = bounds_of(&mut visual, "MODAL-TEST");
    assert!(
        !modal.contains(&outside),
        "the click point sits outside the modal"
    );
    visual.simulate_click(outside, gpui::Modifiers::default());
    draw(window, cx);
    assert!(
        !has_modal(&mut visual),
        "the click outside closed the modal"
    );
    assert_eq!(
        downs.get(),
        1,
        "its mouse-down reached only the layer, nothing below it"
    );
}
