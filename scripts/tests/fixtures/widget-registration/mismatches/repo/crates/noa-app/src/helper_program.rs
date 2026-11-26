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

pub(crate) const DELTA: HelperProgram = HelperProgram {
    title: "Delta",
    bundled: "Noa Delta",
    crate_name: "delta",
};

pub(crate) const EPSILON: HelperProgram = HelperProgram {
    title: "Epsilon",
    bundled: "Noa Epsilon",
    crate_name: "epsilon",
};

pub(crate) const ZETA: HelperProgram = HelperProgram {
    title: "Zeta",
    bundled: "Noa Zeta",
    crate_name: "zeta",
};

pub(crate) const THETA: HelperProgram = HelperProgram {
    title: "Theta",
    bundled: "Noa Theta",
    crate_name: "theta",
};

pub(crate) const KAPPA: HelperProgram = HelperProgram {
    title: "Kappa",
    bundled: "Noa Kappa",
    crate_name: "kappa",
};

pub(crate) const XI: HelperProgram = HelperProgram {
    title: "Xi",
    bundled: "Noa Xi",
    crate_name: "xi",
};

pub(crate) const NU: HelperProgram = HelperProgram {
    title: "Nu",
    bundled: "Noa Nu",
    crate_name: "nu",
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
