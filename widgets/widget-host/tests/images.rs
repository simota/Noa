//! Offline previews draw packaged images from the manifest's own package
//! directory, and fall back to alt text in the same box when one cannot be
//! decoded.

use std::path::Path;
use std::process::Command;

use serde_json::json;

/// A solid PNG of one color, generated here so the test owns its pixels.
fn solid_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&rgb.repeat((width * height) as usize))
        .unwrap();
    writer.finish().unwrap();
    bytes
}

fn package(dir: &Path, image: &[u8], shape: &str) -> std::path::PathBuf {
    let package = dir.join("pictures");
    std::fs::create_dir_all(package.join("assets")).unwrap();
    std::fs::write(package.join("assets/swatch.png"), image).unwrap();
    let manifest = json!({
        "apiVersion": "noa.widget/v1", "id": "pictures", "version": "1.0.0",
        "title": "Pictures", "description": "One packaged image",
        "assets": {"swatch": {"path": "assets/swatch.png"}},
        "dataSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "initialData": {},
        "view": {"type": "column", "id": "root", "children": [
            {"type": "image", "id": "swatch", "src": "swatch", "alt": "Swatch", "size": "lg", "shape": shape}
        ]}
    });
    let path = package.join("widget.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    path
}

/// Pixels of the rendered preview that are close to `rgb`.
fn count_close(png: &[u8], rgb: [u8; 3]) -> usize {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    let channels = info.line_size / info.width as usize;
    buffer[..info.buffer_size()]
        .chunks(channels)
        .filter(|p| (0..3).all(|i| p[i].abs_diff(rgb[i]) <= 8))
        .count()
}

fn render(manifest: &Path, output: &Path) {
    let result = Command::new(env!("CARGO_BIN_EXE_widget-host"))
        .arg(format!("--manifest={}", manifest.display()))
        .arg(format!("--snapshot={}", output.display()))
        .arg("--size=480x480")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn a_packaged_png_fills_its_image_box_in_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let red = [230, 30, 40];
    let manifest = package(dir.path(), &solid_png(8, 8, red), "square");
    let output = dir.path().join("preview.png");
    render(&manifest, &output);
    // lg is a 192-point square, drawn at 2x: 384x384 device pixels.
    let painted = count_close(&std::fs::read(&output).unwrap(), red);
    assert!(
        (384 * 384 * 9 / 10..=384 * 384 + 1600).contains(&painted),
        "{painted} red pixels"
    );
}

#[test]
fn a_circle_paints_about_a_disc_of_the_box() {
    let dir = tempfile::tempdir().unwrap();
    let blue = [20, 90, 230];
    let manifest = package(dir.path(), &solid_png(8, 8, blue), "circle");
    let output = dir.path().join("preview.png");
    render(&manifest, &output);
    let painted = count_close(&std::fs::read(&output).unwrap(), blue) as f64;
    let disc = std::f64::consts::PI * 192.0 * 192.0;
    assert!(
        (disc * 0.9..disc * 1.05).contains(&painted),
        "{painted} blue pixels, disc {disc}"
    );
}

#[test]
fn an_undecodable_image_keeps_its_box_and_shows_alt_text() {
    let dir = tempfile::tempdir().unwrap();
    let red = [230, 30, 40];
    let mut broken = solid_png(8, 8, red);
    // Keep the signature and IHDR (so validation accepts the header), but
    // corrupt the compressed image data.
    let len = broken.len();
    broken[37..len - 12].fill(0xa5);
    let manifest = package(dir.path(), &broken, "square");
    let output = dir.path().join("preview.png");
    render(&manifest, &output);
    assert_eq!(
        count_close(&std::fs::read(&output).unwrap(), red),
        0,
        "nothing of the broken picture is drawn"
    );
}

#[test]
fn a_symlinked_asset_is_refused_before_drawing() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = package(dir.path(), &solid_png(8, 8, [1, 2, 3]), "square");
    let asset = manifest.parent().unwrap().join("assets/swatch.png");
    let outside = dir.path().join("outside.png");
    std::fs::rename(&asset, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &asset).unwrap();
    let output = dir.path().join("preview.png");
    let result = Command::new(env!("CARGO_BIN_EXE_widget-host"))
        .arg(format!("--manifest={}", manifest.display()))
        .arg(format!("--snapshot={}", output.display()))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("symbolic link"));
    assert!(!output.exists());
}

#[test]
fn catalog_previews_upload_the_current_package_images_each_time() {
    let repo = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(repo.path())
            .status()
            .unwrap()
            .success()
    );
    let registry = repo.path().join(".noa-widgets");
    let red = [230, 30, 40];
    let blue = [20, 90, 230];
    let manifest = package(&registry, &solid_png(8, 8, red), "square");
    let output = repo.path().join("catalog-preview.png");
    let render = || {
        let result = Command::new(env!("CARGO_BIN_EXE_widget-host"))
            .arg(format!("--repo={}", repo.path().display()))
            .arg("--preview-id=pictures")
            .arg(format!("--snapshot={}", output.display()))
            .arg("--size=480x720")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::read(&output).unwrap()
    };
    assert!(count_close(&render(), red) > 100_000);
    std::fs::write(
        manifest.parent().unwrap().join("assets/swatch.png"),
        solid_png(8, 8, blue),
    )
    .unwrap();
    let next = render();
    assert!(count_close(&next, blue) > 100_000);
    assert_eq!(count_close(&next, red), 0);
}
