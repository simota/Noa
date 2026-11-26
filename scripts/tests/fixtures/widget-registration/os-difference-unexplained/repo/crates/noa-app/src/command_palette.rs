pub(crate) fn command_palette_title(command: AppCommand) -> &'static str {
    match command {
        AppCommand::OpenAlpha => "Open Alpha",
        AppCommand::OpenBeta => "Open Beta",
    }
}

pub(crate) fn command_palette_entries() -> &'static [AppCommand] {
    &[
        AppCommand::OpenAlpha,
        AppCommand::OpenBeta,
    ]
}
