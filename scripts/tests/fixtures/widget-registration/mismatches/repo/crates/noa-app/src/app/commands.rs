impl App {
    fn open_helper_program(&mut self, helper: HelperProgram) {
        let _ = helper;
    }

    fn run_app_command(&mut self, command: AppCommand) {
        match command {
            AppCommand::OpenAlpha => self.open_helper_program(crate::helper_program::ALPHA),
            AppCommand::OpenBeta => self.open_helper_program(crate::helper_program::BETA),
            AppCommand::OpenDelta => self.open_helper_program(crate::helper_program::DELTA),
            AppCommand::OpenEpsilon => self.open_helper_program(crate::helper_program::EPSILON),
            AppCommand::OpenZeta => self.open_helper_program(crate::helper_program::ZETA),
            AppCommand::OpenTheta => self.open_helper_program(crate::helper_program::THETA),
            AppCommand::OpenKappa => self.open_helper_program(crate::helper_program::KAPPA),
            AppCommand::OpenNu => self.open_helper_program(crate::helper_program::NU),
        }
    }
}
