fn build_view_menu() {
    let _ = &MenuItem::with_id(AppCommand::OpenAlpha.menu_id(), "Alpha", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenBeta.menu_id(), "Beta", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenDelta.menu_id(), "Delta", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenEpsilon.menu_id(), "Epsilon", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenZeta.menu_id(), "Zeta", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenTheta.menu_id(), "Theta", true, None);
    let _ = &MenuItem::with_id(AppCommand::OpenKappa.menu_id(), "Kappa", true, None);
}
