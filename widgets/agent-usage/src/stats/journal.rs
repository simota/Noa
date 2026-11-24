//! Noa control journals, `<data dir>/noa/ctl/*.jsonl`: a `spawn` event is one
//! worker of the agent `basename(argv[0])` when that is `claude`, `codex` or
//! `agy`; other commands (scripts, shells) are not agents.

use std::path::Path;

use serde::Deserialize;

use super::index::Sink;
use crate::model::Agent;

#[derive(Deserialize)]
struct Event {
    #[serde(default)]
    ev: Option<String>,
    #[serde(default)]
    ts: Option<i64>,
    #[serde(default)]
    argv: Vec<String>,
}

/// The agent a command line starts, if any.
fn agent_of(argv0: &str) -> Option<Agent> {
    let name = Path::new(argv0).file_name()?.to_str()?;
    Agent::parse(name)
}

/// One journal line; false when it is not valid JSON of the journal shape.
pub(crate) fn line(bytes: &[u8], sink: &mut Sink<'_>) -> bool {
    let Ok(ev) = serde_json::from_slice::<Event>(bytes) else {
        return false;
    };
    if ev.ev.as_deref() != Some("spawn") {
        return true;
    }
    let Some(ts) = ev.ts else {
        return false;
    };
    if let Some(agent) = ev.argv.first().and_then(|a| agent_of(a)) {
        sink.worker(ts, agent);
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::model::Agent;
    use crate::stats::testutil::*;

    #[test]
    fn only_agent_spawns_count_as_workers() {
        let t = Tree::fixtures();
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let mut workers: Vec<_> = ix
            .index()
            .files
            .values()
            .flat_map(|e| e.data.workers.iter().map(|w| w.agent))
            .collect();
        workers.sort();
        // claude, /opt/bin/claude, codex, agy; not ./scripts/verify.sh, zsh,
        // claude-helper, or the send/close events.
        assert_eq!(
            workers,
            vec![Agent::Claude, Agent::Claude, Agent::Codex, Agent::Agy]
        );
    }
}
