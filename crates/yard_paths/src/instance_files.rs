//! The paths through which a second start reaches the running instance.

use crate::Paths;
use std::path::PathBuf;

impl Paths {
    /// The private folder that holds the instance's socket and lock.
    pub fn instance_dir(&self) -> PathBuf {
        self.data_dir().join("instance")
    }

    /// The socket the running instance listens on, so that a second start
    /// hands it its arguments instead of starting again.
    pub fn instance_socket(&self) -> PathBuf {
        self.instance_dir().join("instance.sock")
    }

    /// The lock a start holds while it looks for the running instance and,
    /// when there is none, takes its place.
    pub fn instance_lock(&self) -> PathBuf {
        self.instance_dir().join("instance.lock")
    }
}

#[cfg(test)]
mod tests {
    use crate::Paths;
    use std::path::Path;

    #[test]
    fn instance_files_are_in_their_own_folder_in_the_data_directory() {
        let paths = Paths::under("given");
        assert_eq!(paths.instance_dir(), Path::new("given/data/instance"));
        assert_eq!(
            paths.instance_socket(),
            Path::new("given/data/instance/instance.sock")
        );
        assert_eq!(
            paths.instance_lock(),
            Path::new("given/data/instance/instance.lock")
        );
    }
}
