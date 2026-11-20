//! The path of the socket through which a second start reaches the running
//! instance.

use crate::Paths;
use std::path::PathBuf;

impl Paths {
    /// The socket the running instance listens on, so that a second start
    /// hands it its arguments instead of starting again.
    pub fn instance_socket(&self) -> PathBuf {
        self.data_dir().join("instance.sock")
    }
}

#[cfg(test)]
mod tests {
    use crate::Paths;
    use std::path::Path;

    #[test]
    fn instance_socket_is_in_the_data_directory() {
        let paths = Paths::under("given");
        assert_eq!(
            paths.instance_socket(),
            Path::new("given/data/instance.sock")
        );
    }
}
