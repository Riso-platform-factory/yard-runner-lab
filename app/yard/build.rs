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
    write_window_icon(&config.icon);
}

/// Writes the config's icon as a 256 × 256 PNG, `$OUT_DIR/window_icon.png`,
/// which `startup.rs` embeds as the window icon on X11.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn write_window_icon(icon: &std::path::Path) {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("window_icon.png");
    image::open(icon)
        .unwrap_or_else(|error| panic!("reading the icon {} failed: {error}", icon.display()))
        .resize_exact(256, 256, image::imageops::FilterType::CatmullRom)
        .save_with_format(&out, image::ImageFormat::Png)
        .unwrap_or_else(|error| panic!("writing {} failed: {error}", out.display()));
}
