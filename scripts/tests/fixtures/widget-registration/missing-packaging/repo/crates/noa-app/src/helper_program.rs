//! Fixture: the helper programs Noa can start.

pub(crate) const ALPHA: HelperProgram = HelperProgram {
    title: "Alpha",
    bundled: "Noa Alpha",
    crate_name: "alpha",
};

pub(crate) const BETA: HelperProgram = HelperProgram {
    title: "Beta",
    bundled: "Noa Beta",
    crate_name: "beta",
};

pub(crate) const GAMMA: HelperProgram = HelperProgram {
    title: "Gamma",
    bundled: "Noa Gamma",
    crate_name: "gamma",
};

pub(crate) fn launch_args(helper: HelperProgram, focused_cwd: Option<&str>) -> Vec<String> {
    match focused_cwd {
        Some(cwd)
            if (helper.crate_name == ALPHA.crate_name)
                && Path::new(cwd).is_absolute() =>
        {
            vec![format!("--repo={cwd}")]
        }
        _ => Vec::new(),
    }
}
