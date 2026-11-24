//! Limit sample store (unit B owns this file; this is the D0 stub: it holds
//! nothing and writes nothing).

use std::path::PathBuf;

use crate::model::{LimitSample, Unix, UsageSource};

pub struct History {
    samples: Vec<LimitSample>,
}

impl History {
    /// A missing file is an empty history.
    pub fn open(_path: PathBuf) -> History {
        History {
            samples: Vec::new(),
        }
    }

    /// Records the source's fresh windows, throttled (L1-4.6).
    pub fn offer(&mut self, _source: &UsageSource, _now: Unix) -> std::io::Result<()> {
        Ok(())
    }

    pub fn samples(&self, since: Unix) -> &[LimitSample] {
        let start = self.samples.partition_point(|s| s.ts < since);
        &self.samples[start..]
    }
}

pub struct Series {
    pub label: String,
    pub points: Vec<(Unix, f64)>,
}

/// One series per window over `range`; a gap over `max_gap` seconds breaks it.
pub fn series(_samples: &[LimitSample], _range: (Unix, Unix), _max_gap: i64) -> Vec<Series> {
    Vec::new()
}
