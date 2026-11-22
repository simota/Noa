//! The discussion widget's binary. The model, engine and store live in the
//! library (`lib.rs`); the UI and the live link to Noa come next.

/// The live session runner (`src/live/`): `ipc` is the Noa port and its
/// `noa-ipc` implementation, `runner` drives one discussion against it. The
/// UI starts a session with `session::runner::spawn_session`.
#[path = "live"]
#[expect(
    dead_code,
    reason = "the UI calls the runner; until it lands only the tests do"
)]
mod session {
    pub mod ipc;
    pub mod runner;
}

fn main() {
    eprintln!("discussion: the UI is not built yet");
    std::process::exit(2);
}
