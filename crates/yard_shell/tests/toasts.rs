use gpui::{AppContext, InteractiveElement, IntoElement, ParentElement, Styled, TestAppContext};
use std::{cell::Cell, rc::Rc, time::Duration};
use yard_shell::Extension;
use yard_ui::{Toast, ToastKind};

const WIDTH: f32 = 800.;
const HEIGHT: f32 = 600.;
// The title bar's height on each platform, as Zed's `platform_title_bar_height`
// gives it: 32 px on Windows, and at least 34 px elsewhere.
const TITLE_BAR: f32 = if cfg!(windows) { 32. } else { 34. };
// The toast's bottom edge rests this far above the window's bottom edge.
const TOAST_LIFT: f32 = 40.;
const TOAST_LIFETIME: Duration = Duration::from_secs(10);
const MINIMUM_RESUME: Duration = Duration::from_millis(800);
// Past the 150 ms entry animation the toast rests at its final place.
const ENTRY_SETTLED: Duration = Duration::from_millis(200);

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

/// A card with two buttons in a row at the toast's height: `beside` at the
/// left and `under` centred, so the toast covers `under` and not `beside`.
struct BottomButtons {
    node: String,
    under: Rc<Cell<usize>>,
    beside: Rc<Cell<usize>>,
}

impl gpui::Render for BottomButtons {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let under = self.under.clone();
        let beside = self.beside.clone();
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .pb(gpui::px(TOAST_LIFT))
            .child(
                gpui::div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .child(
                        gpui::div().w(gpui::px(120.)).flex().flex_row().child(
                            gpui::div()
                                .debug_selector(|| "BUTTON-beside".to_string())
                                .child(
                                    yard_ui::Button::new("beside", "beside")
                                        .size(yard_ui::ButtonSize::Compact)
                                        .on_click(move |_, _| beside.set(beside.get() + 1)),
                                ),
                        ),
                    )
                    .child(
                        gpui::div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .justify_center()
                            .child(
                                gpui::div()
                                    .debug_selector(|| "BUTTON-under".to_string())
                                    .child(
                                        yard_ui::Button::new("under", "under")
                                            .size(yard_ui::ButtonSize::Compact)
                                            .on_click(move |_, _| under.set(under.get() + 1)),
                                    ),
                            ),
                    )
                    .child(gpui::div().w(gpui::px(120.))),
            )
    }
}

/// A fixed-size modal surface, like the modal layer tests use.
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
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| "MODAL-TEST".to_string())
            .track_focus(&self.focus)
            .w(gpui::px(240.))
            .h(gpui::px(120.))
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

/// Whether the dev shell's layer shows a toast.
fn has_toast(visual: &mut gpui::VisualTestContext) -> bool {
    visual.update(|window, cx| yard_shell::has_toast(window, cx))
}

/// Clicks the center of the element with `selector`.
fn click(visual: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = bounds_of(visual, selector);
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
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

/// Shows `toast` through the extension.
fn show_toast(
    ext: &Extension,
    window: gpui::WindowHandle<yard_shell::Shell>,
    cx: &mut TestAppContext,
    toast: Toast,
) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| ext.show_toast(toast, window, cx))
        .unwrap();
}

/// Closes the shown toast through the extension.
fn hide_toast(
    ext: &Extension,
    window: gpui::WindowHandle<yard_shell::Shell>,
    cx: &mut TestAppContext,
) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| ext.hide_toast(window, cx))
        .unwrap();
}

/// Shows a modal of type `V` through the extension.
fn show_modal<V: yard_ui::ModalView>(
    ext: &Extension,
    window: gpui::WindowHandle<yard_shell::Shell>,
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut gpui::Window, &mut gpui::Context<V>) -> V,
) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| ext.toggle_modal::<V>(window, cx, build))
        .unwrap();
}

/// Registers a `tour.buttons` component that counts its buttons' clicks into
/// `under` and `beside`.
fn register_buttons(
    ext: &Extension,
    cx: &mut gpui::App,
    under: Rc<Cell<usize>>,
    beside: Rc<Cell<usize>>,
) {
    ext.register(cx, "tour.buttons", &[], move |node, _, _, cx| {
        let under = under.clone();
        let beside = beside.clone();
        cx.new(|_| BottomButtons {
            node: node.to_string(),
            under,
            beside,
        })
        .into()
    });
}

/// Advances the test clock by `duration` and runs everything it unblocks. The
/// first park lets a just-spawned timer register its deadline before the clock
/// moves.
fn advance(cx: &mut TestAppContext, duration: Duration) {
    cx.run_until_parked();
    cx.executor().advance_clock(duration);
    cx.run_until_parked();
}

/// The toast row is centred horizontally in the window and its bottom edge is
/// 40 px above the window's bottom, both within 1 px.
fn assert_toast_position(visual: &mut gpui::VisualTestContext) {
    let toast = bounds_of(visual, "TOAST");
    assert!(
        (f32::from(toast.center().x) - WIDTH / 2.).abs() <= 1.,
        "the toast is centred horizontally: {toast:?}"
    );
    assert!(
        (f32::from(toast.bottom()) - (HEIGHT - TOAST_LIFT)).abs() <= 1.,
        "the toast's bottom edge is 40 px above the window's bottom: {toast:?}"
    );
}

/// Opens a dev-shell window over the focused card `a` of a one-card layout.
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
fn a_shown_toast_is_centred_40_px_up_and_keeps_the_focus(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Info, "hello"));
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "has_toast is true while a toast is shown"
    );
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("a"),
        "the toast does not take the focus"
    );
    advance(cx, ENTRY_SETTLED);
    draw(window, cx);
    assert_toast_position(&mut visual);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn each_kind_shows_and_only_a_toast_with_a_button_draws_one(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    for (kind, text) in [
        (ToastKind::Info, "information"),
        (ToastKind::Warning, "warning"),
        (ToastKind::Error, "error"),
    ] {
        show_toast(&ext, window, cx, Toast::new(kind, text));
        draw(window, cx);
        assert!(has_toast(&mut visual), "the {text} toast is shown");
        bounds_of(&mut visual, "TOAST");
        assert_eq!(
            visual.debug_bounds("TOAST-ACTION"),
            None,
            "no button was set on the {text} toast"
        );
        hide_toast(&ext, window, cx);
        draw(window, cx);
        assert!(!has_toast(&mut visual));
    }
    show_toast(
        &ext,
        window,
        cx,
        Toast::new(ToastKind::Error, "failed").action("Retry", |_, _| {}),
    );
    draw(window, cx);
    let toast = bounds_of(&mut visual, "TOAST");
    let action = bounds_of(&mut visual, "TOAST-ACTION");
    let dismiss = bounds_of(&mut visual, "TOAST-DISMISS");
    assert!(
        toast.contains(&action.center()) && toast.contains(&dismiss.center()),
        "the buttons are inside the toast row"
    );
    assert!(
        action.center().x < dismiss.center().x,
        "the action sits between the text and the close button"
    );
    hide_toast(&ext, window, cx);
    draw(window, cx);
    assert!(!has_toast(&mut visual));
}

#[gpui::test]
fn the_toast_closes_itself_at_ten_seconds(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Info, "ten seconds"));
    draw(window, cx);
    advance(cx, TOAST_LIFETIME - Duration::from_millis(1));
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "just before 10 s the toast is still shown"
    );
    bounds_of(&mut visual, "TOAST");
    advance(cx, Duration::from_millis(1));
    draw(window, cx);
    assert!(!has_toast(&mut visual), "at 10 s the toast is gone");
    assert_eq!(visual.debug_bounds("TOAST"), None);
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("a"),
        "the focus never moved"
    );
}

#[gpui::test]
fn the_pointer_over_the_toast_pauses_the_timer(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Warning, "hover"));
    draw(window, cx);
    advance(cx, Duration::from_secs(5));
    draw(window, cx);
    let toast = bounds_of(&mut visual, "TOAST");
    visual.simulate_mouse_move(toast.center(), None, gpui::Modifiers::default());
    draw(window, cx);
    advance(cx, TOAST_LIFETIME);
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "the pointer over the toast pauses the timer"
    );
    visual.simulate_mouse_move(
        gpui::point(gpui::px(10.), gpui::px(10.)),
        None,
        gpui::Modifiers::default(),
    );
    draw(window, cx);
    advance(cx, Duration::from_secs(5) - Duration::from_millis(1));
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "just before the time that was left has passed, the toast is still shown"
    );
    advance(cx, Duration::from_millis(1));
    draw(window, cx);
    assert!(
        !has_toast(&mut visual),
        "it closes when the time that was left has passed"
    );
}

#[gpui::test]
fn with_less_than_0_8_seconds_left_the_toast_closes_0_8_seconds_after_the_pointer_leaves(
    cx: &mut TestAppContext,
) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(
        &ext,
        window,
        cx,
        Toast::new(ToastKind::Error, "nearly gone"),
    );
    draw(window, cx);
    advance(cx, TOAST_LIFETIME - Duration::from_millis(500));
    draw(window, cx);
    let toast = bounds_of(&mut visual, "TOAST");
    visual.simulate_mouse_move(toast.center(), None, gpui::Modifiers::default());
    draw(window, cx);
    visual.simulate_mouse_move(
        gpui::point(gpui::px(10.), gpui::px(10.)),
        None,
        gpui::Modifiers::default(),
    );
    draw(window, cx);
    advance(cx, MINIMUM_RESUME - Duration::from_millis(1));
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "just before 0.8 s the toast is still shown"
    );
    advance(cx, Duration::from_millis(1));
    draw(window, cx);
    assert!(!has_toast(&mut visual), "at 0.8 s the toast is gone");
}

#[gpui::test]
fn the_close_button_closes_the_toast(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Info, "close me"));
    draw(window, cx);
    click(&mut visual, "TOAST-DISMISS");
    draw(window, cx);
    assert!(!has_toast(&mut visual), "the close button closed the toast");
    assert_eq!(visual.debug_bounds("TOAST"), None);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn the_action_button_runs_its_handler_once_and_closes_the_toast(cx: &mut TestAppContext) {
    let clicks = Rc::new(Cell::new(0));
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(
        &ext,
        window,
        cx,
        Toast::new(ToastKind::Error, "failed").action("Retry", {
            let clicks = clicks.clone();
            move |_, _| clicks.set(clicks.get() + 1)
        }),
    );
    draw(window, cx);
    click(&mut visual, "TOAST-ACTION");
    draw(window, cx);
    assert_eq!(clicks.get(), 1, "the handler ran once");
    assert!(!has_toast(&mut visual), "the button closed the toast");
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn a_middle_click_closes_the_toast(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Info, "middle"));
    draw(window, cx);
    let toast = bounds_of(&mut visual, "TOAST");
    visual.simulate_mouse_down(
        toast.center(),
        gpui::MouseButton::Middle,
        gpui::Modifiers::default(),
    );
    visual.simulate_mouse_up(
        toast.center(),
        gpui::MouseButton::Middle,
        gpui::Modifiers::default(),
    );
    draw(window, cx);
    assert!(!has_toast(&mut visual), "a middle click closed the toast");
    assert_eq!(visual.debug_bounds("TOAST"), None);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn a_click_on_the_toast_does_not_reach_below_and_a_click_beside_does(cx: &mut TestAppContext) {
    let under = Rc::new(Cell::new(0));
    let beside = Rc::new(Cell::new(0));
    let (ext, window) = open_app(cx, {
        let under = under.clone();
        let beside = beside.clone();
        move |cx, ext| {
            register_buttons(ext, cx, under, beside);
            ext.place_root(cx, "root", "tour.buttons");
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Info, "cover"));
    draw(window, cx);
    advance(cx, ENTRY_SETTLED);
    draw(window, cx);
    let toast = bounds_of(&mut visual, "TOAST");
    let below = bounds_of(&mut visual, "BUTTON-under");
    assert!(
        toast.contains(&below.center()),
        "the button sits under the toast"
    );
    visual.simulate_click(toast.center(), gpui::Modifiers::default());
    draw(window, cx);
    visual.simulate_click(below.center(), gpui::Modifiers::default());
    draw(window, cx);
    assert_eq!(
        under.get(),
        0,
        "a click on the toast does not reach the button under it"
    );
    assert_eq!(beside.get(), 0);
    assert!(
        has_toast(&mut visual),
        "a left click does not close the toast"
    );
    let off = bounds_of(&mut visual, "BUTTON-beside");
    assert!(
        !toast.contains(&off.center()),
        "the button sits beside the toast"
    );
    click(&mut visual, "BUTTON-beside");
    draw(window, cx);
    assert_eq!(
        beside.get(),
        1,
        "a click beside the toast reaches the card's button"
    );
}

#[gpui::test]
fn hide_toast_closes_it_and_without_one_nothing_changes(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    hide_toast(&ext, window, cx);
    draw(window, cx);
    assert!(!has_toast(&mut visual));
    assert_eq!(visual.debug_bounds("TOAST"), None);
    assert_eq!(
        focused(&mut visual).as_deref(),
        Some("a"),
        "hide_toast with no toast changes nothing"
    );
    show_toast(&ext, window, cx, Toast::new(ToastKind::Warning, "hide"));
    draw(window, cx);
    assert!(has_toast(&mut visual));
    hide_toast(&ext, window, cx);
    draw(window, cx);
    assert!(!has_toast(&mut visual), "hide_toast closed the toast");
    assert_eq!(visual.debug_bounds("TOAST"), None);
    assert_eq!(focused(&mut visual).as_deref(), Some("a"));
}

#[gpui::test]
fn a_second_toast_replaces_the_first_and_the_firsts_timer_closes_nothing(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Info, "first"));
    draw(window, cx);
    advance(cx, Duration::from_secs(5));
    draw(window, cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Warning, "second"));
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "the second toast replaced the first"
    );
    bounds_of(&mut visual, "TOAST");
    advance(
        cx,
        TOAST_LIFETIME - Duration::from_secs(5) - Duration::from_millis(1),
    );
    draw(window, cx);
    advance(cx, Duration::from_millis(1));
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "the first toast's deadline passed and closed nothing"
    );
    advance(cx, Duration::from_secs(5) - Duration::from_millis(1));
    draw(window, cx);
    assert!(
        has_toast(&mut visual),
        "just before the second toast's 10 s it is still shown"
    );
    advance(cx, Duration::from_millis(1));
    draw(window, cx);
    assert!(
        !has_toast(&mut visual),
        "the second toast's own timer closed it"
    );
}

#[gpui::test]
fn with_no_toast_the_layer_takes_no_room_and_blocks_nothing(cx: &mut TestAppContext) {
    let beside = Rc::new(Cell::new(0));
    let (_ext, window) = open_app(cx, {
        let beside = beside.clone();
        move |cx, ext| {
            register_buttons(ext, cx, Rc::new(Cell::new(0)), beside);
            ext.place_root(cx, "root", "tour.buttons");
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    assert_eq!(
        node_bounds(&mut visual, "root"),
        Some(body_frame()),
        "the root fills the body: the layer took no room"
    );
    assert!(!has_toast(&mut visual));
    click(&mut visual, "BUTTON-beside");
    draw(window, cx);
    assert_eq!(beside.get(), 1, "the click reached the button");
    click(&mut visual, "BUTTON-beside");
    draw(window, cx);
    assert_eq!(beside.get(), 2, "each click reaches the button");
}

#[gpui::test]
fn a_modal_over_a_toast_closes_on_a_click_and_the_toast_stays(cx: &mut TestAppContext) {
    let (ext, window, mut visual) = open_over_focused_card(cx);
    show_toast(&ext, window, cx, Toast::new(ToastKind::Info, "under modal"));
    draw(window, cx);
    advance(cx, ENTRY_SETTLED);
    draw(window, cx);
    show_modal::<TestModal>(&ext, window, cx, |_, cx| TestModal::new(cx));
    draw(window, cx);
    assert!(
        has_modal(&mut visual) && has_toast(&mut visual),
        "a modal over a shown toast"
    );
    let toast = bounds_of(&mut visual, "TOAST");
    let modal = bounds_of(&mut visual, "MODAL-TEST");
    assert!(
        !modal.contains(&toast.center()),
        "the click point sits outside the modal"
    );
    visual.simulate_click(toast.center(), gpui::Modifiers::default());
    draw(window, cx);
    assert!(
        !has_modal(&mut visual),
        "a click where the toast is closed the modal"
    );
    assert!(has_toast(&mut visual), "the click did not reach the toast");
    bounds_of(&mut visual, "TOAST");
}
