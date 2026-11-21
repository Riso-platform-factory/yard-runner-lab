use super::*;
use crate::v_flex;
use WindowButton::{Close, Maximize, Minimize};
use gpui::{AppContext, Tiling};
use std::sync::Arc;

const ALL: WindowControls = WindowControls {
    fullscreen: true,
    maximize: true,
    minimize: true,
    window_menu: true,
};

fn client() -> Decorations {
    Decorations::Client {
        tiling: Tiling::default(),
    }
}

fn layout(left: ButtonSide, right: ButtonSide) -> WindowButtonLayout {
    WindowButtonLayout { left, right }
}

#[test]
fn a_colon_splits_the_left_side_from_the_right() {
    assert_eq!(
        parse_button_layout("close,minimize:maximize"),
        Some(layout(
            [Some(Close), Some(Minimize), None],
            [Some(Maximize), None, None]
        ))
    );
}

#[test]
fn without_a_colon_every_button_is_on_the_right() {
    assert_eq!(
        parse_button_layout("minimize,maximize,close"),
        Some(default_button_layout())
    );
}

#[test]
fn a_trailing_colon_puts_every_button_on_the_left() {
    assert_eq!(
        parse_button_layout("close,minimize,maximize:"),
        Some(layout(
            [Some(Close), Some(Minimize), Some(Maximize)],
            [None; 3]
        ))
    );
}

#[test]
fn spaces_around_names_are_ignored() {
    assert_eq!(
        parse_button_layout(" close , minimize : maximize "),
        parse_button_layout("close,minimize:maximize")
    );
}

#[test]
fn an_empty_layout_has_no_buttons() {
    for empty in ["", ":"] {
        assert_eq!(
            parse_button_layout(empty),
            Some(layout([None; 3], [None; 3])),
            "{empty:?}"
        );
    }
}

#[test]
fn unknown_names_are_skipped() {
    assert_eq!(
        parse_button_layout("close,appmenu,minimize:maximize,spacer"),
        parse_button_layout("close,minimize:maximize")
    );
}

#[test]
fn a_layout_of_only_unknown_names_is_refused() {
    assert_eq!(parse_button_layout("appmenu:spacer"), None);
}

#[test]
fn a_button_is_placed_once() {
    assert_eq!(
        parse_button_layout("close:maximize,close,minimize"),
        Some(layout(
            [Some(Close), None, None],
            [Some(Maximize), Some(Minimize), None]
        ))
    );
}

#[test]
fn linux_with_client_decorations_draws_the_right_side() {
    assert_eq!(
        drawn_sides(Platform::Linux, client(), Some(default_button_layout())),
        DrawnSides {
            left: None,
            right: Some([Some(Minimize), Some(Maximize), Some(Close)]),
        }
    );
}

#[test]
fn linux_with_client_decorations_draws_the_left_side() {
    let left = parse_button_layout("close,minimize,maximize:");
    assert_eq!(
        drawn_sides(Platform::Linux, client(), left),
        DrawnSides {
            left: Some([Some(Close), Some(Minimize), Some(Maximize)]),
            right: None,
        }
    );
}

#[test]
fn linux_with_client_decorations_draws_both_sides() {
    let both = parse_button_layout("close:minimize,maximize");
    assert_eq!(
        drawn_sides(Platform::Linux, client(), both),
        DrawnSides {
            left: Some([Some(Close), None, None]),
            right: Some([Some(Minimize), Some(Maximize), None]),
        }
    );
}

#[test]
fn linux_draws_nothing_without_a_layout_or_with_server_decorations() {
    assert_eq!(
        drawn_sides(Platform::Linux, client(), None),
        DrawnSides::default()
    );
    assert_eq!(
        drawn_sides(Platform::Linux, client(), parse_button_layout(":")),
        DrawnSides::default()
    );
    assert_eq!(
        drawn_sides(
            Platform::Linux,
            Decorations::Server,
            Some(default_button_layout())
        ),
        DrawnSides::default()
    );
}

#[test]
fn macos_gets_no_drawn_controls() {
    for decorations in [Decorations::Server, client()] {
        assert_eq!(
            drawn_sides(Platform::Mac, decorations, Some(default_button_layout())),
            DrawnSides::default(),
            "{decorations:?}"
        );
    }
}

#[test]
fn windows_draws_its_caption_buttons_unless_fullscreen() {
    assert!(draws_windows_buttons(Platform::Windows, false));
    assert!(!draws_windows_buttons(Platform::Windows, true));
}

#[test]
fn macos_and_linux_draw_no_windows_caption_buttons() {
    for platform in [Platform::Mac, Platform::Linux] {
        for fullscreen in [false, true] {
            assert!(
                !draws_windows_buttons(platform, fullscreen),
                "{platform:?} fullscreen={fullscreen}"
            );
        }
    }
}

#[test]
fn windows_draws_none_of_the_linux_buttons() {
    for decorations in [Decorations::Server, client()] {
        assert_eq!(
            drawn_sides(
                Platform::Windows,
                decorations,
                Some(default_button_layout())
            ),
            DrawnSides::default(),
            "{decorations:?}"
        );
    }
}

#[test]
fn the_windows_buttons_are_minimize_maximize_and_close_in_order() {
    assert_eq!(
        windows_buttons(false),
        [Kind::Minimize, Kind::Maximize, Kind::Close]
    );
    assert_eq!(
        windows_buttons(true),
        [Kind::Minimize, Kind::Restore, Kind::Close]
    );
}

#[test]
fn each_windows_button_is_its_control_area_with_its_glyph() {
    let cases = [
        (
            Kind::Minimize,
            WindowControlArea::Min,
            "\u{e921}",
            "minimize",
        ),
        (
            Kind::Maximize,
            WindowControlArea::Max,
            "\u{e922}",
            "maximize",
        ),
        (Kind::Restore, WindowControlArea::Max, "\u{e923}", "restore"),
        (Kind::Close, WindowControlArea::Close, "\u{e8bb}", "close"),
    ];
    for (kind, area, glyph, action) in cases {
        assert_eq!(kind.control_area(), area, "{kind:?}");
        assert_eq!(kind.glyph(), glyph, "{kind:?}");
        assert_eq!(kind.action().name(), action, "{kind:?}");
    }
}

#[test]
fn this_mac_build_is_the_mac_platform() {
    if cfg!(target_os = "macos") {
        assert_eq!(Platform::CURRENT, Platform::Mac);
    } else if cfg!(target_os = "linux") {
        assert_eq!(Platform::CURRENT, Platform::Linux);
    }
}

#[test]
fn every_supported_button_shows_in_order() {
    assert_eq!(
        shown_buttons(&[Some(Close), Some(Minimize), Some(Maximize)], ALL),
        vec![Close, Minimize, Maximize]
    );
}

#[test]
fn an_unsupported_minimize_or_maximize_is_not_shown() {
    let side = [Some(Minimize), Some(Maximize), Some(Close)];
    let no_minimize = WindowControls {
        minimize: false,
        ..ALL
    };
    assert_eq!(shown_buttons(&side, no_minimize), vec![Maximize, Close]);
    let no_maximize = WindowControls {
        maximize: false,
        ..ALL
    };
    assert_eq!(shown_buttons(&side, no_maximize), vec![Minimize, Close]);
}

#[test]
fn close_shows_even_when_nothing_else_is_supported() {
    let none = WindowControls {
        fullscreen: false,
        maximize: false,
        minimize: false,
        window_menu: false,
    };
    assert_eq!(
        shown_buttons(&[Some(Minimize), Some(Maximize), Some(Close)], none),
        vec![Close]
    );
}

struct Buttons;

impl gpui::Render for Buttons {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let right = default_button_layout().right;
        v_flex()
            .child(WindowButtons::new("default", right))
            .child(WindowButtons::new("maximized", right).maximized(true))
            .child(WindowButtons::new("hovered", right).hovered(Close))
            .child(WindowButtons::new(
                "left",
                parse_button_layout("close,minimize,maximize:")
                    .unwrap()
                    .left,
            ))
            .child(WindowButtons::new("none", [None; 3]))
            .child(WindowsWindowButtons::new("windows"))
            .child(WindowsWindowButtons::new("windows-maximized").maximized(true))
            .child(WindowsWindowButtons::new("windows-hovered").hovered(Close))
            .child(
                WindowButtons::new("closed-only", right).supported(WindowControls {
                    fullscreen: false,
                    maximize: false,
                    minimize: false,
                    window_menu: false,
                }),
            )
    }
}

// A window whose app has the bundled assets, so each button's icon is really
// loaded when it paints.
#[test]
fn every_state_draws_in_a_window_with_assets() {
    let mut cx = gpui::HeadlessAppContext::with_asset_source(
        Arc::new(gpui::NoopTextSystem),
        Arc::new(crate::Assets),
    );
    cx.update(crate::init);
    let window = cx
        .open_window(gpui::size(gpui::px(400.), gpui::px(300.)), |_, cx| {
            cx.new(|_| Buttons)
        })
        .unwrap();
    cx.update_window(window.into(), |_, window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    })
    .unwrap();
}
