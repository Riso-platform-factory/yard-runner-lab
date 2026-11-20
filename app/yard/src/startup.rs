use gpui::{App, AsyncApp, WindowOptions};
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// What an extension registers when the application starts.
pub type Init = fn(&mut App, &yard_shell::Extension);

use yard_product::Product;

/// The product, from the build config.
pub const PRODUCT: Product = Product {
    name: env!("PRODUCT_NAME"),
    version: env!("PRODUCT_VERSION"),
    identifier: env!("PRODUCT_IDENTIFIER"),
};

/// The extensions that come with the application.
pub const EXTENSIONS: &[(&str, Init)] = &[("command_palette", command_palette::init)];

/// The URLs the system hands over, kept until the app can open them.
#[derive(Default)]
pub struct OpenUrls {
    app: Option<AsyncApp>,
    early: Vec<Vec<String>>,
}

/// Runs the application with the product's identifier in the logs.
pub fn run(
    product: Product,
    extensions: &[(&'static str, Init)],
    menus: fn(&Product) -> Vec<gpui::Menu>,
) {
    let (product, paths) = start_trace(product);
    // A second start hands its arguments over and exits before it opens the
    // log files, which belong to the running instance.
    #[cfg(not(target_os = "macos"))]
    let (instance, argument_urls) = one_instance(product, &paths);
    open_log_files(&paths);
    let extensions = extensions.to_vec();
    let app = gpui_platform::application().with_assets(yard_ui::Assets);
    let open_urls = Rc::new(RefCell::new(OpenUrls::default()));
    // GPUI calls `on_open_urls` only on macOS, so elsewhere the URLs given
    // on the command line take the same way.
    #[cfg(not(target_os = "macos"))]
    if !argument_urls.is_empty() {
        deliver_urls(&open_urls, argument_urls);
    }
    app.on_open_urls({
        let open_urls = open_urls.clone();
        move |urls| deliver_urls(&open_urls, urls)
    });
    app.run(move |cx: &mut App| {
        #[cfg(not(target_os = "macos"))]
        if let Some(listener) = instance {
            yard_shell::serve_instance_requests(cx, listener, env!("PRODUCT_URL_SCHEME"));
        }
        init(cx, product, &paths, &extensions, menus, &open_urls);
        let session = yard_shell::saved_session(cx, &paths.session_file());
        yard_shell::track_session(cx, paths.session_file());
        match session {
            Some(windows) => {
                for (index, saved) in windows.iter().enumerate() {
                    let window_file = (index == 0).then(|| paths.window_file());
                    open_session_window(cx, product, saved, window_file);
                }
            }
            None => {
                open_window(cx, product, Some(paths.window_file()), None);
            }
        }
        start_update_check(cx, product);
        yard_log::startup_finished();
    });
}

/// Starts both logs and opens their files in the product's logs directory,
/// and returns the checked product with its directories.
pub fn start_logs(product: Product) -> (Product, yard_paths::Paths) {
    let (product, paths) = start_trace(product);
    open_log_files(&paths);
    (product, paths)
}

/// Starts both logs, and returns the checked product with its directories.
fn start_trace(product: Product) -> (Product, yard_paths::Paths) {
    // The logs start from the build values as they are, since `start` needs
    // the product; the product is then checked once the trace listens, so
    // its `product` group shows in `app-startup`.
    yard_log::start(product);
    let product = Product::new(product.name, product.version, product.identifier);
    let paths = yard_paths::Paths::from_env(&product);
    (product, paths)
}

/// Opens the files of both logs in the product's logs directory.
fn open_log_files(paths: &yard_paths::Paths) {
    let _group = tracing::info_span!("logs-prepare").entered();
    yard_log::open_files(paths.logs_dir());
}

/// Hands the command line's URLs to the instance already running and exits,
/// or else returns this start's listener, if it is the one instance, with
/// the URLs. macOS keeps one instance and hands URLs over itself.
#[cfg(not(target_os = "macos"))]
fn one_instance(
    product: Product,
    paths: &yard_paths::Paths,
) -> (Option<yard_shell::InstanceListener>, Vec<String>) {
    let (instance, urls) = yard_shell::start_instance(
        paths,
        std::env::args_os().skip(1),
        env!("PRODUCT_URL_SCHEME"),
    );
    let listener = match instance {
        yard_shell::Instance::Forwarded => std::process::exit(0),
        yard_shell::Instance::Refused(reason) => {
            eprintln!("{} is already running: {reason}", product.name);
            std::process::exit(1)
        }
        yard_shell::Instance::Listening(listener) => Some(listener),
        yard_shell::Instance::Alone(_) => None,
    };
    (listener, urls)
}

/// Opens URLs once the extensions are loaded: the system's on macOS, and
/// elsewhere those of the command line.
fn deliver_urls(open_urls: &RefCell<OpenUrls>, urls: Vec<String>) {
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

/// Sets up the app as the application does before it opens a window: the
/// settings, the theme, the shell, the extensions, the keymap, and the menus.
pub fn init(
    cx: &mut App,
    product: Product,
    paths: &yard_paths::Paths,
    extensions: &[(&'static str, Init)],
    menus: fn(&Product) -> Vec<gpui::Menu>,
    open_urls: &RefCell<OpenUrls>,
) {
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
        for &(name, init) in extensions {
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
        open_window(cx, product, None, None);
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
pub fn window_options(product: Product, cx: &App) -> WindowOptions {
    let window_background = yard_ui::window_background(cx);
    tracing::debug!(app_id = product.identifier, background = ?window_background, "window options");
    WindowOptions {
        titlebar: Some(yard_ui::titlebar_options(product.name)),
        window_background,
        app_id: Some(product.identifier.to_owned()),
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        icon: app_icon(),
        // YARD draws its own frame and controls on Linux, as Zed does.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        window_decorations: Some(gpui::WindowDecorations::Client),
        ..Default::default()
    }
}

/// The window icon, decoded on first use from the PNG that `build.rs` wrote,
/// and shared by every window after that.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn app_icon() -> Option<Arc<image::RgbaImage>> {
    static ICON: std::sync::OnceLock<Option<Arc<image::RgbaImage>>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let png = include_bytes!(concat!(env!("OUT_DIR"), "/window_icon.png"));
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .map(|icon| Arc::new(icon.into_rgba8()))
            // `build.rs` wrote this PNG with the same crate, so a failure is a bug.
            .inspect_err(|error| tracing::error!("the window icon does not decode: {error}"))
            .ok()
    })
    .clone()
}

/// Opens a window of the product at `bounds`, or else at the bounds saved in
/// `window_file`, which it then keeps up to date.
pub fn open_window(
    cx: &mut App,
    product: Product,
    window_file: Option<std::path::PathBuf>,
    bounds: Option<gpui::WindowBounds>,
) -> gpui::AnyWindowHandle {
    let _group = tracing::info_span!("window-open").entered();
    let saved = window_file
        .as_ref()
        .and_then(|file| yard_shell::saved_window_bounds(cx, file));
    let options = window_options(product, cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: bounds.or(saved.map(|(bounds, _)| bounds)),
                display_id: saved.map(|(_, display)| display),
                ..options
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
    window.into()
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
    let options = window_options(product, cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: saved.bounds,
                display_id: saved.display,
                ..options
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
