use gpui::{App, AsyncApp, WindowOptions};
use std::{cell::RefCell, rc::Rc, sync::Arc};

type Init = fn(&mut App, &yard_shell::Extension);

use yard_product::Product;

#[derive(Default)]
struct OpenUrls {
    app: Option<AsyncApp>,
    early: Vec<Vec<String>>,
}

/// Runs the application with the product's identifier in the logs.
pub(crate) fn run(
    product: Product,
    extensions: &[(&'static str, Init)],
    menus: fn(&Product) -> Vec<gpui::Menu>,
) {
    // The logs start from the build values as they are, since `start` needs
    // the product; the product is then checked once the trace listens, so
    // its `product` group shows in `app-startup`.
    yard_log::start(product);
    let product = Product::new(product.name, product.version, product.identifier);
    let paths = yard_paths::Paths::from_env(&product);
    {
        let _group = tracing::info_span!("logs-prepare").entered();
        yard_log::open_files(paths.logs_dir());
    }
    let extensions = extensions.to_vec();
    let app = gpui_platform::application().with_assets(yard_ui::Assets);
    let open_urls = Rc::new(RefCell::new(OpenUrls::default()));
    app.on_open_urls({
        let open_urls = open_urls.clone();
        move |urls| {
            let mut listener = open_urls.borrow_mut();
            if let Some(app) = &listener.app {
                app.spawn(async move |cx| {
                    cx.update(|cx| yard_shell::open_urls(cx, urls));
                })
                .detach();
            } else {
                listener.early.push(urls);
            }
        }
    });
    app.run(move |cx: &mut App| {
        {
            let _group = tracing::info_span!("app-init").entered();
            yard_log::init_app(cx);
            yard_settings::init(cx);
            yard_settings::load(cx, paths.settings_file());
            yard_settings::watch(cx);
            yard_ui::init(cx);
            yard_ui::load_user_themes(cx, &paths.themes_dir());
            yard_ui::apply_theme_setting(cx);
            yard_shell::set_window_title(cx, product.name);
            yard_shell::set_url_scheme(cx, env!("PRODUCT_URL_SCHEME"));
            yard_shell::set_layout_file(cx, paths.layout_file());
            yard_shell::set_state_file(cx, paths.state_file());
            yard_shell::set_store_dir(cx, paths.store_dir());
            let named_paths = paths.clone();
            yard_shell::set_named_layout_files(cx, move |name| named_paths.named_layout_file(name));
            yard_shell::bind_default_keys(cx);
            yard_ui::bind_text_input_keys(cx);
            yard_shell::handle_quit(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        }
        let early_urls = std::mem::take(&mut open_urls.borrow_mut().early);
        for urls in early_urls {
            yard_shell::open_urls(cx, urls);
        }
        {
            let _group = tracing::info_span!("extensions-load", count = extensions.len()).entered();
            for (name, init) in extensions {
                let _group = tracing::info_span!("extension-init", extension = name).entered();
                if let Some(extension) = yard_shell::Extension::new(cx, name) {
                    init(cx, &extension);
                }
            }
        }
        // Hand over any URLs that arrived during extension initialization.
        // GPUI queues subsequent deliveries on the main thread in callback order.
        let early_urls = {
            let mut listener = open_urls.borrow_mut();
            listener.app = Some(cx.to_async());
            std::mem::take(&mut listener.early)
        };
        for urls in early_urls {
            yard_shell::open_urls(cx, urls);
        }
        yard_shell::extensions_loaded(cx);
        yard_keymap::load_user_keymap(cx, &paths.keymap_file());
        yard_keymap::watch_user_keymap(cx, paths.keymap_file());
        yard_shell::set_menus(cx, menus(&product));
        yard_shell::set_window_opener(cx, move |cx| {
            open_window(cx, product, None);
        });
        let session = yard_shell::saved_session(cx, &paths.session_file());
        yard_shell::track_session(cx, paths.session_file());
        match session {
            Some(windows) => {
                for (index, saved) in windows.iter().enumerate() {
                    let window_file = (index == 0).then(|| paths.window_file());
                    open_session_window(cx, product, saved, window_file);
                }
            }
            None => open_window(cx, product, Some(paths.window_file())),
        }
        start_update_check(cx, product);
        yard_log::startup_finished();
    });
}

/// Starts checking the product's update address, when it names one, and
/// installing what it finds, through an extension handle named `update` for
/// the toasts.
fn start_update_check(cx: &mut App, product: Product) {
    let url = env!("PRODUCT_UPDATE_URL");
    if url.is_empty() {
        tracing::debug!("no update address");
        return;
    }
    let Some(extension) = yard_shell::Extension::new(cx, "update") else {
        return;
    };
    let user_agent = format!("{}/{}", product.name, product.version);
    match reqwest_client::ReqwestClient::user_agent(&user_agent) {
        Ok(client) => yard_update::start(cx, &extension, product, url, Arc::new(client)),
        Err(error) => tracing::warn!("no update check: the HTTP client failed to start: {error:#}"),
    }
}

/// The options every window of the product starts from: its title bar, its
/// `app_id`, which Wayland and X11 match with the product's `.desktop` file,
/// and on Linux and FreeBSD its X11 icon.
fn window_options(product: Product) -> WindowOptions {
    tracing::debug!(app_id = product.identifier, "window options");
    WindowOptions {
        titlebar: Some(yard_ui::titlebar_options(product.name)),
        app_id: Some(product.identifier.to_owned()),
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        icon: app_icon(),
        ..Default::default()
    }
}

/// The product's icon, decoded once from the PNG that `build.rs` sized for
/// X11, as Zed's window options do.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn app_icon() -> Option<Arc<image::RgbaImage>> {
    static APP_ICON: std::sync::LazyLock<Option<Arc<image::RgbaImage>>> =
        std::sync::LazyLock::new(|| {
            const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app_icon.png"));
            match image::load_from_memory_with_format(BYTES, image::ImageFormat::Png) {
                Ok(image) => Some(Arc::new(image.into_rgba8())),
                // `build.rs` wrote this PNG, so it not decoding is a bug.
                Err(error) => {
                    tracing::error!("the window icon does not decode: {error}");
                    None
                }
            }
        });
    APP_ICON.clone()
}

fn open_window(cx: &mut App, product: Product, window_file: Option<std::path::PathBuf>) {
    let _group = tracing::info_span!("window-open").entered();
    let saved = window_file
        .as_ref()
        .and_then(|file| yard_shell::saved_window_bounds(cx, file));
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: saved.map(|(bounds, _)| bounds),
                display_id: saved.map(|(_, display)| display),
                ..window_options(product)
            },
            |window, cx| {
                yard_ui::follow_appearance(window, cx);
                yard_shell::new_shell(window, cx)
            },
        )
        .unwrap_or_else(|error| panic!("{} window opens: {error}", product.name));
    window
        .update(cx, |_, window, cx| {
            if let Some(file) = window_file {
                yard_shell::track_window_bounds(window, cx, file);
            }
            window.activate_window();
        })
        .unwrap_or_else(|error| panic!("{} window is open: {error}", product.name));
    cx.activate(true);
}

/// Opens a window of the saved session with its bounds and layers; the
/// main window, the first, also keeps `window.json` up to date.
fn open_session_window(
    cx: &mut App,
    product: Product,
    saved: &yard_shell::SessionWindow,
    window_file: Option<std::path::PathBuf>,
) {
    let _group = tracing::info_span!("window-open").entered();
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: saved.bounds,
                display_id: saved.display,
                ..window_options(product)
            },
            |window, cx| {
                yard_ui::follow_appearance(window, cx);
                yard_shell::new_session_shell(window, cx, saved)
            },
        )
        .unwrap_or_else(|error| panic!("{} window opens: {error}", product.name));
    window
        .update(cx, |_, window, cx| {
            if let Some(file) = window_file {
                yard_shell::track_window_bounds(window, cx, file);
            }
            window.activate_window();
        })
        .unwrap_or_else(|error| panic!("{} window is open: {error}", product.name));
    cx.activate(true);
}
