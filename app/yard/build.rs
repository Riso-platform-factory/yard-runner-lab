use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-env-changed=PRODUCT_CONFIG");
    let path = env::var_os("PRODUCT_CONFIG").map_or_else(
        || PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("product.toml"),
        PathBuf::from,
    );
    println!("cargo:rerun-if-changed={}", path.display());
    let config = yard_build_config::read(&path).unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rerun-if-changed={}", config.icon.display());
    println!("cargo:rustc-env=PRODUCT_NAME={}", config.name);
    println!("cargo:rustc-env=PRODUCT_VERSION={}", config.version);
    println!("cargo:rustc-env=PRODUCT_IDENTIFIER={}", config.identifier);
    println!("cargo:rustc-env=PRODUCT_URL_SCHEME={}", config.url_scheme);
    println!("cargo:rustc-env=PRODUCT_ICON={}", config.icon.display());
    println!(
        "cargo:rustc-env=PRODUCT_UPDATE_URL={}",
        config.update_url.unwrap_or_default()
    );
    // A build script runs on the host, so the target is read from Cargo.
    if needs_window_icon(&env::var("CARGO_CFG_TARGET_OS").unwrap()) {
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("window_icon.png");
        write_window_icon(&config.icon, &out);
    }
}

/// Whether a build for `target_os` embeds the window icon, as X11 needs.
pub(crate) fn needs_window_icon(target_os: &str) -> bool {
    matches!(target_os, "linux" | "freebsd")
}

/// Writes the config's icon as a 256 × 256 PNG to `out`, which `startup.rs`
/// embeds as the window icon on X11. The icon is decoded as a PNG whatever
/// its file name, as `yard_build_config` checked it by its bytes.
pub(crate) fn write_window_icon(icon: &Path, out: &Path) {
    let bytes = fs::read(icon)
        .unwrap_or_else(|error| panic!("reading the icon {} failed: {error}", icon.display()));
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .unwrap_or_else(|error| panic!("decoding the icon {} failed: {error}", icon.display()))
        .resize_exact(256, 256, image::imageops::FilterType::CatmullRom)
        .save_with_format(out, image::ImageFormat::Png)
        .unwrap_or_else(|error| panic!("writing {} failed: {error}", out.display()));
}
