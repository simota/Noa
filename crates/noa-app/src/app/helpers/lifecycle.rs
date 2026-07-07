//! Pane io-thread shutdown, running-program counting, close-confirm text.

use super::*;

pub(crate) fn shutdown_pane_io_threads<'a>(surfaces: impl IntoIterator<Item = &'a mut Surface>) {
    for surface in surfaces {
        surface.shutdown();
    }
}

pub(crate) fn surface_has_running_program(surface: &Surface) -> bool {
    surface.terminal.lock().has_running_program()
}

pub(crate) fn running_program_count<'a>(surfaces: impl IntoIterator<Item = &'a Surface>) -> usize {
    surfaces
        .into_iter()
        .filter(|surface| surface_has_running_program(surface))
        .count()
}

pub(crate) fn close_confirm_message(target: CloseConfirmTarget, running_programs: usize) -> String {
    match target {
        CloseConfirmTarget::Pane => {
            "A program is still running in this pane. Close it?".to_string()
        }
        CloseConfirmTarget::Session => {
            close_confirm_plural(running_programs, "this session", "Close this session?")
        }
        CloseConfirmTarget::Window => {
            close_confirm_plural(running_programs, "this window", "Close this window?")
        }
        CloseConfirmTarget::App => close_confirm_plural(running_programs, "Noa", "Quit Noa?"),
    }
}

pub(crate) fn close_confirm_plural(running_programs: usize, scope: &str, question: &str) -> String {
    if running_programs == 0 {
        return question.to_string();
    }
    if running_programs == 1 {
        format!("A program is still running in {scope}. {question}")
    } else {
        format!("{running_programs} programs are still running in {scope}. {question}")
    }
}
