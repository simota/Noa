//! `spawn --ready` (AU-02): after a spawn, wait until the worker looks
//! ready to be given a task, so a `send` does not race its start (a trust
//! prompt or a login drawn late would take the paste and its Enter).

use std::time::{Duration, Instant};

use noa_ipc::Panel;

use super::Failure;

/// How long a non-empty screen must stay the same to count as settled.
pub(crate) const QUIET_FOR: Duration = Duration::from_millis(1500);
/// How often the worker is looked at.
pub(crate) const POLL: Duration = Duration::from_millis(250);
/// `--task` without `--ready`: this long.
pub(crate) const TASK_READY: Duration = Duration::from_secs(30);

/// Why a ready wait ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ready {
    /// The worker's agent sent its first report.
    Report,
    /// The screen showed something and stayed the same for [`QUIET_FOR`].
    Quiet,
    /// Neither within the time given.
    Timeout,
    /// The pane is no longer listed (closed, or its program exited without
    /// being held open).
    Gone,
}

impl Ready {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Quiet => "quiet",
            Self::Timeout => "timeout",
            Self::Gone => "gone",
        }
    }
}

/// What [`wait_ready`] looks at.
pub(crate) trait ReadySource {
    fn panel(&mut self, pane: u64) -> Result<Option<Panel>, Failure>;
    fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure>;
    fn now(&mut self) -> Instant;
    fn pause(&mut self, duration: Duration);
}

/// Wait up to `limit` for `pane` to be ready; returns why the wait ended
/// and the last screen read. A request that timed out counts as nothing
/// seen and is taken again.
pub(crate) fn wait_ready(
    source: &mut impl ReadySource,
    pane: u64,
    limit: Duration,
) -> Result<(Ready, Option<String>), Failure> {
    let started = source.now();
    let mut seen: Option<(String, Instant)> = None;
    let mut screen: Option<String> = None;
    loop {
        let now = source.now();
        let reported = match source.panel(pane) {
            Ok(None) => return Ok((Ready::Gone, screen)),
            Ok(Some(panel)) => panel.agent.as_ref().is_some_and(|agent| agent.seq > 0),
            Err(failure) if failure.transient => false,
            Err(failure) => return Err(failure),
        };
        match source.screen(pane) {
            Ok(Some(text)) => screen = Some(text),
            Ok(None) => return Ok((Ready::Gone, screen)),
            Err(failure) if failure.transient => {}
            Err(failure) => return Err(failure),
        }
        if reported {
            return Ok((Ready::Report, screen));
        }
        if let Some(text) = screen.as_ref().filter(|text| !text.trim().is_empty()) {
            match &seen {
                Some((before, since)) if before == text => {
                    if now.saturating_duration_since(*since) >= QUIET_FOR {
                        return Ok((Ready::Quiet, screen));
                    }
                }
                _ => seen = Some((text.clone(), now)),
            }
        }
        if now.saturating_duration_since(started) >= limit {
            return Ok((Ready::Timeout, screen));
        }
        source.pause(POLL);
    }
}

/// Whether `spawn --task` types its task: the worker is ready (reported,
/// or quiet) and the bottom of its screen shows an agent's input box. A
/// trust prompt or a login is left to the orchestrator, which sees it under
/// `"screen"`.
pub(crate) fn takes_task(ready: Ready, screen: Option<&str>) -> bool {
    matches!(ready, Ready::Report | Ready::Quiet)
        && screen.is_some_and(super::prompt::has_input_box)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker seen at 250 ms steps: `panels[N]` and `screens[N]` at step N
    /// (the last entries repeat).
    struct Script {
        panels: Vec<Option<Panel>>,
        screens: Vec<&'static str>,
        step: usize,
        base: Instant,
    }

    impl ReadySource for Script {
        fn panel(&mut self, _pane: u64) -> Result<Option<Panel>, Failure> {
            Ok(self.panels[self.step.min(self.panels.len() - 1)].clone())
        }

        fn screen(&mut self, _pane: u64) -> Result<Option<String>, Failure> {
            Ok(Some(
                self.screens[self.step.min(self.screens.len() - 1)].to_string(),
            ))
        }

        fn now(&mut self) -> Instant {
            self.base + POLL * self.step as u32
        }

        fn pause(&mut self, _duration: Duration) {
            self.step += 1;
        }
    }

    fn worker(seq: u64) -> Panel {
        serde_json::from_value(serde_json::json!({
            "windowGroupId": "1", "windowId": "1", "paneId": "5", "name": "claude",
            "cwd": "/r", "busy": false, "attention": false, "preview": [],
            "agent": { "kind": "claude", "seq": seq },
        }))
        .unwrap()
    }

    fn run(
        panels: Vec<Option<Panel>>,
        screens: Vec<&'static str>,
        limit: Duration,
    ) -> ((Ready, Option<String>), usize) {
        let mut script = Script {
            panels,
            screens,
            step: 0,
            base: Instant::now(),
        };
        let ready = wait_ready(&mut script, 5, limit).unwrap();
        (ready, script.step)
    }

    #[test]
    fn the_first_report_makes_a_worker_ready() {
        let ((ready, screen), step) = run(
            vec![Some(worker(0)), Some(worker(0)), Some(worker(1))],
            vec!["", "starting", "> "],
            Duration::from_secs(30),
        );
        assert_eq!(ready, Ready::Report);
        assert_eq!(screen.as_deref(), Some("> "));
        assert_eq!(step, 2);
    }

    #[test]
    fn a_screen_that_stays_the_same_for_1_5_s_is_ready() {
        // Blank at first (not quiet), drawn at step 2, the same from then.
        let ((ready, _), step) = run(
            vec![Some(worker(0))],
            vec!["", "", "Trust this folder?", "Trust this folder?"],
            Duration::from_secs(30),
        );
        assert_eq!(ready, Ready::Quiet);
        assert_eq!(step, 2 + 6);
    }

    #[test]
    fn a_worker_that_never_settles_times_out_and_a_closed_one_is_gone() {
        let screens: Vec<&'static str> = (0..100)
            .map(|n| &*Box::leak(format!("tick {n}").into_boxed_str()))
            .collect();
        let ((ready, _), step) = run(vec![Some(worker(0))], screens, Duration::from_secs(2));
        assert_eq!(ready, Ready::Timeout);
        assert_eq!(step, 8);
        let ((ready, _), _) = run(
            vec![Some(worker(0)), None],
            vec![""],
            Duration::from_secs(9),
        );
        assert_eq!(ready, Ready::Gone);
    }

    #[test]
    fn a_task_is_typed_only_into_a_ready_input_box() {
        let rule = "─".repeat(40);
        let input = format!("Welcome\n{rule}\n> \n{rule}\n  ? for shortcuts\n");
        assert!(takes_task(Ready::Quiet, Some(&input)));
        assert!(takes_task(Ready::Report, Some(&input)));
        assert!(!takes_task(Ready::Timeout, Some(&input)));
        let trust = "Do you trust the files in this folder?\n❯ 1. Yes, proceed\n  2. No, exit\n";
        assert!(!takes_task(Ready::Quiet, Some(trust)));
        assert!(!takes_task(Ready::Report, None));
    }
}
