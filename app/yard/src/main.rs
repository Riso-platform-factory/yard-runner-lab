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
