use std::{env, path::PathBuf};

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
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    prepare_app_icon_x11(&config.icon);
}

/// Writes the config's icon at 256 × 256 to `$OUT_DIR/app_icon.png`, which
/// the app decodes for its X11 window icon, as Zed's `build.rs` does.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn prepare_app_icon_x11(icon: &std::path::Path) {
    use image::{ImageReader, imageops};

    let resized = ImageReader::open(icon)
        .unwrap_or_else(|error| panic!("opening {} failed: {error}", icon.display()))
        .decode()
        .unwrap_or_else(|error| panic!("decoding {} failed: {error}", icon.display()))
        .resize(256, 256, imageops::FilterType::Lanczos3);
    // The name matches the `include_bytes!` of the window options in `startup.rs`.
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("app_icon.png");
    resized
        .save(&out)
        .unwrap_or_else(|error| panic!("saving {} failed: {error}", out.display()));
}
