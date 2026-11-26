impl App {
    fn open_helper_program(&mut self, helper: HelperProgram) {
        let _ = helper;
    }

    fn run_app_command(&mut self, command: AppCommand) {
        match command {
            AppCommand::OpenAlpha => self.open_helper_program(crate::helper_program::ALPHA),
            AppCommand::OpenBeta => self.open_helper_program(crate::helper_program::BETA),
            AppCommand::OpenGamma => self.open_helper_program(crate::helper_program::GAMMA),
        }
    }
}
