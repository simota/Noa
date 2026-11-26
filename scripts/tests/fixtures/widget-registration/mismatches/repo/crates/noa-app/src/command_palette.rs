pub(crate) fn command_palette_title(command: AppCommand) -> &'static str {
    match command {
        AppCommand::OpenAlpha => "Open Alpha",
        AppCommand::OpenBeta => "Open Beta",
        AppCommand::OpenDelta => "Open Delta",
        AppCommand::OpenEpsilon => "Open Epsilon",
        AppCommand::OpenZeta => "Open Zeta",
        AppCommand::OpenKappa => "Open Kappa",
    }
}

pub(crate) fn command_palette_entries() -> &'static [AppCommand] {
    &[
        AppCommand::OpenAlpha,
        AppCommand::OpenBeta,
        AppCommand::OpenDelta,
        AppCommand::OpenEpsilon,
        AppCommand::OpenZeta,
        AppCommand::OpenTheta,
        AppCommand::OpenKappa,
    ]
}
