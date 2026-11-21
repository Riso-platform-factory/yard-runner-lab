//! The application's three directories, from its home variable or the platform.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// The application's directories, worked out once at startup and handed to other crates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    config: PathBuf,
    data: PathBuf,
    logs: PathBuf,
    origin: Origin,
}

/// Where the directories came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The application's home variable.
    Home,
    /// The platform's locations.
    Platform,
    /// A directory a caller gave, such as a test's.
    Given,
}

impl Paths {
    /// The directories for this run: under the product's
    /// [home variable](yard_product::Product::home_variable) when it is set and
    /// not empty, otherwise the platform's locations for the product name, in
    /// [lower case](yard_product::Product::file_name) on Linux. A relative
    /// home is taken from the folder the application starts in.
    pub fn from_env(product: &yard_product::Product) -> Self {
        let _span = tracing::info_span!("paths-resolve").entered();
        let home_variable = product.home_variable();
        let paths = Self::resolve(
            &folder_name(product),
            std::env::var_os(&home_variable),
            std::env::var_os("XDG_CONFIG_HOME"),
        );
        tracing::debug!(origin = ?paths.origin, variable = %home_variable, "resolved");
        tracing::debug!(path = %paths.config.display(), "config");
        tracing::debug!(path = %paths.data.display(), "data");
        tracing::debug!(path = %paths.logs.display(), "logs");
        paths
    }
    /// `config/`, `data/`, and `logs/` under one directory.
    pub fn under(home: impl Into<PathBuf>) -> Self {
        Self::under_from(home.into(), Origin::Given)
    }

    fn under_from(home: PathBuf, origin: Origin) -> Self {
        Self {
            config: home.join("config"),
            data: home.join("data"),
            logs: home.join("logs"),
            origin,
        }
    }

    fn resolve(name: &str, home: Option<OsString>, xdg_config: Option<OsString>) -> Self {
        if let Some(home) = home.filter(|home| !home.is_empty()) {
            // Processes the application starts may run in other folders with this variable in
            // their environment, so the paths must not depend on the folder.
            let home = PathBuf::from(home);
            return Self::under_from(std::path::absolute(&home).unwrap_or(home), Origin::Home);
        }
        let home = dirs::home_dir().expect("home directory is available");
        // As Zed's: on Windows, settings in `%APPDATA%` and data in
        // `%LOCALAPPDATA%`, with no XDG variable.
        let config = if cfg!(target_os = "windows") {
            dirs::config_dir().expect("config directory is available")
        } else {
            xdg_config
                .map(PathBuf::from)
                // As the XDG specification says, a relative path is ignored.
                .filter(|dir| dir.is_absolute())
                .unwrap_or_else(|| home.join(".config"))
        }
        .join(name);
        let data = if cfg!(target_os = "windows") {
            dirs::data_local_dir()
        } else {
            dirs::data_dir()
        }
        .expect("data directory is available")
        .join(name);
        let logs = if cfg!(target_os = "macos") {
            home.join("Library/Logs").join(name)
        } else {
            dirs::data_local_dir()
                .expect("local data directory is available")
                .join(name)
                .join("logs")
        };
        Self {
            config,
            data,
            logs,
            origin: Origin::Platform,
        }
    }

    /// Where the directories came from.
    pub fn origin(&self) -> Origin {
        self.origin
    }
    pub fn config_dir(&self) -> &Path {
        &self.config
    }
    pub fn data_dir(&self) -> &Path {
        &self.data
    }
    pub fn logs_dir(&self) -> &Path {
        &self.logs
    }
}

/// The name of the product's folders: on Linux and FreeBSD the lower-case
/// file name, as Zed's are, and elsewhere the name as written.
fn folder_name(product: &yard_product::Product) -> String {
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        product.file_name()
    } else {
        product.name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_named_in_lower_case_on_linux_only() {
        let product = yard_product::Product {
            name: "Lab Notes Pro",
            version: "0",
            identifier: "test.yard.labnotes",
        };
        let expected = if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            "lab-notes-pro"
        } else {
            "Lab Notes Pro"
        };
        assert_eq!(folder_name(&product), expected);
    }

    #[test]
    fn a_relative_home_is_made_absolute() {
        let paths = Paths::resolve("testapp", Some("tmp/home".into()), None);
        let home = std::env::current_dir().unwrap().join("tmp/home");
        assert_eq!(paths.origin(), Origin::Home);
        assert_eq!(paths.config_dir(), home.join("config"));
        assert_eq!(paths.data_dir(), home.join("data"));
        assert_eq!(paths.logs_dir(), home.join("logs"));
    }

    #[test]
    fn home_holds_all_three_directories_and_wins_over_xdg() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::resolve(
            "testapp",
            Some(home.path().into()),
            Some("/elsewhere".into()),
        );
        assert_eq!(paths.origin(), Origin::Home);
        assert_eq!(paths.config_dir(), home.path().join("config"));
        assert_eq!(paths.data_dir(), home.path().join("data"));
        assert_eq!(paths.logs_dir(), home.path().join("logs"));
    }

    #[test]
    fn a_given_home_holds_all_three_directories() {
        let paths = Paths::under("given");
        assert_eq!(paths.origin(), Origin::Given);
        assert_eq!(paths.config_dir(), Path::new("given/config"));
        assert_eq!(paths.data_dir(), Path::new("given/data"));
        assert_eq!(paths.logs_dir(), Path::new("given/logs"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn empty_home_is_ignored_and_xdg_config_is_used() {
        let xdg = tempfile::tempdir().unwrap();
        let paths = Paths::resolve("testapp", Some("".into()), Some(xdg.path().into()));
        assert_eq!(paths.origin(), Origin::Platform);
        assert_eq!(paths.config_dir(), xdg.path().join("testapp"));
        assert!(paths.data_dir().ends_with("testapp"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn relative_xdg_config_is_ignored() {
        let paths = Paths::resolve("testapp", None, Some("relative".into()));
        assert_eq!(paths.origin(), Origin::Platform);
        assert_eq!(
            paths.config_dir(),
            dirs::home_dir().unwrap().join(".config/testapp")
        );
    }

    #[test]
    fn platform_directories_use_the_product_name() {
        let paths = Paths::resolve("my-app 2", None, Some("/elsewhere".into()));
        assert_eq!(paths.origin(), Origin::Platform);
        if cfg!(target_os = "windows") {
            assert_eq!(
                paths.config_dir(),
                dirs::config_dir().unwrap().join("my-app 2")
            );
            assert_eq!(
                paths.data_dir(),
                dirs::data_local_dir().unwrap().join("my-app 2")
            );
            assert_eq!(
                paths.logs_dir(),
                dirs::data_local_dir()
                    .unwrap()
                    .join("my-app 2")
                    .join("logs")
            );
            return;
        }
        let paths = Paths::resolve("my-app 2", None, None);
        assert!(paths.config_dir().ends_with(".config/my-app 2"));
        if cfg!(target_os = "macos") {
            assert!(
                paths
                    .data_dir()
                    .ends_with("Library/Application Support/my-app 2")
            );
            assert!(paths.logs_dir().ends_with("Library/Logs/my-app 2"));
        } else {
            assert_eq!(paths.data_dir(), dirs::data_dir().unwrap().join("my-app 2"));
            assert_eq!(
                paths.logs_dir(),
                dirs::data_local_dir().unwrap().join("my-app 2/logs")
            );
        }
    }
}
