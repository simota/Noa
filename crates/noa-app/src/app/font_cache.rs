//! Every live `FontGrid`, keyed by the pixel size it rasterizes at.
//!
//! `FontGrid` carries `px_size` as an object-scoped field with no size in any
//! cache key, so one grid serves exactly one pixel size. Two things follow,
//! and this map is both of them:
//!
//! **Returning to a size costs nothing.** A font-size or DPI change swaps
//! grids instead of discarding them. Measured
//! (`noa-font/examples/bench_size_change.rs`), building and warming a grid is
//! ~13.5 ms of main-thread CPU, 90% of it `raster_shaped` — swash's
//! scale+hint+render, which noa does not own and cannot make cheaper (the
//! `thicken` dilation it does own is 5.7%). Replaying the same warm set
//! against an already-populated grid is 0.05 ms, ~200x less. So 14 -> 15 -> 14,
//! or dragging a window between a 1x and a 2x display, re-rasterizes nothing.
//!
//! **Windows at different scale factors can each be crisp.** The terminal
//! grid used to be app-wide, rebuilt at whichever window last reported a
//! scale change, which left every other window rasterizing at a size that was
//! not its own. Windows now look their grid up by their own pixel size, and
//! windows that share a size share a grid — so the common case (every window
//! on one display) still allocates exactly one.
//!
//! The renderer side of this is `noa_render::GlyphAtlasCache`, keyed by the
//! same quantized pixel size so two sizes never share one texture set, and
//! `Renderer::rebind_glyph_atlases`, which follows a window across a scale
//! change. Both quantize identically to [`PpemKey`]; they must agree on what
//! "the same size" means or a window would draw from another size's atlas.

use noa_font::{FontConfig, FontGrid};

/// Live grids per role. Each holds a mask + color atlas (256 KiB each at
/// 14 ppem), so the cap bounds the footprint at roughly `CAP * 512 KiB`.
/// Entries are accessed on every use, so eviction naturally targets sizes no
/// window is on rather than one a window still points at.
const CAP: usize = 6;

/// Quantized pixel-size key, 1/64 px. Fine enough that no two visually
/// distinct sizes collide, coarse enough that float noise in the same logical
/// size does not miss. **Must match `noa_render`'s atlas cache key**, or a
/// window could take a grid from one bucket and an atlas from another.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct PpemKey(u32);

impl PpemKey {
    fn new(px_size: f32) -> Self {
        PpemKey((px_size * 64.0).round().max(0.0) as u32)
    }
}

struct Entry {
    key: PpemKey,
    grid: FontGrid,
    /// Access stamp; the lowest is evicted first.
    used: u64,
}

/// The live grids for one logical font role (terminal, or sidebar).
///
/// Never empty once constructed: [`get_mut`](Self::get_mut) is called from the
/// draw path, where returning `None` would mean "do not draw", so a build
/// failure falls back to a resident grid rather than dropping the frame.
pub(super) struct FontGridMap {
    entries: Vec<Entry>,
    /// The config every resident grid was built with. `FontGrid` bakes the
    /// config in at construction, so a config change invalidates all of them.
    config: FontConfig,
    /// Which entry app-wide consumers resolve to. See `primary_mut`.
    primary: PpemKey,
    clock: u64,
}

impl FontGridMap {
    /// Build the map around its first grid. That grid is the fallback for
    /// every later lookup that cannot be satisfied.
    #[cfg(test)]
    fn new(px_size: f32, config: FontConfig) -> Result<Self, noa_font::FontError> {
        let grid = FontGrid::new(px_size, config.clone())?;
        Ok(Self {
            entries: vec![Entry {
                key: PpemKey::new(px_size),
                grid,
                used: 0,
            }],
            config,
            primary: PpemKey::new(px_size),
            clock: 1,
        })
    }

    /// Adopt an already-built grid as the map's first entry.
    pub(super) fn from_grid(grid: FontGrid, config: FontConfig) -> Self {
        let px = grid.px_size();
        Self {
            entries: vec![Entry {
                key: PpemKey::new(px),
                grid,
                used: 0,
            }],
            config,
            primary: PpemKey::new(px),
            clock: 1,
        }
    }

    /// Build the replacement grid a config change needs, without touching
    /// this map. `Ok(None)` means the config is unchanged and there is nothing
    /// to install.
    ///
    /// Split from [`install_config`](Self::install_config) so a caller
    /// changing several maps at once can build them all before committing any:
    /// installing into one and then failing on the next left the app's
    /// configuration and its actual fonts disagreeing.
    pub(super) fn prepare_config(
        &self,
        config: &FontConfig,
        px_size: f32,
    ) -> Result<Option<FontGrid>, noa_font::FontError> {
        if &self.config == config {
            return Ok(None);
        }
        FontGrid::new(px_size, config.clone()).map(Some)
    }

    /// Commit what [`prepare_config`](Self::prepare_config) built. A `None`
    /// grid is the unchanged-config case and is a no-op.
    pub(super) fn install_config(&mut self, config: FontConfig, grid: Option<FontGrid>) {
        let Some(grid) = grid else {
            return;
        };
        let key = PpemKey::new(grid.px_size());
        self.entries.clear();
        self.config = config;
        self.primary = key;
        self.clock += 1;
        self.entries.push(Entry {
            key,
            grid,
            used: self.clock,
        });
    }

    /// Make sure a grid exists for `px_size`, building it if this is the first
    /// time that size is seen. Call this off the draw path — at window
    /// creation and on a scale change — so [`get_mut`](Self::get_mut) never
    /// has to build mid-frame.
    pub(super) fn ensure(&mut self, px_size: f32) -> bool {
        let key = PpemKey::new(px_size);
        if self.entries.iter().any(|entry| entry.key == key) {
            return true;
        }
        match FontGrid::new(px_size, self.config.clone()) {
            Ok(grid) => {
                self.clock += 1;
                let used = self.clock;
                self.entries.push(Entry { key, grid, used });
                self.evict_to_cap();
                true
            }
            Err(err) => {
                log::warn!("font map: failed to build a grid at {px_size} px: {err}");
                false
            }
        }
    }

    /// The grid for `px_size`, resolving a miss exactly as [`get`](Self::get)
    /// does. Infallible: the map always holds at least one grid.
    ///
    /// Deliberately does **not** build on a miss. It used to, as a safety net,
    /// and that was the bug: `get` cannot build (it takes `&self`), so a
    /// caller that picked its atlas set through `get` and then rasterized
    /// through `get_mut` got the primary's atlas and a freshly built grid of
    /// the requested size — two different pixel sizes, which is the cross-size
    /// sync `SharedGlyphAtlases::sync` asserts against. Residency is
    /// [`ensure`](Self::ensure)'s job, off the draw path.
    pub(super) fn get_mut(&mut self, px_size: f32) -> &mut FontGrid {
        let key = PpemKey::new(px_size);
        self.clock += 1;
        let stamp = self.clock;
        let idx = self.index_for(key);
        let entry = &mut self.entries[idx];
        entry.used = stamp;
        &mut entry.grid
    }

    /// Make sure `px_size` is resident and make it the primary.
    ///
    /// Paired for the same reason as [`adopt_as_primary`](Self::adopt_as_primary):
    /// `set_primary` alone silently does nothing when the size is not resident
    /// yet, so the two must not be separable.
    pub(super) fn ensure_primary(&mut self, px_size: f32) -> bool {
        if !self.ensure(px_size) {
            return false;
        }
        self.primary = PpemKey::new(px_size);
        true
    }

    /// The grid for the one consumer with no window in scope: the metrics
    /// probe taken before the first window exists. Everything drawn into a
    /// window passes that window's pixel size instead.
    pub(super) fn primary(&self) -> &FontGrid {
        &self.entries[self.primary_index()].grid
    }

    /// The entry a lookup for `key` resolves to, miss included. The read and
    /// write paths share this on purpose: a caller picks its atlas set through
    /// `get` and rasterizes through `get_mut`, so if the two resolved a miss
    /// differently it would pair a grid with another size's atlas. They used
    /// to — `get_mut` fell back to index 0 while `get` fell back to the
    /// primary.
    fn index_for(&self, key: PpemKey) -> usize {
        self.entries
            .iter()
            .position(|entry| entry.key == key)
            .unwrap_or_else(|| self.primary_index())
    }

    fn primary_index(&self) -> usize {
        self.entries
            .iter()
            .position(|entry| entry.key == self.primary)
            .unwrap_or(0)
    }

    /// Read-only lookup for `px_size`, falling back to the primary.
    pub(super) fn get(&self, px_size: f32) -> &FontGrid {
        &self.entries[self.index_for(PpemKey::new(px_size))].grid
    }

    fn evict_to_cap(&mut self) {
        while self.entries.len() > CAP {
            let primary = self.primary;
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.key != primary)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            self.entries.remove(victim);
        }
    }

    #[cfg(test)]
    fn resident_sizes(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self.entries.iter().map(|entry| entry.key.0).collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> FontGridMap {
        FontGridMap::new(14.0, FontConfig::default()).expect("build map")
    }

    /// The behaviour this exists for: a size already seen comes back without a
    /// rebuild. `atlas_identity` is unique per constructed grid, so a rebuild
    /// would carry a different one.
    #[test]
    fn returning_to_a_seen_size_reuses_the_same_grid() {
        let mut map = map();
        let first = map.get_mut(14.0).atlas_identity();
        assert!(map.ensure(28.0));
        let _ = map.get_mut(28.0);
        assert_eq!(
            map.get_mut(14.0).atlas_identity(),
            first,
            "a resident grid must come back, not be rebuilt"
        );
    }

    /// Windows on one display must not each allocate their own grid.
    #[test]
    fn one_size_is_one_grid_however_many_windows_ask() {
        let mut map = map();
        let a = map.get_mut(14.0).atlas_identity();
        let b = map.get_mut(14.0).atlas_identity();
        assert_eq!(a, b);
        assert_eq!(map.resident_sizes().len(), 1);
    }

    #[test]
    fn distinct_sizes_get_distinct_grids() {
        let mut map = map();
        let small = map.get_mut(14.0).atlas_identity();
        assert!(map.ensure(28.0));
        let large = map.get_mut(28.0).atlas_identity();
        assert_ne!(small, large);
        assert_eq!(map.resident_sizes().len(), 2);
    }

    /// The read and write paths must resolve a non-resident size to the SAME
    /// grid. `get_mut` used to build one instead, which `get` cannot do — so a
    /// caller picked its atlas set from the primary through `get` and then
    /// rasterized a freshly built grid of the requested size through
    /// `get_mut`, syncing one pixel size into another size's textures.
    /// Residency is `ensure`'s job, off the draw path.
    #[test]
    fn get_mut_does_not_build_a_missing_size() {
        let mut map = map();
        let resident = map.resident_sizes();
        let primary = map.primary().atlas_identity();

        assert_eq!(
            map.get_mut(28.0).atlas_identity(),
            primary,
            "a write miss must resolve where a read miss resolves"
        );
        assert_eq!(
            map.resident_sizes(),
            resident,
            "and must not have built anything"
        );
    }

    /// A grid bakes its config in, so none may survive a config change.
    #[test]
    fn a_config_change_invalidates_every_grid() {
        let mut map = map();
        let before = map.get_mut(14.0).atlas_identity();
        let changed = FontConfig {
            thicken: false,
            ..FontConfig::default()
        };
        let prepared = map.prepare_config(&changed, 14.0).expect("build");
        assert!(prepared.is_some(), "a changed config must build a new grid");
        map.install_config(changed, prepared);
        assert_ne!(
            map.get_mut(14.0).atlas_identity(),
            before,
            "a grid built with the old config must not be reused"
        );
        assert_eq!(map.resident_sizes().len(), 1);
    }

    /// Bounded, and eviction must target a size nothing is using rather than
    /// one that was just drawn with.
    #[test]
    fn eviction_is_bounded_and_spares_the_recently_used() {
        let mut map = map();
        for px in [10.0f32, 11.0, 12.0, 13.0, 15.0, 16.0, 17.0] {
            assert!(map.ensure(px));
            // Keep 14 px hot, as a live window would.
            let _ = map.get_mut(14.0);
        }
        let resident = map.resident_sizes();
        assert_eq!(resident.len(), CAP);
        assert!(
            resident.contains(&PpemKey::new(14.0).0),
            "the size in continuous use must survive eviction: {resident:?}"
        );
    }

    /// Same pairing from the other direction: promoting a size that is not
    /// resident yet has to build it, not silently leave the primary behind.
    #[test]
    fn ensure_primary_promotes_a_size_that_was_not_resident() {
        let mut map = map();
        let before = map.primary().px_size();
        assert!(map.ensure_primary(28.0));
        assert_ne!(before, 28.0, "the test needs a size that was not primary");
        assert_eq!(map.primary().px_size(), 28.0);
    }

    /// `get` and `get_mut` must resolve a miss to the SAME entry. A caller
    /// picks its atlas set through the read path and rasterizes through the
    /// write path, so falling back to different entries would pair a grid with
    /// another size's atlas — the corruption
    /// `SharedGlyphAtlases::sync` asserts against. `get_mut` used to fall back
    /// to index 0 while `get` fell back to the primary; this pins that the
    /// primary is not index 0 here, so the two would visibly disagree.
    #[test]
    fn a_missing_size_falls_back_to_the_primary_not_the_first_entry() {
        let mut map = map();
        let first = map.primary().px_size();
        assert!(map.ensure_primary(28.0));
        assert_ne!(first, 28.0, "the primary must not be the first entry");

        // `index_for` is the single resolution point both `get` and `get_mut`
        // go through, so testing it covers the write path too — the write path
        // is where the divergence actually was, and its own fallback is only
        // reachable when `FontGrid::new` fails, which a test cannot force.
        let missing = PpemKey::new(99.0);
        assert_eq!(
            map.index_for(missing),
            map.primary_index(),
            "a miss must resolve to the primary, not to the first entry"
        );
        assert_ne!(
            map.primary_index(),
            0,
            "and the primary is not index 0 here"
        );
    }

    /// Float noise in one logical size must hit; genuinely different sizes
    /// must miss. Shared with `noa_render`'s atlas key, which quantizes the
    /// same way.
    #[test]
    fn ppem_key_quantizes_to_a_sixty_fourth_of_a_pixel() {
        assert_eq!(PpemKey::new(14.0), PpemKey::new(14.0 + f32::EPSILON));
        assert_ne!(PpemKey::new(14.0), PpemKey::new(14.05));
    }
}
