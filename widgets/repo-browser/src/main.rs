mod git;
mod live;
mod model;
mod ui;

/// Heap use of test builds, for the benchmarks' allocation figures. The
/// counts are process-wide: read them with the benchmark running alone.
#[cfg(test)]
pub(crate) mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    struct Counting;

    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    static ALLOCS: AtomicUsize = AtomicUsize::new(0);

    fn grew(by: usize) {
        let live = LIVE.fetch_add(by, Relaxed) + by;
        PEAK.fetch_max(live, Relaxed);
    }

    // SAFETY: every call is passed on to `System` unchanged.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                ALLOCS.fetch_add(1, Relaxed);
                grew(layout.size());
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) };
            LIVE.fetch_sub(layout.size(), Relaxed);
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new = unsafe { System.realloc(ptr, layout, new_size) };
            if !new.is_null() {
                ALLOCS.fetch_add(1, Relaxed);
                if new_size >= layout.size() {
                    grew(new_size - layout.size());
                } else {
                    LIVE.fetch_sub(layout.size() - new_size, Relaxed);
                }
            }
            new
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    /// `f()` with the allocations it made and its peak heap use above what
    /// was live when it started, in bytes.
    pub fn measure<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
        let start = LIVE.load(Relaxed);
        PEAK.store(start, Relaxed);
        let allocs = ALLOCS.load(Relaxed);
        let out = f();
        (
            out,
            ALLOCS.load(Relaxed) - allocs,
            PEAK.load(Relaxed).saturating_sub(start),
        )
    }
}

use std::path::PathBuf;

use std::time::Duration;

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use crate::ui::{RepoBrowser, SnapshotView};

const USAGE: &str = "usage: repo-browser [--demo] [--worktree=PATH] \
                     [--snapshot=FILE.png [--size=WxH] [--merge | --commit] [--composer]]\n  \
                     live data from the running Noa control server by default; \
                     --demo shows canned multi-worktree data;\n  \
                     --worktree selects the worktree holding PATH;\n  \
                     --merge shows a predicted merge conflict, --commit the newest commit, \
                     --composer quotes the selection into the review prompt";
const DEFAULT_SIZE: (u32, u32) = (1440, 900);
const SNAPSHOT_SCALE: f32 = 2.0;
const FIRST_SNAPSHOT_WAIT: Duration = Duration::from_secs(3);
/// How long a live snapshot waits for the selected file's, commit's or
/// merge conflict's text after the listing.
const CONTENT_WAIT: Duration = Duration::from_secs(3);

fn options() -> AppOptions {
    AppOptions {
        title: "Noa · Repo Browser（横断リポジトリ＆差分レビュー）".into(),
        size: (1440.0, 900.0),
        ..AppOptions::new("Noa Repo Browser")
    }
}

fn parse_size(arg: &str) -> Result<(u32, u32), String> {
    let bad = || format!("bad size {arg:?}, expected WxH\n{USAGE}");
    let (w, h) = arg.split_once('x').ok_or_else(bad)?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if (1..=4096).contains(&w) && (1..=4096).contains(&h) => Ok((w, h)),
        _ => Err(bad()),
    }
}

/// Renders `screen` with the text store it was prepared with, if any.
fn render_snapshot(
    path: &PathBuf,
    screen: &mut dyn Screen,
    size: (u32, u32),
    text: &mut TextStore,
) -> Result<(), String> {
    let mut headless = Headless::new_or_software();
    let viewport = Size::new(size.0 as f32, size.1 as f32);
    screen.prepare(viewport, text);
    headless
        .render_screen(screen, text, size, SNAPSHOT_SCALE)
        .map_err(|e| e.to_string())?
        .save_png(path)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn main() -> Result<(), String> {
    let mut snapshot_path = None;
    let mut demo = false;
    let mut composer = false;
    let mut view = None;
    let mut worktree = None;
    let mut size = DEFAULT_SIZE;

    for arg in std::env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot_path = Some(PathBuf::from(path));
        } else if let Some(raw) = arg.strip_prefix("--size=") {
            size = parse_size(raw)?;
        } else if let Some(path) = arg.strip_prefix("--worktree=") {
            if path.trim().is_empty() {
                return Err(format!("--worktree needs a path\n{USAGE}"));
            }
            worktree = Some(path.to_owned());
        } else if arg == "--composer" {
            composer = true;
        } else if arg == "--merge" || arg == "--commit" {
            if view.is_some() {
                return Err(format!("--merge and --commit exclude each other\n{USAGE}"));
            }
            view = Some(arg);
        } else if arg == "--demo" {
            demo = true;
        } else if arg == "--help" || arg == "-h" {
            println!("{USAGE}");
            return Ok(());
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }

    if let Some(path) = snapshot_path {
        let view = match view.as_deref() {
            Some("--merge") => SnapshotView::Merge,
            Some("--commit") => SnapshotView::Commit,
            _ => SnapshotView::Diff,
        };
        // One text store from the first layout to the render.
        let mut text = TextStore::new(Default::default());
        let mut screen = if demo {
            let mut screen = RepoBrowser::demo();
            if let Some(path) = worktree {
                screen.preselect_worktree(path, &mut text);
            }
            match view {
                SnapshotView::Diff => {}
                SnapshotView::Merge => screen.show_first_merge(&mut text),
                SnapshotView::Commit => screen.show_first_commit(&mut text),
            }
            screen
        } else {
            // The live feed stays connected until the selection's text is in.
            RepoBrowser::live_snapshot(
                live::start(|| {}),
                worktree,
                view,
                &mut text,
                Size::new(size.0 as f32, size.1 as f32),
                FIRST_SNAPSHOT_WAIT,
                CONTENT_WAIT,
            )?
        };
        if composer {
            screen.show_composer();
        }
        render_snapshot(&path, &mut screen, size, &mut text)?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    App::new(options(), move |setup| {
        let mut screen = if demo {
            RepoBrowser::demo()
        } else {
            let waker = setup.waker.clone();
            let channel = live::start(move || waker.wake());
            RepoBrowser::live(channel)
        };
        if let Some(path) = worktree.take() {
            // Before the first frame: any text store will do.
            screen.preselect_worktree(path, &mut TextStore::new(Default::default()));
        }
        Box::new(screen)
    })
    .run()
    .map_err(|e| e.to_string())
}
