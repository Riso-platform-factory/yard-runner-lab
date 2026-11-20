use super::*;
use crate::{TextColor, v_flex};
use gpui::{AppContext, AssetSource, IntoElement, ParentElement};
use std::sync::Arc;

const PATHS: [(IconName, &str); 12] = [
    (IconName::Check, "icons/check.svg"),
    (IconName::ChevronDown, "icons/chevron_down.svg"),
    (IconName::ChevronRight, "icons/chevron_right.svg"),
    (IconName::Close, "icons/close.svg"),
    (IconName::GenericClose, "icons/generic_close.svg"),
    (IconName::GenericMaximize, "icons/generic_maximize.svg"),
    (IconName::GenericMinimize, "icons/generic_minimize.svg"),
    (IconName::GenericRestore, "icons/generic_restore.svg"),
    (IconName::Info, "icons/info.svg"),
    (IconName::Plus, "icons/plus.svg"),
    (IconName::Warning, "icons/warning.svg"),
    (IconName::XCircle, "icons/x_circle.svg"),
];
const SIZES: [(IconSize, f32); 5] = [
    (IconSize::Indicator, 0.625),
    (IconSize::XSmall, 0.75),
    (IconSize::Small, 0.875),
    (IconSize::Medium, 1.),
    (IconSize::XLarge, 3.),
];
const COLORS: [TextColor; 2] = [TextColor::Default, TextColor::Error];

#[test]
fn all_holds_every_name_once() {
    assert_eq!(IconName::ALL.len(), PATHS.len());
    for (name, _) in PATHS {
        assert_eq!(
            IconName::ALL
                .iter()
                .filter(|listed| **listed == name)
                .count(),
            1,
            "{name:?} must be in ALL exactly once"
        );
    }
}

#[test]
fn every_path_is_icons_name_svg() {
    for (name, path) in PATHS {
        assert_eq!(name.path(), path, "{name:?}");
    }
}

#[test]
fn assets_serves_an_svg_for_every_icon() {
    for &name in IconName::ALL {
        let path = name.path();
        let bytes = Assets
            .load(path)
            .unwrap()
            .unwrap_or_else(|| panic!("no asset at {path}"));
        assert!(bytes.starts_with(b"<svg"), "{path} is not an SVG");
    }
}

#[test]
fn assets_lists_every_icon_under_icons() {
    let listed = Assets.list("icons/").unwrap();
    for &name in IconName::ALL {
        let path = name.path();
        assert!(
            listed.iter().any(|entry| entry.as_str() == path),
            "{path} is not listed: {listed:?}"
        );
    }
}

#[test]
fn a_path_that_is_not_bundled_gives_nothing() {
    assert!(Assets.load("icons/nope.svg").unwrap().is_none());
}

#[test]
fn every_size_gives_zeds_rems() {
    for (size, expected) in SIZES {
        assert_eq!(size.rems(), gpui::rems(expected), "{size:?}");
    }
}

#[test]
fn the_default_size_is_medium() {
    assert_eq!(IconSize::default(), IconSize::Medium);
}

struct Icons;

impl gpui::Render for Icons {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let mut children = Vec::new();
        for &name in IconName::ALL {
            for (size, _) in SIZES {
                for color in COLORS {
                    children.push(Icon::new(name).size(size).color(color).into_any_element());
                }
            }
        }
        v_flex().children(children)
    }
}

// A window whose app has the bundled assets, so each icon's file is really
// loaded when the icon paints.
#[test]
fn every_icon_at_every_size_and_color_draws_in_a_window_with_assets() {
    let mut cx = gpui::HeadlessAppContext::with_asset_source(
        Arc::new(gpui::NoopTextSystem),
        Arc::new(Assets),
    );
    cx.update(crate::init);
    let window = cx
        .open_window(gpui::size(gpui::px(1024.), gpui::px(768.)), |_, cx| {
            cx.new(|_| Icons)
        })
        .unwrap();
    cx.update_window(window.into(), |_, window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    })
    .unwrap();
}
