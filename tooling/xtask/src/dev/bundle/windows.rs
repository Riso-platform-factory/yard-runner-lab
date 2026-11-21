//! `cargo xtask bundle windows`: makes `<Name>-<version>-<arch>-setup.exe`
//! with Inno Setup, and `<Name>-<version>-windows-<arch>.zip`, from a product
//! config. The installer takes its decisions from Zed's `zed.iss`, cut to what
//! YARD has; its script is YARD's own, written from the Inno Setup
//! documentation (https://jrsoftware.org/ishelp/).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use yard_build_config::BuildConfig;

use super::{
    Failure, Interrupt, Runner, Staging, build, interrupted_at_end, log_outcome, not_interrupted,
    read_product, repository, step,
};

/// The options of `cargo xtask bundle windows`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct WindowsArgs {
    /// The product config [default: app/yard/product.toml in the repository].
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// The folder the package goes into [default: dist in the repository].
    #[arg(long, value_name = "FOLDER")]
    pub out: Option<PathBuf>,
    /// Builds a review bundle, whose shortcut and URL command run `dev`, with
    /// a product other than the production one.
    #[arg(long)]
    pub dev: bool,
    /// Refused on Windows: a shortcut and a URL command cannot set
    /// environment values.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
}

/// What the host gives `bundle windows`: its system, and where Inno Setup's
/// compiler and Windows' own tools are looked for.
#[derive(Debug, Clone, Default)]
pub struct WindowsHost {
    /// `std::env::consts::OS`: anything but `windows` is refused.
    pub system: String,
    /// `PATH`, searched first for `ISCC.exe`.
    pub path: Option<OsString>,
    /// `%ProgramFiles(x86)%`, where Inno Setup 6 installs itself.
    pub program_files_x86: Option<OsString>,
    /// `%SystemRoot%`, whose `System32` holds `tar.exe` and `certutil.exe`.
    pub system_root: Option<OsString>,
}

impl WindowsHost {
    /// This host, from its environment.
    pub fn from_env() -> Self {
        Self {
            system: std::env::consts::OS.to_owned(),
            path: std::env::var_os("PATH"),
            program_files_x86: std::env::var_os("ProgramFiles(x86)"),
            system_root: std::env::var_os("SystemRoot"),
        }
    }

    /// `ISCC.exe` on `PATH`, else in `%ProgramFiles(x86)%\Inno Setup 6`.
    fn iscc(&self) -> Option<PathBuf> {
        let on_path = self
            .path
            .iter()
            .flat_map(std::env::split_paths)
            .map(|folder| folder.join("ISCC.exe"));
        let installed = self
            .program_files_x86
            .iter()
            .map(|folder| Path::new(folder).join("Inno Setup 6").join("ISCC.exe"));
        on_path.chain(installed).find(|path| path.is_file())
    }

    /// `%SystemRoot%\System32\<program>`, never one that `PATH` finds first,
    /// such as Git's `tar`, which cannot write a zip.
    fn system_tool(&self, program: &str) -> Result<PathBuf, String> {
        let root = self
            .system_root
            .as_ref()
            .ok_or_else(|| format!("%SystemRoot% is not set, so {program} cannot be found"))?;
        let tool = Path::new(root).join("System32").join(program);
        if tool.is_file() {
            Ok(tool)
        } else {
            Err(format!(
                "Windows' {program} was not found at {}",
                tool.display()
            ))
        }
    }
}

/// The external programs one run uses, each by its full path.
struct Tools {
    iscc: PathBuf,
    tar: PathBuf,
    certutil: PathBuf,
}

/// Finds the tools: Inno Setup's compiler, and Windows' own `tar.exe` and
/// `certutil.exe`. Inno Setup's version is checked by the script itself,
/// when it compiles (see [`inno_script`]).
fn tools(host: &WindowsHost) -> Result<Tools, Failure> {
    let refuse = |reason: String| Failure::new("tools", reason);
    let iscc = host.iscc().ok_or_else(|| {
        refuse(
            "Inno Setup's compiler was not found: ISCC.exe is neither on PATH nor in %ProgramFiles(x86)%\\Inno Setup 6".into(),
        )
    })?;
    tracing::debug!(iscc = %iscc.display(), "inno setup");
    Ok(Tools {
        iscc,
        tar: host.system_tool("tar.exe").map_err(refuse)?,
        certutil: host.system_tool("certutil.exe").map_err(refuse)?,
    })
}

/// What `bundle_windows` made, each with its SHA-256 in hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsBundled {
    /// `<out>/<Name>-<version>-<arch>-setup.exe`.
    pub installer: (PathBuf, String),
    /// `<out>/<Name>-<version>-windows-<arch>.zip`, holding `<Name>/<Name>.exe`.
    pub zip: (PathBuf, String),
}

/// The sizes of the installer's icon: the `.exe`'s, as `app/yard/build.rs`
/// makes them.
const ICON_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

/// Inno Setup's limit on `AppId`.
const APP_ID_LIMIT: usize = 127;

/// Makes the installer and the zip from the config, inside the group
/// `bundle-windows{config, dev}`, printing each step's name on stdout, as
/// [`super::bundle_macos`] does. Anything but Windows is refused at the
/// start.
pub fn bundle_windows(
    options: &WindowsArgs,
    host: &WindowsHost,
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<WindowsBundled, Failure> {
    let config = options
        .config
        .clone()
        .unwrap_or_else(|| repository().join(super::PRODUCTION_CONFIG));
    let _group = tracing::info_span!(
        "bundle-windows",
        config = %config.display(),
        dev = options.dev
    )
    .entered();
    let result = bundle(&config, options, host, interrupt, runner);
    log_outcome(&result, interrupt);
    result
}

fn bundle(
    config: &Path,
    options: &WindowsArgs,
    host: &WindowsHost,
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<WindowsBundled, Failure> {
    tracing::debug!("arguments");
    let arguments = |reason: String| Failure::new("arguments", reason);
    if host.system != "windows" {
        return Err(arguments(format!(
            "bundle windows runs only on Windows, and this system is {}",
            host.system
        )));
    }
    if !options.env.is_empty() {
        return Err(arguments(
            "--env: a Windows review bundle cannot carry environment values, since a shortcut and a URL command cannot set them".into(),
        ));
    }
    let (config, out) = super::arguments(config, options.out.as_deref(), options.dev, &[])?;

    tracing::debug!("config");
    let product = read_config(&config, options.dev)?;

    tracing::debug!("tools");
    let tools = tools(host)?;

    if product.windows.signing.is_some() {
        // TODO(packaging-5e-windows-signing): sign the `.exe` and the
        // installer with signtool. Part 5e of packaging unblocks it.
        log::warn!("not signed: Windows signing comes with part 5e");
    }

    let binary = build(&config, options.dev, &[], interrupt, runner)?;

    step("icon", interrupt)?;
    let job = Job {
        product: &product,
        dev: options.dev,
        binary: &binary,
        out: &out,
        tools: &tools,
        interrupt,
    };
    let result = super::staged(&out, "icon", "windows", |staging| {
        in_temp(staging, &job, runner)
    });
    interrupted_at_end(result, interrupt)
}

/// Reads the config as every system does, and checks what Windows adds: the
/// name and the version are Windows file names that Inno Setup and `tar`
/// read as written, the files named after them fit Windows' limit, and the
/// identifier fits `AppId`.
fn read_config(config: &Path, dev: bool) -> Result<BuildConfig, Failure> {
    let product = read_product(config, dev, false)?;
    let file = config.display();
    let refuse = |reason: String| Failure::new("config", reason);
    for (key, value) in [("name", &product.name), ("version", &product.version)] {
        if value.contains(['{', '}']) {
            return Err(refuse(format!(
                "{file}: `{key}` must not hold `{{` or `}}` on Windows: Inno Setup's preprocessor and constants would read them"
            )));
        }
        if !is_windows_file_name(value) {
            return Err(refuse(format!(
                "{file}: `{key}` must be a Windows file name: none of < > : \" / \\ | ? * or a control character, no space at either end, no dot at the end, and not a device name such as CON or COM1"
            )));
        }
    }
    if product.name.starts_with(['@', '-']) {
        return Err(refuse(format!(
            "{file}: `name` must not start with `@` or `-` on Windows: tar would read it as an option or an archive to include"
        )));
    }
    let arch = std::env::consts::ARCH;
    let name = &product.name;
    for made in [
        format!("{name}.exe"),
        format!("{name}.lnk"),
        format!("{}.exe", setup_base_name(&product)),
        format!("{name}-{}-windows-{arch}.zip", product.version),
    ] {
        if made.encode_utf16().count() > FILE_NAME_LIMIT {
            return Err(refuse(format!(
                "{file}: `name` and `version` make the file name {made:?}, longer than Windows' {FILE_NAME_LIMIT} characters"
            )));
        }
    }
    if product.identifier.len() > APP_ID_LIMIT {
        return Err(refuse(format!(
            "{file}: `identifier` is the installer's AppId, which Inno Setup limits to {APP_ID_LIMIT} characters"
        )));
    }
    Ok(product)
}

/// Windows' limit on one file name, in UTF-16 units.
const FILE_NAME_LIMIT: usize = 255;

/// Whether Windows accepts `value` as a file name, and keeps it as written.
/// The device names are reserved with any extension, and `COM` and `LPT`
/// with a superscript digit too.
fn is_windows_file_name(value: &str) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    // Windows drops the spaces at the end of the stem, so `CON .txt` is CON.
    let stem = value
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches([' ', '.'])
        .to_uppercase();
    let numbered = ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            let mut digits = rest.chars();
            digits
                .next()
                .is_some_and(|c| c.is_ascii_digit() || "¹²³".contains(c))
                && digits.next().is_none()
        })
    });
    let device = DEVICES.contains(&stem.as_str()) || numbered;
    !value.is_empty()
        && !device
        && !value.starts_with(' ')
        && !value.ends_with([' ', '.'])
        && !value
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
}

/// What one run bundles, and with which tools.
struct Job<'a> {
    product: &'a BuildConfig,
    dev: bool,
    binary: &'a Path,
    out: &'a Path,
    tools: &'a Tools,
    interrupt: &'a Interrupt,
}

/// The steps that work in the staging folder: the icon, the staged files,
/// the installer, and the zip; then, unless the run was interrupted, the
/// installer and the zip are published together.
fn in_temp(
    staging: &mut Staging,
    job: &Job,
    runner: &mut dyn Runner,
) -> Result<WindowsBundled, Failure> {
    let Job {
        product,
        dev,
        binary,
        out,
        tools,
        interrupt,
    } = *job;
    let name = &product.name;
    let temp = staging.path.clone();
    let icon = temp.join(format!("{name}.ico"));
    write_icon(&product.icon, &icon).map_err(|reason| Failure::new("icon", reason))?;
    tracing::debug!(icon = %icon.display(), "icon");

    step("bundle", interrupt)?;
    let script = lay_out(product, dev, binary, &temp).map_err(|error| {
        Failure::new("bundle", format!("could not lay out the bundle: {error}"))
    })?;

    step("installer", interrupt)?;
    let installer = |reason| Failure::new("installer", reason);
    // `/Q` prints only errors, on stderr, where the user reads them, such as
    // the script's refusal of an Inno Setup older than 6.3.
    runner
        .run(Command::new(&tools.iscc).arg("/Q").arg(&script))
        .map_err(|reason| installer(format!("{reason}; Inno Setup printed why above")))?;
    let setup_name = format!("{}.exe", setup_base_name(product));
    let setup = temp.join(&setup_name);
    if !setup.is_file() {
        return Err(installer(format!(
            "Inno Setup wrote no {}",
            setup.display()
        )));
    }
    let setup_digest = sha256(&tools.certutil, &setup, runner).map_err(installer)?;

    step("zip", interrupt)?;
    let zip = |reason| Failure::new("zip", reason);
    let staged_zip = temp.join("release.zip");
    runner
        .run(
            Command::new(&tools.tar)
                .args(["-a", "-c", "-f"])
                .arg(&staged_zip)
                .arg("-C")
                .arg(temp.join(PAYLOAD))
                .arg(name),
        )
        .map_err(zip)?;
    let zip_digest = sha256(&tools.certutil, &staged_zip, runner).map_err(zip)?;

    // Both are published together, or neither is.
    not_interrupted("publish", interrupt)?;
    let published_setup = out.join(&setup_name);
    let published_zip = out.join(format!(
        "{name}-{}-windows-{}.zip",
        product.version,
        std::env::consts::ARCH
    ));
    // Neither is published while the other could not be.
    super::replaceable_file(&published_setup).map_err(installer)?;
    super::replaceable_file(&published_zip).map_err(zip)?;
    tracing::debug!(destination = %published_setup.display(), "publish");
    staging
        .publish(&setup, &published_setup)
        .map_err(installer)?;
    tracing::debug!(installer = %published_setup.display(), sha256 = setup_digest, "installer");
    staging.publish(&staged_zip, &published_zip).map_err(zip)?;
    tracing::debug!(zip = %published_zip.display(), sha256 = zip_digest, "zipped");
    Ok(WindowsBundled {
        installer: (published_setup, setup_digest),
        zip: (published_zip, zip_digest),
    })
}

/// `<Name>-<version>-<arch>-setup`, the installer's name without `.exe`.
fn setup_base_name(product: &BuildConfig) -> String {
    format!(
        "{}-{}-{}-setup",
        product.name,
        product.version,
        std::env::consts::ARCH
    )
}

/// The folder of the staging folder that holds the app's tree, apart from
/// the files xtask names itself (`release.zip`, the previous files kept while
/// publishing), so that no name meets them.
const PAYLOAD: &str = "payload";

/// Lays out `<folder>/payload/<Name>/<Name>.exe` and `<folder>/<Name>.iss`,
/// and returns the script's path. The icon is already at
/// `<folder>/<Name>.ico`.
fn lay_out(product: &BuildConfig, dev: bool, binary: &Path, folder: &Path) -> io::Result<PathBuf> {
    let name = &product.name;
    let app = folder.join(PAYLOAD).join(name);
    fs::create_dir_all(&app)?;
    fs::copy(binary, app.join(format!("{name}.exe")))?;
    let script = folder.join(format!("{name}.iss"));
    fs::write(&script, inno_script(product, dev, std::env::consts::ARCH))?;
    Ok(script)
}

/// The SHA-256 of `file` in hex, from Windows' `certutil -hashfile`, which
/// prints it on a line of its own, in pairs separated by spaces on older
/// versions.
fn sha256(certutil: &Path, file: &Path, runner: &mut dyn Runner) -> Result<String, String> {
    let output = runner.output(
        Command::new(certutil)
            .arg("-hashfile")
            .arg(file)
            .arg("SHA256"),
    )?;
    String::from_utf8_lossy(&output)
        .lines()
        .map(|line| line.replace(' ', "").to_ascii_lowercase())
        .find(|line| line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| "certutil printed no SHA-256".to_owned())
}

/// Writes the config's icon as an ICO to `out`, one PNG frame for each of
/// [`ICON_SIZES`], resized with Lanczos3. The same as `write_windows_icon` of
/// `app/yard/build.rs`, which a build script keeps to itself: sharing it
/// would move it into a production crate.
fn write_icon(icon: &Path, out: &Path) -> Result<(), String> {
    let bytes = fs::read(icon)
        .map_err(|error| format!("could not read the icon {}: {error}", icon.display()))?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|error| format!("could not decode the icon {}: {error}", icon.display()))?;
    let frames = ICON_SIZES
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
        .map_err(|error| format!("could not encode the icon {}: {error}", icon.display()))?;
    let file = fs::File::create(out)
        .map_err(|error| format!("could not write {}: {error}", out.display()))?;
    image::codecs::ico::IcoEncoder::new(io::BufWriter::new(file))
        .encode_images(&frames)
        .map_err(|error| format!("could not write {}: {error}", out.display()))
}

/// A parameter of a section: in double quotes, each `"` inside doubled.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// The text of `<Name>.iss`, every value from the config. Paths of the
/// staged files are relative to the script, as Inno Setup's `SourceDir` is
/// the script's folder. The name and the version hold no `{` or `}`, which
/// Inno Setup's preprocessor and its constants would read.
pub fn inno_script(product: &BuildConfig, dev: bool, arch: &str) -> String {
    let name = &product.name;
    let version = &product.version;
    let identifier = &product.identifier;
    let scheme = &product.url_scheme;
    let exe = format!("{{app}}\\{name}.exe");
    let arguments = if dev { "dev " } else { "" };
    let mut text = format!(
        "; Made by `cargo xtask bundle windows` from {identifier}'s product config.\n\
         \n\
         ; `x64compatible` and a UTF-8 script without a byte order mark need Inno\n\
         ; Setup 6.3. Its preprocessor's VER is the compiler's own version, which\n\
         ; neither ISCC's banner nor a package manager's shim gives.\n\
         #if VER < EncodeVer(6,3,0)\n\
         #pragma error \"bundle windows needs Inno Setup 6.3 or later, and this is Inno Setup \" + DecodeVer(VER)\n\
         #endif\n\
         \n\
         [Setup]\n\
         ; The identifier: stable across versions, unique as a reverse-domain name,\n\
         ; and readable in the uninstall key `{identifier}_is1`.\n\
         AppId={identifier}\n\
         AppName={name}\n\
         AppVersion={version}\n\
         AppVerName={name} {version}\n\
         DefaultDirName={{localappdata}}\\Programs\\{name}\n\
         DefaultGroupName={name}\n\
         DisableProgramGroupPage=yes\n\
         PrivilegesRequired=lowest\n"
    );
    match arch {
        "x86_64" => text.push_str(
            "ArchitecturesAllowed=x64compatible\n\
             ArchitecturesInstallIn64BitMode=x64compatible\n",
        ),
        "aarch64" => text.push_str(
            "ArchitecturesAllowed=arm64\n\
             ArchitecturesInstallIn64BitMode=arm64\n",
        ),
        _ => {}
    }
    text.push_str(&format!(
        "OutputDir=.\n\
         OutputBaseFilename={setup}\n\
         SetupIconFile={name}.ico\n\
         UninstallDisplayIcon={exe}\n\
         ; The mutex the app creates as `Local\\<this name>`. The installer runs in the\n\
         ; user's session, where Windows opens a name without a prefix in the session\n\
         ; namespace, which is the one `Local\\` names.\n\
         AppMutex={mutex}\n\
         CloseApplications=yes\n\
         Compression=lzma2\n\
         SolidCompression=yes\n\
         WizardStyle=modern\n\
         \n\
         [Files]\n\
         Source: {source}; DestDir: \"{{app}}\"; Flags: ignoreversion\n\
         \n\
         [Icons]\n\
         Name: {shortcut}; Filename: {target}; AppUserModelID: {model}{parameters}\n\
         \n\
         [Registry]\n\
         ; Uninstall removes the values written here, then each key once it is empty,\n\
         ; so what another program keeps under the scheme stays.\n\
         Root: HKCU; Subkey: {key}; ValueType: none; Flags: uninsdeletekeyifempty\n\
         Root: HKCU; Subkey: {key}; ValueType: string; ValueName: \"\"; ValueData: {url_name}; Flags: uninsdeletevalue\n\
         Root: HKCU; Subkey: {key}; ValueType: string; ValueName: \"URL Protocol\"; ValueData: \"\"; Flags: uninsdeletevalue\n\
         Root: HKCU; Subkey: {icon_key}; ValueType: none; Flags: uninsdeletekeyifempty\n\
         Root: HKCU; Subkey: {icon_key}; ValueType: string; ValueName: \"\"; ValueData: {icon}; Flags: uninsdeletevalue\n\
         Root: HKCU; Subkey: {shell_key}; ValueType: none; Flags: uninsdeletekeyifempty\n\
         Root: HKCU; Subkey: {open_key}; ValueType: none; Flags: uninsdeletekeyifempty\n\
         Root: HKCU; Subkey: {command_key}; ValueType: none; Flags: uninsdeletekeyifempty\n\
         Root: HKCU; Subkey: {command_key}; ValueType: string; ValueName: \"\"; ValueData: {command}; Flags: uninsdeletevalue\n\
         \n\
         [Run]\n\
         Filename: {target}; {run_parameters}Description: {launch}; Flags: nowait postinstall skipifsilent\n",
        setup = setup_base_name(product),
        mutex = yard_product::instance_name(identifier),
        source = quoted(&format!("{PAYLOAD}\\{name}\\{name}.exe")),
        shortcut = quoted(&format!("{{userprograms}}\\{name}")),
        target = quoted(&exe),
        model = quoted(identifier),
        parameters = if dev { "; Parameters: \"dev\"" } else { "" },
        run_parameters = if dev { "Parameters: \"dev\"; " } else { "" },
        key = quoted(&format!("Software\\Classes\\{scheme}")),
        icon_key = quoted(&format!("Software\\Classes\\{scheme}\\DefaultIcon")),
        shell_key = quoted(&format!("Software\\Classes\\{scheme}\\shell")),
        open_key = quoted(&format!("Software\\Classes\\{scheme}\\shell\\open")),
        command_key = quoted(&format!("Software\\Classes\\{scheme}\\shell\\open\\command")),
        url_name = quoted(&format!("URL:{name}")),
        icon = quoted(&format!("\"{exe}\",0")),
        command = quoted(&format!("\"{exe}\" {arguments}\"%1\"")),
        launch = quoted(&format!("Launch {name}")),
    ));
    text
}
