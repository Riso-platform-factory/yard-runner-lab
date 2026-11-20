//! Renders the window `cargo run` shows into a PNG, with GPUI's headless
//! renderer, so no display server is needed.
//!
//! `cargo run -p screenshot -- --out <file.png> [--width 1280 --height 800]
//! [--view gallery --gallery <case>]`

use clap::{Parser, ValueEnum};
use gpui::{
    AnyWindowHandle, Bounds, HeadlessAppContext, PlatformHeadlessRenderer, WindowBounds, point, px,
    size,
};
use std::{cell::RefCell, path::PathBuf, process::ExitCode, sync::Arc, time::Duration};
use yard::startup::{EXTENSIONS, Init, PRODUCT};

/// What the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum View {
    /// The window `cargo run` shows.
    Window,
    /// The same window, with the UI kit's gallery as its content.
    Gallery,
}

/// Renders the window `cargo run` shows into a PNG.
#[derive(Parser)]
struct Args {
    /// The PNG to write.
    #[arg(long)]
    out: PathBuf,
    /// The window's width, in pixels.
    #[arg(long, default_value_t = 1280)]
    width: u32,
    /// The window's height, in pixels.
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// What the window shows.
    #[arg(long, value_enum, default_value_t = View::Window)]
    view: View,
    /// The gallery's case, as `UI_GALLERY` names it, with `--view gallery`:
    /// `all`, or `window-controls` for that page alone.
    #[arg(long, default_value = "all")]
    gallery: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let home = tempfile::tempdir().expect("a temporary folder can be created");
    // SAFETY: nothing has started a thread yet.
    unsafe { std::env::set_var(PRODUCT.home_variable(), home.path()) };
    let mut extensions: Vec<(&'static str, Init)> = EXTENSIONS.to_vec();
    if args.view == View::Gallery {
        // SAFETY: nothing has started a thread yet.
        unsafe { std::env::set_var("UI_GALLERY", &args.gallery) };
        extensions.push(("ui_gallery", ui_gallery::init));
    }
    let (product, paths) = yard::startup::start_logs(PRODUCT);
    // The group covers the renderer and the capture; the app's own startup
    // between them shows in the trace as the app's startup does.
    let group = tracing::info_span!(
        "screenshot-capture",
        width = args.width,
        height = args.height,
        out = %args.out.display(),
        view = ?args.view
    );
    let Some(renderer) = group.in_scope(start_renderer) else {
        return ExitCode::FAILURE;
    };
    // The window is the only one opened, so it takes the renderer.
    let renderer = RefCell::new(Some(renderer));
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(yard_ui::Assets),
        move || Ok(renderer.borrow_mut().take()),
    );
    cx.update(|cx| {
        yard::startup::init(
            cx,
            product,
            &paths,
            &extensions,
            yard::menus::menus,
            &RefCell::default(),
        )
    });
    let bounds = Bounds {
        origin: point(px(0.), px(0.)),
        size: size(px(args.width as f32), px(args.height as f32)),
    };
    let window = cx.update(|cx| {
        yard::startup::open_window(cx, product, None, Some(WindowBounds::Windowed(bounds)))
    });
    yard_log::startup_finished();
    match group.in_scope(|| capture(&mut cx, window, &args)) {
        Some(()) => ExitCode::SUCCESS,
        None => ExitCode::FAILURE,
    }
}

/// The platform's headless renderer, or `None`, logged as an error, when
/// this system has none or it fails to start.
fn start_renderer() -> Option<Box<dyn PlatformHeadlessRenderer>> {
    match gpui_platform::current_headless_renderer() {
        Ok(Some(renderer)) => {
            tracing::debug!("headless renderer ready");
            Some(renderer)
        }
        Ok(None) => {
            tracing::error!("no headless renderer on this system");
            None
        }
        Err(error) => {
            tracing::error!("the headless renderer failed to start: {error:#}");
            None
        }
    }
}

/// Lets the window settle, then writes what it draws to `args.out`.
fn capture(cx: &mut HeadlessAppContext, window: AnyWindowHandle, args: &Args) -> Option<()> {
    // Let layout, fonts, timers, and the window's activation settle over a
    // few frames.
    settle(cx);
    // The test platform's window reports a scale of 2, which GPUI's window
    // takes back each time the window's bounds or activation change, and
    // GPUI offers no way to change it from here. Once the window is settled,
    // it draws at scale 1 into the top left quarter of the image, which is
    // cropped below.
    let scaled = cx.update_window(window, |_, window, _| window.set_scale_factor(1.0));
    if let Err(error) = scaled {
        tracing::error!("the window closed before it settled: {error:#}");
        return None;
    }
    settle(cx);
    let scale = cx.update_window(window, |_, window, _| window.scale_factor());
    match scale {
        Ok(1.0) => tracing::debug!("window settled"),
        Ok(scale) => {
            tracing::error!("the window draws at scale {scale}, not 1");
            return None;
        }
        Err(error) => {
            tracing::error!("the window closed before it settled: {error:#}");
            return None;
        }
    }

    let image = match cx.capture_screenshot(window) {
        Ok(image) => image,
        Err(error) => {
            tracing::error!("the capture failed: {error:#}");
            return None;
        }
    };
    tracing::debug!(width = image.width(), height = image.height(), "captured");
    if image.width() < args.width || image.height() < args.height {
        tracing::error!(
            "the capture is {} × {}, smaller than the window",
            image.width(),
            image.height()
        );
        return None;
    }
    let image = image::imageops::crop_imm(&image, 0, 0, args.width, args.height).to_image();
    if let Err(error) = image.save_with_format(&args.out, image::ImageFormat::Png) {
        tracing::error!("writing the PNG failed: {error}");
        return None;
    }
    tracing::debug!("written");
    Some(())
}

/// Runs what is pending over a second of the test clock, a frame at a time.
fn settle(cx: &mut HeadlessAppContext) {
    for _ in 0..10 {
        cx.run_until_parked();
        cx.advance_clock(Duration::from_millis(100));
    }
    cx.run_until_parked();
}
