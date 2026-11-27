//! "--repo=" opens the repository catalogue; args.rs parses that public
//! launch contract alongside offline previews and the private live pipe.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use noa_gpui_platform::{App, AppOptions, Headless, Screen, Setup};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;
use noa_widget::Manifest;
use serde_json::Value;

use widget_host::args::{self, DEFAULT_SIZE, USAGE};
use widget_host::bridge::{self, Bridge};
use widget_host::catalog::Catalog;
use widget_host::screen::WidgetScreen;

fn read_capped(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds the size limit", path.display()));
    }
    Ok(bytes)
}

fn options(title: String) -> AppOptions {
    AppOptions {
        title,
        size: (DEFAULT_SIZE.0 as f64, DEFAULT_SIZE.1 as f64),
        min_size: Some((480.0, 360.0)),
        ..AppOptions::new("Noa Repository Widgets")
    }
}

fn snapshot(screen: &mut WidgetScreen, path: &Path, size: (u32, u32)) -> Result<(), String> {
    // The software renderer makes previews available without a window,
    // display server or GPU adapter and is deterministic for a given font set.
    let mut renderer = Headless::software();
    screen.upload_images(&mut |width, height, rgba| renderer.create_image(width, height, rgba));
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(size.0 as f32, size.1 as f32), &mut text);
    renderer
        .render_screen(screen, &mut text, size, 2.0)
        .map_err(|error| error.to_string())?
        .save_png(path)
        .map_err(|error| format!("write {}: {error}", path.display()))
}

/// Images can only be uploaded while the window's GPU is set up.
fn upload(screen: &mut WidgetScreen, setup: &mut Setup<'_>) {
    screen.upload_images(&mut |width, height, rgba| {
        setup
            .renderer
            .create_image(setup.device, setup.queue, width, height, rgba)
    });
}

fn main() -> Result<(), String> {
    let args = args::parse(std::env::args().skip(1))?;
    if args.help {
        println!("{USAGE}");
        return Ok(());
    }
    if args.bridge {
        let (snapshot, assets, reader) = bridge::bootstrap()?;
        let mut screen = WidgetScreen::from_bridge(snapshot, &assets)?;
        drop(assets);
        let title = format!("Noa · {}", screen.title());
        return App::new(options(title), move |setup| {
            upload(&mut screen, setup);
            let waker = setup.waker.clone();
            screen.attach_bridge(Bridge::start(reader, move || waker.wake()));
            Box::new(screen)
        })
        .run()
        .map_err(|error| error.to_string());
    }
    if let Some(repo) = args.repo {
        let screen = Catalog::open(&repo)?;
        return App::new(options("Noa · Repository Widgets".into()), move |_| {
            Box::new(screen)
        })
        .run()
        .map_err(|error| error.to_string());
    }
    let path = args.manifest.ok_or("--manifest is required")?;
    let manifest = Manifest::from_slice(&read_capped(&path, noa_widget::MAX_MANIFEST_BYTES)?)
        .map_err(|error| error.to_string())?;
    let data: Value = match args.data {
        Some(path) => serde_json::from_slice(&read_capped(&path, noa_widget::MAX_DATA_BYTES)?)
            .map_err(|error| error.to_string())?,
        None => manifest.initial_data.clone(),
    };
    // Offline previews read the package directly: the manifest's directory.
    let package = path.parent().ok_or("--manifest has no package directory")?;
    let assets = noa_widget::assets::load(package, &manifest).map_err(|error| error.to_string())?;
    let title = format!("Noa · {} · Preview", manifest.title);
    let mut screen = WidgetScreen::preview_package(manifest, data, &assets)?;
    drop(assets);
    if let Some(path) = args.snapshot {
        snapshot(&mut screen, &path, args.size.unwrap_or(DEFAULT_SIZE))?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    App::new(options(title), move |setup| {
        upload(&mut screen, setup);
        Box::new(screen)
    })
    .run()
    .map_err(|error| error.to_string())
}
