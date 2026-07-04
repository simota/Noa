use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::default_config_path;
use crate::ghostty::ghostty_config_candidates;
use crate::parser::{is_supported_scalar_key, parse_directives};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportStats {
    pub sources: usize,
    pub supported: usize,
    pub commented_out: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub target: PathBuf,
    pub stats: ImportStats,
}

pub fn build_import_output(source_texts: &[String]) -> (String, ImportStats) {
    let mut output = String::new();
    let mut stats = ImportStats {
        sources: source_texts.len(),
        supported: 0,
        commented_out: 0,
    };

    for source in source_texts {
        for line in source.lines() {
            let (line, counted) = import_line(line);
            match counted {
                ImportLineKind::Supported => stats.supported += 1,
                ImportLineKind::CommentedOut => stats.commented_out += 1,
                ImportLineKind::Passthrough => {}
            }
            output.push_str(&line);
            output.push('\n');
        }
    }

    (output, stats)
}

pub fn import_ghostty_config_at(
    candidates: &[PathBuf],
    target: &Path,
) -> anyhow::Result<ImportOutcome> {
    if target.exists() {
        bail!(
            "refusing to overwrite existing noa config {}",
            target.display()
        );
    }

    let mut sources = Vec::new();
    for candidate in candidates {
        if candidate.exists() {
            sources.push(
                fs::read_to_string(candidate)
                    .with_context(|| format!("failed to read {}", candidate.display()))?,
            );
        }
    }

    if sources.is_empty() {
        let paths = candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        bail!("no Ghostty config found in candidates: {paths}");
    }

    let (output, stats) = build_import_output(&sources);
    if let Some(parent) = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .with_context(|| format!("failed to create {}", target.display()))?;
    file.write_all(output.as_bytes())
        .with_context(|| format!("failed to write {}", target.display()))?;

    Ok(ImportOutcome {
        target: target.to_path_buf(),
        stats,
    })
}

pub fn import_ghostty_config() -> anyhow::Result<ImportOutcome> {
    let target = default_config_path().context("failed to determine noa config path")?;
    import_ghostty_config_at(&ghostty_config_candidates(), &target)
}

enum ImportLineKind {
    Supported,
    CommentedOut,
    Passthrough,
}

fn import_line(line: &str) -> (String, ImportLineKind) {
    let trimmed_start = line.trim_start();
    if trimmed_start.is_empty() || trimmed_start.starts_with('#') {
        return (line.to_string(), ImportLineKind::Passthrough);
    }

    let directives = parse_directives(line);
    let Some(directive) = directives.first() else {
        return (format!("# {line}"), ImportLineKind::CommentedOut);
    };

    if is_supported_scalar_key(&directive.key) {
        (line.to_string(), ImportLineKind::Supported)
    } else {
        (format!("# {line}"), ImportLineKind::CommentedOut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noa-config-{name}-{}", std::process::id()))
    }

    #[test]
    fn build_import_output_preserves_supported_and_comments_unsupported() {
        let (output, stats) = build_import_output(&[String::from(
            "window-width = 100\ntheme = \"Foo\"\nkeybind = cmd+n=new_tab\nwindow-decoration = false\n# comment\n",
        )]);

        assert!(output.contains("window-width = 100\n"));
        assert!(output.contains("theme = \"Foo\"\n"));
        assert!(output.contains("# keybind = cmd+n=new_tab\n"));
        assert!(output.contains("# window-decoration = false\n"));
        assert!(output.contains("# comment\n"));
        assert_eq!(stats.supported, 2);
        assert_eq!(stats.commented_out, 2);
    }

    #[test]
    fn scrollback_limit_is_preserved_uncommented_on_import() {
        let (output, stats) = build_import_output(&[String::from("scrollback-limit = 5000000\n")]);

        assert!(output.contains("scrollback-limit = 5000000\n"));
        assert!(!output.contains("# scrollback-limit"));
        assert_eq!(stats.supported, 1);
        assert_eq!(stats.commented_out, 0);
    }

    #[test]
    fn alpha_blending_is_preserved_uncommented_on_import() {
        let (output, stats) =
            build_import_output(&[String::from("alpha-blending = linear-corrected\n")]);

        assert!(output.contains("alpha-blending = linear-corrected\n"));
        assert!(!output.contains("# alpha-blending"));
        assert_eq!(stats.supported, 1);
        assert_eq!(stats.commented_out, 0);
    }

    #[test]
    fn macos_native_keys_are_preserved_uncommented_on_import() {
        let (output, stats) = build_import_output(&[String::from(
            "macos-option-as-alt = true\nmacos-titlebar-style = transparent\n",
        )]);

        assert!(output.contains("macos-option-as-alt = true\n"));
        assert!(output.contains("macos-titlebar-style = transparent\n"));
        assert!(!output.contains("# macos-option-as-alt"));
        assert!(!output.contains("# macos-titlebar-style"));
        assert_eq!(stats.supported, 2);
        assert_eq!(stats.commented_out, 0);
    }

    #[test]
    fn import_errors_when_no_candidates_exist() {
        let dir = unique_temp_path("missing");
        let target = dir.join("noa/config");
        let candidates = [
            dir.join("ghostty/config.ghostty"),
            dir.join("ghostty/config"),
        ];

        let error = import_ghostty_config_at(&candidates, &target).unwrap_err();

        assert!(error.to_string().contains("no Ghostty config found"));
        assert!(error.to_string().contains("config.ghostty"));
        assert!(!target.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn import_preserves_last_wins_when_read_back() {
        let dir = unique_temp_path("last-wins");
        let low = dir.join("ghostty/config.ghostty");
        let high = dir.join("app/config");
        let target = dir.join("noa/config");
        fs::create_dir_all(low.parent().unwrap()).unwrap();
        fs::create_dir_all(high.parent().unwrap()).unwrap();
        fs::write(&low, "font-size = 12").unwrap();
        fs::write(&high, "font-size = 14").unwrap();

        import_ghostty_config_at(&[low, high], &target).unwrap();
        let output = fs::read_to_string(&target).unwrap();
        let (overrides, diagnostics) = crate::parse_overrides(&target, &output);

        assert!(diagnostics.is_empty());
        assert_eq!(overrides.font_size, Some(14.0));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_refuses_to_overwrite_existing_target() {
        let dir = unique_temp_path("overwrite");
        let source = dir.join("ghostty/config");
        let target = dir.join("noa/config");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&source, "font-size = 12").unwrap();
        fs::write(&target, "existing").unwrap();

        let error = import_ghostty_config_at(&[source], &target).unwrap_err();

        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "existing");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_does_not_follow_config_file_directives() {
        let dir = unique_temp_path("config-file");
        let source = dir.join("ghostty/config");
        let included = dir.join("ghostty/extra");
        let target = dir.join("noa/config");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, format!("config-file = {}\n", included.display())).unwrap();
        fs::write(&included, "font-size = 99").unwrap();

        import_ghostty_config_at(&[source], &target).unwrap();
        let output = fs::read_to_string(&target).unwrap();

        assert!(output.contains("# config-file ="));
        assert!(!output.contains("font-size = 99"));
        fs::remove_dir_all(dir).unwrap();
    }
}
