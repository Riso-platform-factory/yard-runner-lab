//! `cargo xtask bundle linux`: makes `<name>-<version>-linux-<arch>.tar.gz`,
//! holding `<name>.app/` with the app, its `.desktop` file, its icons, and
//! `install.sh`, from a product config. Laid out as Zed's `script/bundle-linux`
//! lays out `zed.app`; the scripts are YARD's own.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use yard_build_config::BuildConfig;

use super::signing::shell_quote;
use super::{
    Failure, Interrupt, Runner, Staging, arguments, build, env_pairs, interrupted_at_end,
    is_path_component, log_outcome, make_executable, not_interrupted, read_product, repository,
    set_mode, sha256, step,
};

/// The options of `cargo xtask bundle linux`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct LinuxArgs {
    /// The product config [default: app/yard/product.toml in the repository].
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// The folder the package goes into [default: dist in the repository].
    #[arg(long, value_name = "FOLDER")]
    pub out: Option<PathBuf>,
    /// Builds a review bundle, which runs `dev`, with a product other than
    /// the production one.
    #[arg(long)]
    pub dev: bool,
    /// An environment variable of the review bundle, exported by its
    /// `bin/<name>`. Needs `--dev`.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
}

/// What `bundle_linux` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBundled {
    /// `<out>/<name>.app`, unpacked, for local runs.
    pub app: PathBuf,
    /// `<out>/<name>-<version>-linux-<arch>.tar.gz` and its SHA-256 in hex.
    pub archive: (PathBuf, String),
}

/// The icon sizes, each made only from a source at least as large.
const ICON_SIZES: [u32; 2] = [512, 1024];

/// Makes `<out>/<name>.app` and its archive from the config, inside the
/// group `bundle-linux{config, dev}`, printing each step's name on stdout, as
/// [`super::bundle_macos`] does. `system` is the host's
/// (`std::env::consts::OS`): anything but `linux` is refused at the start.
pub fn bundle_linux(
    options: &LinuxArgs,
    system: &str,
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<LinuxBundled, Failure> {
    let config = options
        .config
        .clone()
        .unwrap_or_else(|| repository().join(super::PRODUCTION_CONFIG));
    let _group = tracing::info_span!(
        "bundle-linux",
        config = %config.display(),
        dev = options.dev
    )
    .entered();
    let result = bundle(&config, options, system, interrupt, runner);
    log_outcome(&result, interrupt);
    result
}

fn bundle(
    config: &Path,
    options: &LinuxArgs,
    system: &str,
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<LinuxBundled, Failure> {
    tracing::debug!("arguments");
    if system != "linux" {
        return Err(Failure::new(
            "arguments",
            format!("bundle linux runs only on Linux, and this system is {system}"),
        ));
    }
    let (config, out) = arguments(config, options.out.as_deref(), options.dev, &options.env)?;

    tracing::debug!("config");
    let product = read_product(&config, options.dev, true)?;
    if !is_path_component(&product.version) {
        return Err(Failure::new(
            "config",
            format!(
                "{}: version must be a single path component",
                config.display()
            ),
        ));
    }

    // Without debug data, as Zed's is.
    let binary = build(
        &config,
        options.dev,
        &[("CARGO_PROFILE_RELEASE_STRIP", "debuginfo")],
        interrupt,
        runner,
    )?;

    step("icon", interrupt)?;
    let result = super::staged(&out, "icon", "linux", |staging| {
        in_temp(staging, &product, options, &binary, &out, interrupt, runner)
    });
    interrupted_at_end(result, interrupt)
}

/// The steps that work in the staging folder: the icons, the bundle, and the
/// archive; then, unless the run was interrupted, the app and the archive
/// are published together.
fn in_temp(
    staging: &mut Staging,
    product: &BuildConfig,
    options: &LinuxArgs,
    binary: &Path,
    out: &Path,
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<LinuxBundled, Failure> {
    let name = yard_product::file_name(&product.name);
    let temp = staging.path.clone();
    let icons = make_icons(&temp, &product.icon).map_err(|reason| Failure::new("icon", reason))?;

    step("bundle", interrupt)?;
    let app = lay_out(product, options, binary, &temp, &icons).map_err(|error| {
        Failure::new("bundle", format!("could not lay out the bundle: {error}"))
    })?;

    step("archive", interrupt)?;
    let archive = |reason| Failure::new("archive", reason);
    let staged = temp.join("release.tar.gz");
    runner
        .run(
            Command::new("tar")
                .arg("-czf")
                .arg(&staged)
                .arg("-C")
                .arg(&temp)
                .arg(format!("{name}.app")),
        )
        .map_err(archive)?;
    // The digest comes from the staged archive, so no published archive
    // lacks one.
    let digest = sha256(&["sha256sum"], &staged, runner).map_err(archive)?;

    // Both are published together, or neither is.
    not_interrupted("publish", interrupt)?;
    let destination = out.join(format!("{name}.app"));
    tracing::debug!(destination = %destination.display(), "publish");
    staging
        .publish(&app, &destination)
        .map_err(|reason| Failure::new("bundle", reason))?;
    let published = out.join(format!(
        "{name}-{}-linux-{}.tar.gz",
        product.version,
        std::env::consts::ARCH
    ));
    staging.publish(&staged, &published).map_err(archive)?;
    tracing::debug!(archive = %published.display(), sha256 = digest, "archived");
    Ok(LinuxBundled {
        app: destination,
        archive: (published, digest),
    })
}

/// Makes `<temp>/icon-<size>.png` from the PNG for each of [`ICON_SIZES`]
/// that is not larger than it, never scaling up.
fn make_icons(temp: &Path, icon: &Path) -> Result<Vec<(u32, PathBuf)>, String> {
    let source =
        image::open(icon).map_err(|error| format!("could not read {}: {error}", icon.display()))?;
    let mut icons = Vec::new();
    for size in ICON_SIZES {
        if size > source.width() {
            tracing::debug!(size, "the icon is smaller: not made");
            continue;
        }
        let path = temp.join(format!("icon-{size}.png"));
        if size == source.width() {
            fs::copy(icon, &path)
                .map_err(|error| format!("could not copy {}: {error}", icon.display()))?;
        } else {
            source
                .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
                .save(&path)
                .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        }
        tracing::debug!(size, "icon");
        icons.push((size, path));
    }
    Ok(icons)
}

/// Lays out `<folder>/<name>.app`: the binary (behind a wrapper running `dev`
/// with `--dev`), the `.desktop` file, the icons, and `install.sh`.
fn lay_out(
    product: &BuildConfig,
    options: &LinuxArgs,
    binary: &Path,
    folder: &Path,
    icons: &[(u32, PathBuf)],
) -> io::Result<PathBuf> {
    let name = yard_product::file_name(&product.name);
    let app = folder.join(format!("{name}.app"));
    fs::create_dir_all(app.join("bin"))?;
    let executable = app.join("bin").join(&name);
    if options.dev {
        let real = app.join("libexec").join(&name);
        fs::create_dir_all(app.join("libexec"))?;
        fs::copy(binary, &real)?;
        make_executable(&real)?;
        let env = env_pairs(&options.env).map_err(io::Error::other)?;
        fs::write(&executable, wrapper(&name, &env))?;
    } else {
        fs::copy(binary, &executable)?;
    }
    make_executable(&executable)?;

    let applications = app.join("share/applications");
    fs::create_dir_all(&applications)?;
    let desktop = applications.join(format!("{}.desktop", product.identifier));
    fs::write(&desktop, desktop_entry(product))?;
    set_mode(&desktop, 0o644)?;

    for (size, icon) in icons {
        let apps = app.join(format!("share/icons/hicolor/{size}x{size}/apps"));
        fs::create_dir_all(&apps)?;
        let target = apps.join(format!("{}.png", product.identifier));
        fs::rename(icon, &target)?;
        set_mode(&target, 0o644)?;
    }

    let install = app.join("install.sh");
    fs::write(&install, install_script(product, options.dev))?;
    make_executable(&install)?;
    Ok(app)
}

/// `bin/<name>` of a review bundle: it exports the `--env` values and runs
/// `libexec/<name>` with `dev`. It follows the links to itself first, so it
/// also runs through `~/.local/bin/<name>`, and finds `libexec/<name>` before
/// it exports anything.
fn wrapper(name: &str, env: &[(&str, &str)]) -> String {
    let mut text = String::from(
        "#!/bin/sh\n\
         self=$0\n\
         while [ -L \"$self\" ]; do\n\
         \x20   link=$(readlink \"$self\")\n\
         \x20   case $link in\n\
         \x20   /*) self=$link ;;\n\
         \x20   *) self=$(dirname \"$self\")/$link ;;\n\
         \x20   esac\n\
         done\n",
    );
    // The path is found before an `--env PATH=…` can hide `dirname`.
    text.push_str(&format!(
        "real=\"$(dirname \"$self\")/../libexec/{name}\"\n"
    ));
    for (key, value) in env {
        text.push_str(&format!("export {key}={}\n", shell_quote(value)));
    }
    text.push_str("exec \"$real\" dev \"$@\"\n");
    text
}

/// The keys and values of the `.desktop` file in the bundle, in order, each
/// value escaped.
fn desktop_fields(product: &BuildConfig) -> Vec<(&'static str, String)> {
    let name = yard_product::file_name(&product.name);
    vec![
        ("Version", "1.0".into()),
        ("Type", "Application".into()),
        ("Name", desktop_escape(&product.name)),
        ("Exec", format!("{name} %U")),
        ("TryExec", name),
        ("Icon", product.identifier.clone()),
        ("StartupNotify", "true".into()),
        (
            "MimeType",
            format!("x-scheme-handler/{};", product.url_scheme),
        ),
    ]
}

/// The text of `share/applications/<identifier>.desktop`.
fn desktop_entry(product: &BuildConfig) -> String {
    let mut text = String::from("[Desktop Entry]\n");
    for (key, value) in desktop_fields(product) {
        text.push_str(&format!("{key}={value}\n"));
    }
    text
}

/// A string value of a desktop entry, escaped as the Desktop Entry
/// Specification says: a backslash, a line break, a tab, and a carriage
/// return always, and a space where the reader would drop it, first or last.
fn desktop_escape(value: &str) -> String {
    let last = value.chars().count().saturating_sub(1);
    let mut escaped = String::with_capacity(value.len());
    for (index, c) in value.chars().enumerate() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            '\r' => escaped.push_str("\\r"),
            ' ' if index == 0 || index == last => escaped.push_str("\\s"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// The text of `install.sh`, with the config's values in it.
fn install_script(product: &BuildConfig, dev: bool) -> String {
    let name = yard_product::file_name(&product.name);
    let binary = if dev {
        format!("libexec/{name}")
    } else {
        format!("bin/{name}")
    };
    let mut desktop = String::from("    printf '%s\\n' '[Desktop Entry]'\n");
    for (key, value) in desktop_fields(product) {
        desktop.push_str(&match key {
            "Exec" => {
                "    printf 'Exec=%s %%U\\n' \"$(exec_argument \"$app/bin/$name\")\"\n".into()
            }
            "TryExec" => {
                "    printf 'TryExec=%s\\n' \"$(desktop_string \"$app/bin/$name\")\"\n".into()
            }
            "Icon" => "    printf 'Icon=%s\\n' \"$(desktop_string \"$icon\")\"\n".into(),
            key => format!(
                "    printf '%s\\n' {}\n",
                shell_quote(&format!("{key}={value}"))
            ),
        });
    }
    INSTALL_SCRIPT
        .replace("@PRODUCT@", &shell_quote(&product.name))
        .replace("@NAME@", &shell_quote(&name))
        .replace("@IDENTIFIER@", &shell_quote(&product.identifier))
        .replace("@BINARY@", &shell_quote(&binary))
        .replace("@DESKTOP@\n", &desktop)
}

/// `install.sh`, before the values are put in.
const INSTALL_SCRIPT: &str = r##"#!/bin/sh
# Installs this app for the current user, from the folder this script is in:
# the app in ~/.local/<name>.app, the command ~/.local/bin/<name>, and the
# menu entry in $XDG_DATA_HOME/applications (else ~/.local/share/applications).
# Made by `cargo xtask bundle linux`.
set -eu

product=@PRODUCT@
name=@NAME@
identifier=@IDENTIFIER@
binary=@BINARY@

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
if [ ! -f "$here/bin/$name" ]; then
    echo "install.sh: $here holds no bin/$name; run the install.sh of an unpacked $name.app" >&2
    exit 1
fi

local_dir="$HOME/.local"
app="$local_dir/$name.app"
bin_dir="$local_dir/bin"
data="${XDG_DATA_HOME:-}"
case $data in
/*) ;;
*) data="$HOME/.local/share" ;;
esac
applications="$data/applications"
icon="$app/share/icons/hicolor/512x512/apps/$identifier.png"
case "$app$applications" in
*'
'*)
    echo "install.sh: the install folders must not hold a line break" >&2
    exit 1
    ;;
esac

# A string value of a desktop entry: each backslash doubled.
desktop_string() {
    printf '%s' "$1" | sed 's/\\/\\\\/g'
}

# One argument of Exec, quoted as the Desktop Entry Specification asks.
exec_argument() {
    printf '"%s"' "$(printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/%/%%/g' | sed 's/\\/\\\\/g')"
}

# The new app is copied beside the old one, then renamed into place. The old
# one is kept until every step has succeeded: a failure or a signal before the
# end removes the new one and puts the old one back.
new="$local_dir/.$name.app.new-$$"
old="$local_dir/.$name.app.old-$$"
replaced=
linked=
desktop_temp=
finished=
clean_up() {
    if [ -n "$desktop_temp" ]; then
        rm -f "$desktop_temp"
    fi
    if [ -n "$finished" ]; then
        return
    fi
    rm -rf "$new"
    if [ -n "$replaced" ]; then
        rm -rf "$app"
        if [ -e "$old" ]; then
            mv "$old" "$app"
        elif [ -n "$linked" ]; then
            rm -f "$bin_dir/$name"
        fi
    elif [ -e "$old" ] && [ ! -e "$app" ]; then
        mv "$old" "$app"
    fi
}
trap clean_up EXIT
trap 'exit 1' HUP INT TERM

mkdir -p "$local_dir"
rm -rf "$new" "$old"
cp -R "$here" "$new"
if [ -e "$app" ] || [ -L "$app" ]; then
    mv "$app" "$old"
fi
# Set first, so a signal right after the rename still undoes it.
replaced=1
if ! mv "$new" "$app"; then
    echo "install.sh: could not move the new $name.app into $local_dir" >&2
    exit 1
fi

mkdir -p "$bin_dir"
linked=1
rm -f "$bin_dir/$name"
ln -s "$app/bin/$name" "$bin_dir/$name"

mkdir -p "$applications"
desktop="$applications/$identifier.desktop"
desktop_temp=$(mktemp "$applications/.$identifier.desktop.XXXXXX")
{
@DESKTOP@
} >"$desktop_temp"
chmod 644 "$desktop_temp"
mv "$desktop_temp" "$desktop"
desktop_temp=

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$applications" ||
        echo "install.sh: warning: update-desktop-database failed" >&2
fi

if command -v ldd >/dev/null 2>&1; then
    missing=$(ldd "$app/$binary" 2>/dev/null | awk '/=> not found/ { print "    " $1 }' || true)
    if [ -n "$missing" ]; then
        echo "install.sh: warning: this system lacks libraries $product needs:" >&2
        printf '%s\n' "$missing" >&2
        echo "Install them with the system's package manager, or $product will not start." >&2
    fi
fi

case "$app/bin/$name" in
*%* | *[[:space:]]*)
    echo "install.sh: warning: $app/bin/$name holds a space or a %: the command $bin_dir/$name works, but the menu entry and xdg-open may not start $product from this path." >&2
    ;;
esac

finished=1
rm -rf "$old"
echo "Installed $product in $app"
case ":${PATH:-}:" in
*":$bin_dir:"*)
    echo "Start it from the applications menu, or with: $name"
    ;;
*)
    echo "Start it from the applications menu, or with: $bin_dir/$name"
    echo "$bin_dir is not on PATH; add it to run $name by its name."
    ;;
esac
"##;
