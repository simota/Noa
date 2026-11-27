//! The live pipe cannot be combined with filesystem inputs: the parent
//! supplies the already validated, pinned definition and instance data.

use std::path::PathBuf;

pub const USAGE: &str = "usage: widget-host --manifest=FILE [--data=FILE] [--snapshot=FILE.png [--size=WxH]]\n       widget-host --repo=DIR [--preview-id=ID [--snapshot=FILE.png [--size=WxH]]]\n       widget-host --bridge-stdio";
pub const DEFAULT_SIZE: (u32, u32) = (960, 720);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub manifest: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub snapshot: Option<PathBuf>,
    pub size: Option<(u32, u32)>,
    pub repo: Option<PathBuf>,
    pub preview_id: Option<String>,
    pub bridge: bool,
    pub help: bool,
}

fn path_arg(slot: &mut Option<PathBuf>, name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || slot.is_some() {
        return Err(format!("{name} needs one nonempty path"));
    }
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn size_arg(value: &str) -> Result<(u32, u32), String> {
    let bad =
        || "--size expects WxH, 320..4096 per side, at most 8 million logical pixels".to_owned();
    let (width, height) = value.split_once('x').ok_or_else(bad)?;
    let width = width.parse::<u32>().map_err(|_| bad())?;
    let height = height.parse::<u32>().map_err(|_| bad())?;
    if !(320..=4096).contains(&width)
        || !(320..=4096).contains(&height)
        || u64::from(width) * u64::from(height) > 8_000_000
    {
        return Err(bad());
    }
    Ok((width, height))
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    for arg in args {
        if matches!(arg.as_str(), "-h" | "--help") {
            parsed.help = true;
        } else if arg == "--bridge-stdio" && !parsed.bridge {
            parsed.bridge = true;
        } else if let Some(value) = arg.strip_prefix("--manifest=") {
            path_arg(&mut parsed.manifest, "--manifest", value)?;
        } else if let Some(value) = arg.strip_prefix("--data=") {
            path_arg(&mut parsed.data, "--data", value)?;
        } else if let Some(value) = arg.strip_prefix("--snapshot=") {
            path_arg(&mut parsed.snapshot, "--snapshot", value)?;
        } else if let Some(value) = arg.strip_prefix("--repo=") {
            path_arg(&mut parsed.repo, "--repo", value)?;
        } else if let Some(value) = arg.strip_prefix("--preview-id=") {
            if parsed.preview_id.is_some() {
                return Err("--preview-id may be specified once".into());
            }
            noa_widget::check_id(value).map_err(|error| error.to_string())?;
            parsed.preview_id = Some(value.to_owned());
        } else if let Some(value) = arg.strip_prefix("--size=") {
            if parsed.size.is_some() {
                return Err("--size may be specified once".into());
            }
            parsed.size = Some(size_arg(value)?);
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }
    if parsed.help {
        return Ok(parsed);
    }
    let modes = usize::from(parsed.manifest.is_some())
        + usize::from(parsed.repo.is_some())
        + usize::from(parsed.bridge);
    if modes != 1 {
        return Err(format!(
            "choose exactly one of --manifest, --repo, --bridge-stdio\n{USAGE}"
        ));
    }
    if parsed.data.is_some() && parsed.manifest.is_none() {
        return Err("--data requires --manifest".into());
    }
    if parsed.snapshot.is_some()
        && parsed.manifest.is_none()
        && !(parsed.repo.is_some() && parsed.preview_id.is_some())
    {
        return Err("--snapshot requires --manifest or --repo with --preview-id".into());
    }
    if parsed.size.is_some() && parsed.snapshot.is_none() {
        return Err("--size requires --snapshot".into());
    }
    if parsed.preview_id.is_some() && parsed.repo.is_none() {
        return Err("--preview-id requires --repo".into());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Result<Args, String> {
        parse(values.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn bridge_rejects_all_local_file_inputs() {
        assert!(args(&["--bridge-stdio"]).unwrap().bridge);
        for extra in [
            "--manifest=x",
            "--data=x",
            "--repo=x",
            "--preview-id=example",
            "--snapshot=x",
            "--size=800x600",
        ] {
            assert!(args(&["--bridge-stdio", extra]).is_err(), "{extra}");
        }
    }

    #[test]
    fn snapshot_requires_a_definition_and_has_a_bounded_size() {
        let parsed = args(&[
            "--manifest=widget.json",
            "--data=data.json",
            "--snapshot=view.png",
            "--size=800x600",
        ])
        .unwrap();
        assert_eq!(parsed.size, Some((800, 600)));
        assert!(args(&["--manifest=x", "--size=800x600"]).is_err());
        assert!(args(&["--manifest=x", "--snapshot=x", "--size=4096x4096"]).is_err());
        assert!(args(&["--manifest=x", "--snapshot=x", "--size=0x700"]).is_err());
        assert!(args(&["--manifest="]).is_err());
        assert!(args(&["--manifest=x", "--manifest=y"]).is_err());
    }

    #[test]
    fn catalogue_preview_is_scoped_to_a_repository_and_a_valid_id() {
        let parsed = args(&["--repo=/project", "--preview-id=example"]).unwrap();
        assert_eq!(parsed.preview_id.as_deref(), Some("example"));
        for input in [
            vec!["--manifest=x", "--preview-id=example"],
            vec!["--repo=/project", "--preview-id=../outside"],
            vec!["--repo=/project", "--preview-id=example", "--data=outside"],
            vec![
                "--repo=/project",
                "--preview-id=example",
                "--preview-id=second",
            ],
        ] {
            assert!(args(&input).is_err(), "{input:?}");
        }
    }
}
