//! `noa.timeline.list` filter and query (`docs/specs/board-timeline.md`
//! D12): a scan, no index. Titles and summaries are matched in memory; a
//! body is read back from `posts.jsonl` by offset only when they miss.
//!
//! The query is a case-insensitive substring of the title, summary or
//! body. For an ASCII query with nothing JSON escapes, a post's raw line is
//! searched first, ASCII-case-insensitively: the body is in that line
//! verbatim, so a miss there is a miss, and only a hit pays for decoding
//! the line. That holds only for needles without the ASCII letters a
//! non-ASCII character lowercases to (`K` KELVIN SIGN → `k`, `İ` → `i̇`):
//! a body's `Kelvin` matches `kelvin` without an ASCII `k` in its bytes.

use noa_ipc::timeline::{Filter, PostSummary, TimelineError, TimelineErrorKind};

use super::replay::{BodyLine, Rec};

/// Whether `post` passes `filter` (an empty list matches anything).
pub(crate) fn filter_matches(filter: &Filter, post: &PostSummary) -> bool {
    (filter.worker_ids.is_empty()
        || post
            .worker_id
            .as_ref()
            .is_some_and(|id| filter.worker_ids.contains(id)))
        && (filter.agent_kinds.is_empty()
            || post
                .author
                .agent_kind
                .as_ref()
                .is_some_and(|kind| filter.agent_kinds.contains(kind)))
        && (filter.kinds.is_empty() || filter.kinds.contains(&post.kind))
        && (filter.outcomes.is_empty() || filter.outcomes.contains(&post.outcome))
}

pub(crate) struct Query {
    /// Lowercased.
    needle: String,
    /// The needle's bytes when a raw-line search is exact (ASCII, nothing
    /// JSON would escape, no byte of [`FROM_NON_ASCII`]).
    raw: Option<Vec<u8>>,
}

/// The ASCII characters that some non-ASCII character's lowercase
/// contains (U+212A KELVIN SIGN → `k`, U+0130 → `i` + U+0307), pinned by
/// `from_non_ascii_is_every_ascii_a_lowercase_reaches`.
const FROM_NON_ASCII: &[u8] = b"ik";

impl Query {
    /// `None` for an empty or blank query (no query).
    pub(crate) fn new(query: &str) -> Option<Self> {
        if query.chars().all(char::is_whitespace) {
            return None;
        }
        let needle = query.to_lowercase();
        let raw = needle
            .bytes()
            .all(|b| {
                b.is_ascii()
                    && !b.is_ascii_control()
                    && b != b'"'
                    && b != b'\\'
                    && !FROM_NON_ASCII.contains(&b)
            })
            .then(|| needle.as_bytes().to_vec());
        Some(Self { needle, raw })
    }

    fn in_text(&self, text: &str) -> bool {
        text.to_lowercase().contains(&self.needle)
    }

    /// Whether `rec` matches; `line` reads its `posts.jsonl` line, called
    /// only when the title and summary miss.
    pub(crate) fn matches(
        &self,
        rec: &Rec,
        line: impl FnOnce() -> Result<Vec<u8>, TimelineError>,
    ) -> Result<bool, TimelineError> {
        if self.in_text(&rec.summary.title) || self.in_text(&rec.summary.summary) {
            return Ok(true);
        }
        let line = line()?;
        if let Some(raw) = &self.raw
            && !contains_ascii_ci(&line, raw)
        {
            return Ok(false);
        }
        let body: BodyLine = serde_json::from_slice(&line).map_err(|err| {
            let mut e = TimelineError::new(
                TimelineErrorKind::CorruptStore,
                format!("post at seq {} does not decode: {err}", rec.summary.seq.0),
            );
            e.line = Some(rec.summary.seq.0);
            e
        })?;
        Ok(self.in_text(&body.post.body_markdown))
    }
}

/// `needle` (lowercase ASCII) occurs in `hay`, ignoring ASCII case.
fn contains_ascii_ci(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if hay.len() < needle.len() {
        return false;
    }
    (0..=hay.len() - needle.len()).any(|i| {
        hay[i].to_ascii_lowercase() == first
            && hay[i + 1..i + needle.len()]
                .iter()
                .zip(rest)
                .all(|(h, n)| h.to_ascii_lowercase() == *n)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_search_ignores_ascii_case() {
        assert!(contains_ascii_ci(b"xx Hello World", b"o w"));
        assert!(contains_ascii_ci(b"ABC", b"abc"));
        assert!(!contains_ascii_ci(b"ab", b"abc"));
        assert!(!contains_ascii_ci(b"a-b-c", b"abc"));
    }

    #[test]
    fn blank_query_is_none_and_escapable_needles_skip_the_raw_search() {
        assert!(Query::new("  ").is_none());
        assert!(Query::new("Parser").unwrap().raw.is_some());
        assert!(Query::new("say \"hi\"").unwrap().raw.is_none());
        assert!(Query::new("日本").unwrap().raw.is_none());
        assert!(Query::new("kelvin").unwrap().raw.is_none());
        assert!(Query::new("Istanbul").unwrap().raw.is_none());
    }

    #[test]
    fn from_non_ascii_is_every_ascii_a_lowercase_reaches() {
        let mut reached: Vec<u8> = (0x80..=0x10_FFFF)
            .filter_map(char::from_u32)
            .flat_map(char::to_lowercase)
            .filter(char::is_ascii)
            .map(|c| c as u8)
            .collect();
        reached.sort_unstable();
        reached.dedup();
        assert_eq!(reached, FROM_NON_ASCII);
    }
}
