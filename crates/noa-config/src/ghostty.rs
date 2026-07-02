use std::path::{Path, PathBuf};

pub fn ghostty_config_candidates_from(
    xdg_config_home: Option<&Path>,
    home_dir: &Path,
) -> [PathBuf; 4] {
    let xdg_config_home = xdg_config_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".config"));
    let app_support = home_dir.join("Library/Application Support/com.mitchellh.ghostty");

    [
        xdg_config_home.join("ghostty/config.ghostty"),
        xdg_config_home.join("ghostty/config"),
        app_support.join("config.ghostty"),
        app_support.join("config"),
    ]
}

pub fn ghostty_config_candidates() -> [PathBuf; 4] {
    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    ghostty_config_candidates_from(xdg_config_home.as_deref(), &home_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghostty_candidates_use_xdg_then_app_support_order() {
        let candidates =
            ghostty_config_candidates_from(Some(Path::new("/xdg")), Path::new("/Users/example"));

        assert_eq!(
            candidates,
            [
                PathBuf::from("/xdg/ghostty/config.ghostty"),
                PathBuf::from("/xdg/ghostty/config"),
                PathBuf::from(
                    "/Users/example/Library/Application Support/com.mitchellh.ghostty/config.ghostty"
                ),
                PathBuf::from(
                    "/Users/example/Library/Application Support/com.mitchellh.ghostty/config"
                ),
            ]
        );
    }

    #[test]
    fn ghostty_candidates_fall_back_to_home_dot_config() {
        let candidates = ghostty_config_candidates_from(None, Path::new("/Users/example"));

        assert_eq!(
            candidates[0],
            PathBuf::from("/Users/example/.config/ghostty/config.ghostty")
        );
        assert_eq!(
            candidates[1],
            PathBuf::from("/Users/example/.config/ghostty/config")
        );
    }
}
