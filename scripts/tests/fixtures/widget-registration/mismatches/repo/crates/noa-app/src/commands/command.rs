impl AppCommand {
    const OPEN_ALPHA_MENU_ID: &'static str = "noa.view.open-alpha";
    const OPEN_BETA_MENU_ID: &'static str = "noa.view.open-beta";
    const OPEN_DELTA_MENU_ID: &'static str = "noa.view.open-delta";
    const OPEN_EPSILON_MENU_ID: &'static str = "noa.view.open-epsilon";
    const OPEN_ZETA_MENU_ID: &'static str = "noa.view.open-zeta";
    const OPEN_THETA_MENU_ID: &'static str = "noa.view.open-theta";
    const OPEN_KAPPA_MENU_ID: &'static str = "noa.view.open-kappa";
    const OPEN_NU_MENU_ID: &'static str = "noa.view.open-nu";

    pub(crate) fn menu_id(self) -> &'static str {
        match self {
            AppCommand::OpenAlpha => Self::OPEN_ALPHA_MENU_ID,
            AppCommand::OpenBeta => Self::OPEN_BETA_MENU_ID,
            AppCommand::OpenDelta => Self::OPEN_DELTA_MENU_ID,
            AppCommand::OpenEpsilon => Self::OPEN_EPSILON_MENU_ID,
            AppCommand::OpenZeta => Self::OPEN_ZETA_MENU_ID,
            AppCommand::OpenTheta => Self::OPEN_THETA_MENU_ID,
            AppCommand::OpenKappa => Self::OPEN_KAPPA_MENU_ID,
            AppCommand::OpenNu => Self::OPEN_NU_MENU_ID,
        }
    }

    pub(crate) fn from_menu_id(id: &str) -> Option<Self> {
        match id {
            Self::OPEN_ALPHA_MENU_ID => Some(Self::OpenAlpha),
            Self::OPEN_BETA_MENU_ID => Some(Self::OpenBeta),
            Self::OPEN_DELTA_MENU_ID => Some(Self::OpenDelta),
            Self::OPEN_EPSILON_MENU_ID => Some(Self::OpenEpsilon),
            Self::OPEN_ZETA_MENU_ID => Some(Self::OpenZeta),
            Self::OPEN_KAPPA_MENU_ID => Some(Self::OpenKappa),
            Self::OPEN_NU_MENU_ID => Some(Self::OpenNu),
            _ => None,
        }
    }

    fn fixed_action_name(self) -> &'static str {
        match self {
            Self::OpenAlpha => "alpha.open",
            Self::OpenBeta => "beta.open",
            Self::OpenDelta => "delta.open",
            Self::OpenEpsilon => "epsilon.open",
            Self::OpenZeta => "zeta.open",
            Self::OpenTheta => "theta.open",
            Self::OpenKappa => "kappa.open",
            Self::OpenNu => "nu.open",
        }
    }

    pub fn from_action_name(name: &str) -> Option<Self> {
        match name {
            "alpha.open" => Some(Self::OpenAlpha),
            "beta.open" => Some(Self::OpenBeta),
            "delta.open" => Some(Self::OpenDelta),
            "epsilon.open" => Some(Self::OpenEpsilon),
            "zeta.open" => Some(Self::OpenZeta),
            "theta.open" => Some(Self::OpenTheta),
            "kappa.open" => Some(Self::OpenKappa),
            "nu.open" => Some(Self::OpenNu),
            _ => None,
        }
    }
}
