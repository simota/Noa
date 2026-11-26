impl AppCommand {
    const OPEN_ALPHA_MENU_ID: &'static str = "noa.view.open-alpha";
    const OPEN_BETA_MENU_ID: &'static str = "noa.view.open-beta";
    const OPEN_GAMMA_MENU_ID: &'static str = "noa.view.open-gamma";

    pub(crate) fn menu_id(self) -> &'static str {
        match self {
            AppCommand::OpenAlpha => Self::OPEN_ALPHA_MENU_ID,
            AppCommand::OpenBeta => Self::OPEN_BETA_MENU_ID,
            AppCommand::OpenGamma => Self::OPEN_GAMMA_MENU_ID,
        }
    }

    pub(crate) fn from_menu_id(id: &str) -> Option<Self> {
        match id {
            Self::OPEN_ALPHA_MENU_ID => Some(Self::OpenAlpha),
            Self::OPEN_BETA_MENU_ID => Some(Self::OpenBeta),
            Self::OPEN_GAMMA_MENU_ID => Some(Self::OpenGamma),
            _ => None,
        }
    }

    fn fixed_action_name(self) -> &'static str {
        match self {
            Self::OpenAlpha => "alpha.open",
            Self::OpenBeta => "beta.open",
            Self::OpenGamma => "gamma.open",
        }
    }

    pub fn from_action_name(name: &str) -> Option<Self> {
        match name {
            "alpha.open" => Some(Self::OpenAlpha),
            "beta.open" => Some(Self::OpenBeta),
            "gamma.open" => Some(Self::OpenGamma),
            _ => None,
        }
    }
}
