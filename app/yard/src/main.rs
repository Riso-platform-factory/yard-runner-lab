// A production build on Windows opens no console, as Zed's does.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(feature = "dev")]
    if yard::dev::run() {
        return;
    }
    yard::startup::run(
        yard::startup::PRODUCT,
        yard::startup::EXTENSIONS,
        yard::menus::menus,
    );
}
