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
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    if needs_window_icon(&target_os) {
        write_window_icon(&config.icon, &out_dir.join("window_icon.png"));
    }
    if target_os == "windows" {
        let icon = out_dir.join("app.ico");
        write_windows_icon(&config.icon, &icon)
            .unwrap_or_else(|error| panic!("product config {}: {error}", path.display()));
        let resource = out_dir.join("app.rc");
        fs::write(
            &resource,
            windows_resource(&icon, &config.name, &config.version),
        )
        .unwrap_or_else(|error| {
            panic!(
                "product config {}: writing {} failed: {error}",
                path.display(),
                resource.display()
            )
        });
        compile_windows_resource(&resource, &path);
    }
}

/// The sizes of the `.exe`'s icon, as Windows uses them: the title bar and
/// small icons, the taskbar, Explorer's views, and the largest tiles.
pub(crate) const WINDOWS_ICON_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

/// Writes the config's icon as an ICO to `out`, with one PNG frame for each
/// of [`WINDOWS_ICON_SIZES`], resized with Lanczos3 as the other icons are.
pub(crate) fn write_windows_icon(icon: &Path, out: &Path) -> Result<(), String> {
    let bytes = fs::read(icon)
        .map_err(|error| format!("reading the icon {} failed: {error}", icon.display()))?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|error| format!("decoding the icon {} failed: {error}", icon.display()))?;
    let frames = WINDOWS_ICON_SIZES
        .iter()
        .map(|&size| {
            let resized = image
                .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
                .into_rgba8();
            image::codecs::ico::IcoFrame::as_png(
                resized.as_raw(),
                size,
                size,
                image::ExtendedColorType::Rgba8,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("encoding the icon {} failed: {error}", icon.display()))?;
    let file = fs::File::create(out)
        .map_err(|error| format!("writing {} failed: {error}", out.display()))?;
    image::codecs::ico::IcoEncoder::new(std::io::BufWriter::new(file))
        .encode_images(&frames)
        .map_err(|error| format!("writing {} failed: {error}", out.display()))
}

/// The resource script of the `.exe`: the icon as resource 1, which GPUI
/// loads for the window and the taskbar, and the version information that
/// Explorer shows, from the config's name and version.
pub(crate) fn windows_resource(icon: &Path, name: &str, version: &str) -> String {
    // FILEVERSION takes four numbers: the leading digits of each part of the
    // version, and 0 for what is missing.
    let mut numbers = version
        .split(['.', '-', '+'])
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u16>().unwrap_or(0)
        })
        .chain(std::iter::repeat(0));
    let numbers: Vec<String> = (0..4)
        .map(|_| numbers.next().unwrap().to_string())
        .collect();
    let numbers = numbers.join(",");
    let icon = rc_string(&icon.display().to_string());
    let name = rc_string(name);
    let version = rc_string(version);
    // rc.exe reads the script in the ANSI code page unless it is told UTF-8,
    // and a name or a path may hold any character.
    format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "FileDescription", "{name}"
            VALUE "FileVersion", "{version}"
            VALUE "ProductName", "{name}"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    )
}

/// A string of a resource script: a backslash is written twice, and a
/// double quote as two double quotes.
fn rc_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\"\"")
}

/// Compiles the resource script and links it into the `.exe`. It needs
/// Windows' resource compiler, so it runs only on a Windows host; a build
/// for Windows on another host has no icon or version in its `.exe`.
#[cfg(all(windows, not(test)))]
fn compile_windows_resource(resource: &Path, config: &Path) {
    if let Err(error) = embed_resource::compile(resource, embed_resource::NONE).manifest_required()
    {
        panic!(
            "product config {}: compiling the Windows resource {} failed: {error}",
            config.display(),
            resource.display()
        );
    }
}

#[cfg(not(all(windows, not(test))))]
fn compile_windows_resource(resource: &Path, _config: &Path) {
    println!(
        "cargo:warning=the Windows resource {} is compiled only on Windows: this .exe has no icon or version",
        resource.display()
    );
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
