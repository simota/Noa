fn build_view_menu() {
    let _ = &MenuItem::with_id(AppCommand::OpenAlpha.menu_id(), "Alpha", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenBeta.menu_id(), "Beta", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenGamma.menu_id(), "Gamma", true, None);
}
