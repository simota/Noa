//! Noa control journals, `<data dir>/noa/ctl/*.jsonl`: a `spawn` event is one
//! worker of the agent `basename(argv[0])` when that is `claude`, `codex` or
//! `agy`; other commands (scripts, shells) are not agents.

use std::path::Path;

use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};

use super::index::Sink;
use crate::model::Agent;

#[derive(Debug, Deserialize)]
struct Event {
    #[serde(default)]
    ev: Option<String>,
    #[serde(default)]
    ts: Option<i64>,
    /// `argv[0]` only: the arguments after it (a prompt, say) are skipped
    /// without being read into any structure.
    #[serde(rename = "argv", default, deserialize_with = "first_arg")]
    argv0: Option<String>,
}

fn first_arg<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Option<String>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a command line")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Option<String>, A::Error> {
            let first = seq.next_element::<String>()?;
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(first)
        }
    }
    d.deserialize_seq(V)
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
    if let Some(agent) = ev.argv0.as_deref().and_then(agent_of) {
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

    #[test]
    fn only_argv0_is_decoded() {
        let line = r#"{"ev":"spawn","ts":1795395840,"argv":["agy","-p","SYNTHETIC-PROMPT",{"x":1}],"tier":"standard","pane":4}"#;
        let ev: super::Event = serde_json::from_str(line).unwrap();
        assert_eq!(ev.argv0.as_deref(), Some("agy"));
        assert!(!format!("{ev:?}").contains("SYNTHETIC-PROMPT"));
        // Indexed: the worker counts, and the prompt is nowhere in the index.
        let t = Tree::new();
        let journal = t.root.join("data/noa/ctl/2026-11-23.jsonl");
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        write(&journal, &jsonl(&[line.to_owned()]));
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let workers: Vec<_> = ix.index().files[&journal]
            .data
            .workers
            .iter()
            .map(|w| w.agent)
            .collect();
        assert_eq!(workers, vec![Agent::Agy]);
        let saved = std::fs::read_to_string(crate::paths::index_path(&t.own)).unwrap();
        assert!(!saved.contains("SYNTHETIC-PROMPT"));
        assert!(!format!("{:?}", ix.index()).contains("SYNTHETIC-PROMPT"));
    }
}
