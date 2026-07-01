//! `FrameSnapshot` — a lock-free-to-render copy of the bits of `Terminal`
//! needed to rebuild a frame's GPU instances. `noa-app` takes this under the
//! `Terminal` mutex and then calls into the renderer unlocked.

use noa_grid::{Cursor, Row, Terminal};

/// A snapshot of the active screen taken under the `Terminal` lock.
///
/// Inc-1 clones every row rather than diffing dirty rows — correctness over
/// micro-optimization; dirty-row diffing is a natural inc≥2 follow-up.
pub struct FrameSnapshot {
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub cols: u16,
    pub rows_n: u16,
}

impl FrameSnapshot {
    /// Clone the active screen's rows + cursor out of `terminal`.
    pub fn from_terminal(terminal: &Terminal) -> Self {
        let screen = terminal.active();
        FrameSnapshot {
            rows: screen.grid.clone(),
            cursor: screen.cursor,
            cols: screen.cols,
            rows_n: screen.rows,
        }
    }
}
