use std::path::{Path, PathBuf};

pub fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("patpans.toml")))
        .unwrap_or_else(|| PathBuf::from("patpans.toml"))
}

pub fn default_log_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("patpans.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_path_sits_next_to_the_config() {
        let path = default_log_path(Path::new("/opt/patpans/patpans.toml"));
        assert_eq!(path, PathBuf::from("/opt/patpans/patpans.log"));
    }

    #[test]
    fn default_config_path_ends_with_the_file_name() {
        assert!(default_config_path().ends_with("patpans.toml"));
    }
}
