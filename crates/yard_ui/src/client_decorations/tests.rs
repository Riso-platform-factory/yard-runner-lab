use super::*;

const SHADOW: Pixels = px(10.);

fn window() -> gpui::Size<Pixels> {
    size(px(800.), px(600.))
}

fn edge(x: f32, y: f32) -> Option<ResizeEdge> {
    resize_edge(point(px(x), px(y)), SHADOW, window(), Tiling::default())
}

#[test]
fn the_middle_resizes_nothing() {
    assert_eq!(edge(400., 300.), None);
    // Just inside the band of one and a half shadows.
    assert_eq!(edge(16., 300.), None);
}

#[test]
fn each_edge_resizes_from_its_shadow() {
    assert_eq!(edge(400., 5.), Some(ResizeEdge::Top));
    assert_eq!(edge(400., 595.), Some(ResizeEdge::Bottom));
    assert_eq!(edge(5., 300.), Some(ResizeEdge::Left));
    assert_eq!(edge(795., 300.), Some(ResizeEdge::Right));
}

#[test]
fn each_corner_reaches_one_and_a_half_shadows_in() {
    assert_eq!(edge(2., 2.), Some(ResizeEdge::TopLeft));
    assert_eq!(edge(14., 14.), Some(ResizeEdge::TopLeft));
    assert_eq!(edge(798., 2.), Some(ResizeEdge::TopRight));
    assert_eq!(edge(786., 14.), Some(ResizeEdge::TopRight));
    assert_eq!(edge(2., 598.), Some(ResizeEdge::BottomLeft));
    assert_eq!(edge(14., 586.), Some(ResizeEdge::BottomLeft));
    assert_eq!(edge(798., 598.), Some(ResizeEdge::BottomRight));
    assert_eq!(edge(786., 586.), Some(ResizeEdge::BottomRight));
}

#[test]
fn between_the_shadow_and_the_corner_band_an_edge_resizes_nothing() {
    // Past the shadow, but inside the band of one and a half shadows.
    assert_eq!(edge(400., 12.), None);
    assert_eq!(edge(12., 300.), None);
}

#[test]
fn a_tiled_edge_does_not_resize() {
    let tiled = Tiling {
        top: true,
        left: true,
        right: false,
        bottom: false,
    };
    let at = |x: f32, y: f32| resize_edge(point(px(x), px(y)), SHADOW, window(), tiled);
    assert_eq!(at(400., 5.), None);
    assert_eq!(at(5., 300.), None);
    assert_eq!(at(2., 2.), None);
    // A corner on a tiled edge resizes from the edge that is not tiled.
    assert_eq!(at(798., 2.), Some(ResizeEdge::Right));
    assert_eq!(at(795., 300.), Some(ResizeEdge::Right));
    assert_eq!(at(2., 598.), Some(ResizeEdge::BottomLeft));
    let all = Tiling::tiled();
    for (x, y) in [(2., 2.), (400., 5.), (795., 300.), (798., 598.)] {
        assert_eq!(
            resize_edge(point(px(x), px(y)), SHADOW, window(), all),
            None
        );
    }
}
