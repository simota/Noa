//! Horizontal tab stops (default every 8 columns).

#[derive(Clone, Debug)]
pub struct Tabstops {
    stops: Vec<bool>,
}

impl Tabstops {
    pub fn new(cols: u16) -> Self {
        let mut t = Tabstops {
            stops: vec![false; cols as usize],
        };
        t.reset();
        t
    }

    /// Reset to the default stops at columns 8, 16, 24, …
    pub fn reset(&mut self) {
        for (i, s) in self.stops.iter_mut().enumerate() {
            *s = i > 0 && i % 8 == 0;
        }
    }

    pub fn set(&mut self, col: u16) {
        if let Some(s) = self.stops.get_mut(col as usize) {
            *s = true;
        }
    }

    /// The next tab stop strictly greater than `from`, clamped to the last column.
    pub fn next(&self, from: u16, cols: u16) -> u16 {
        let mut i = from as usize + 1;
        while i < self.stops.len() && (i as u16) < cols {
            if self.stops[i] {
                return i as u16;
            }
            i += 1;
        }
        cols.saturating_sub(1)
    }
}
