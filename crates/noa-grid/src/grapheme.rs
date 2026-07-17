//! Process-global interner for combining-scalar tails.
//!
//! A [`crate::Cell`] is a fixed-size POD; the rare cells that carry combining
//! scalars (accents, ZWJ emoji tails, kitty-placeholder diacritics) store an
//! opaque [`GraphemeId`] here instead of an inline heap `String`. Interning is
//! content-addressed and append-only: the same tail always yields the same id,
//! entries are leaked `&'static str`s, and ids stay valid for the process
//! lifetime — so cells are freely `memcpy`-movable between rows, screens,
//! scrollback and `FrameSnapshot`s, and the renderer can resolve text after
//! the `Terminal` lock is released.
//!
//! Growth is bounded by deduplication in practice (real streams repeat a
//! small set of tails) and by [`MAX_ENTRIES`] against a hostile stream
//! minting unbounded distinct tails: past the cap, [`intern`] returns `None`
//! and the caller keeps the cell's previous tail (the new mark is dropped —
//! degraded rendering, never unbounded memory).

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{LazyLock, RwLock};

/// Opaque handle to one interned combining tail. Only
/// [`crate::Cell::push_combining`] / [`crate::Cell::set_combining`] mint
/// values, so every id in a live cell is resolvable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GraphemeId(NonZeroU32);

/// Hard cap on distinct interned tails. Each entry costs its string bytes
/// (≤ [`crate::Cell::MAX_COMBINING_BYTES`]) plus map/vec overhead, so the
/// worst case is a few hundred MB *if* a hostile stream mints the maximum —
/// legitimate content (even kitty placeholder grids, which encode row/column
/// pairs as diacritics) stays orders of magnitude below this.
const MAX_ENTRIES: usize = 1 << 20;

struct Interner {
    lookup: HashMap<&'static str, GraphemeId>,
    entries: Vec<&'static str>,
}

static INTERNER: LazyLock<RwLock<Interner>> = LazyLock::new(|| {
    RwLock::new(Interner {
        lookup: HashMap::new(),
        entries: Vec::new(),
    })
});

/// Intern `tail` (non-empty), returning its stable id, or `None` when the
/// [`MAX_ENTRIES`] cap is reached and `tail` is not already present.
pub(crate) fn intern(tail: &str) -> Option<GraphemeId> {
    debug_assert!(!tail.is_empty());
    {
        let interner = INTERNER.read().unwrap();
        if let Some(&id) = interner.lookup.get(tail) {
            return Some(id);
        }
        if interner.entries.len() >= MAX_ENTRIES {
            return None;
        }
    }
    let mut interner = INTERNER.write().unwrap();
    // Re-check under the write lock: another thread may have interned `tail`
    // (or filled the table) between the read unlock and here.
    if let Some(&id) = interner.lookup.get(tail) {
        return Some(id);
    }
    if interner.entries.len() >= MAX_ENTRIES {
        return None;
    }
    let leaked: &'static str = Box::leak(tail.to_owned().into_boxed_str());
    let id = GraphemeId(
        NonZeroU32::new(interner.entries.len() as u32 + 1).expect("len + 1 is non-zero"),
    );
    interner.entries.push(leaked);
    interner.lookup.insert(leaked, id);
    Some(id)
}

/// The tail behind `id`. Entries are never freed, so the `&'static` borrow
/// outlives every lock.
pub(crate) fn resolve(id: GraphemeId) -> &'static str {
    INTERNER.read().unwrap().entries[id.0.get() as usize - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_content_addressed_and_stable() {
        let a = intern("\u{301}").expect("under cap");
        let b = intern("\u{301}").expect("under cap");
        assert_eq!(a, b);
        assert_eq!(resolve(a), "\u{301}");

        let c = intern("\u{301}\u{302}").expect("under cap");
        assert_ne!(a, c);
        assert_eq!(resolve(c), "\u{301}\u{302}");
    }
}
