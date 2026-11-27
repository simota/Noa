//! Packaged image assets and the `image` component: declaration grammar,
//! loading from a package directory, limits and the definition digest.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use noa_widget::assets::{self, MAX_ASSET_BYTES};
use noa_widget::{Asset, ImageFormat, Manifest, content_digest, definition_digest};
use serde_json::{Value, json};

const GALLERY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.noa-widgets/image-gallery"
);

fn gallery() -> Value {
    serde_json::from_slice(&fs::read(Path::new(GALLERY).join("widget.json")).unwrap()).unwrap()
}

fn parse(value: &Value) -> Result<Manifest, noa_widget::WidgetError> {
    Manifest::from_slice(&serde_json::to_vec(value).unwrap())
}

fn error(value: &Value) -> String {
    parse(value).unwrap_err().to_string()
}

/// A minimal manifest with one image node; `image` fields are merged in.
fn single(image: Value) -> Value {
    let mut node = json!({"type":"image","id":"picture","src":"logo","alt":"Logo"});
    for (key, value) in image.as_object().unwrap() {
        if value.is_null() {
            node.as_object_mut().unwrap().remove(key);
        } else {
            node[key] = value.clone();
        }
    }
    json!({
        "apiVersion":"noa.widget/v1","id":"pictures","version":"1.0.0","title":"Pictures",
        "description":"Images","assets":{"logo":{"path":"assets/logo.png"},"photo":{"path":"assets/photo.jpg"}},
        "dataSchema":{"type":"object","properties":{
            "pick":{"type":"string","enum":["logo","photo"]},
            "free":{"type":"string"},
            "other":{"type":"string","enum":["logo","nope"]},
            "count":{"type":"integer"},
            "caption":{"type":"string"}
        },"required":["pick","free","other","count","caption"],"additionalProperties":false},
        "initialData":{"pick":"photo","free":"logo","other":"logo","count":1,"caption":"Caption"},
        "view":node
    })
}

/// A synthetic PNG header (signature + IHDR); validation reads headers only.
fn png_header(width: u32, height: u32, total: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.resize(total.max(bytes.len()), 0);
    bytes
}

struct Package(PathBuf);

impl Package {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("noa-widget-assets-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("assets")).unwrap();
        Self(path)
    }

    fn write(&self, relative: &str, bytes: &[u8]) {
        fs::write(self.0.join(relative), bytes).unwrap();
    }

    fn load(&self, manifest: &Value) -> Result<Vec<Asset>, String> {
        assets::load(&self.0, &parse(manifest).unwrap()).map_err(|e| e.to_string())
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_example_package_validates_with_all_three_formats() {
    let manifest = parse(&gallery()).unwrap();
    let loaded = assets::load(Path::new(GALLERY), &manifest).unwrap();
    let formats: BTreeMap<_, _> = loaded
        .iter()
        .map(|a| (a.id.as_str(), (a.format, a.width, a.height)))
        .collect();
    assert_eq!(formats["logo"], (ImageFormat::Png, 96, 96));
    assert_eq!(formats["landscape"], (ImageFormat::Jpeg, 160, 96));
    assert_eq!(formats["pattern"], (ImageFormat::Webp, 96, 96));
    for fixture in ["landscape", "pattern"] {
        let data: Value = serde_json::from_slice(
            &fs::read(Path::new(GALLERY).join(format!("fixtures/{fixture}.json"))).unwrap(),
        )
        .unwrap();
        manifest.validate_data(&data).unwrap();
    }
}

#[test]
fn manifests_without_assets_keep_exactly_their_previous_digest() {
    let bytes = include_bytes!("../../../.noa-widgets/test-summary/widget.json");
    let manifest = Manifest::from_slice(bytes).unwrap();
    // The pre-assets digest: canonical (sorted-key) JSON of the manifest's
    // fields, with `actions` always present and no `assets` key.
    let mut raw: Value = serde_json::from_slice(bytes).unwrap();
    raw.as_object_mut()
        .unwrap()
        .entry("actions")
        .or_insert(json!([]));
    assert_eq!(
        manifest.digest(),
        content_digest(&serde_json::to_vec(&raw).unwrap())
    );
    assert_eq!(
        manifest.digest(),
        "sha256:5d76b30d5e960bbab1f14c91408635243a0c522a570b7c1ad59d5c35a2614ddb"
    );
    assert_eq!(
        definition_digest(&manifest, &BTreeMap::new()).unwrap(),
        manifest.digest()
    );
    assert!(!serde_json::to_string(&manifest).unwrap().contains("assets"));
}

#[test]
fn the_definition_digest_covers_every_asset_byte() {
    let manifest = parse(&gallery()).unwrap();
    let loaded = assets::load(Path::new(GALLERY), &manifest).unwrap();
    let digests = assets::digests(&loaded);
    let pinned = definition_digest(&manifest, &digests).unwrap();
    assert_ne!(pinned, manifest.digest());
    for id in digests.keys() {
        let mut changed = digests.clone();
        changed.insert(id.clone(), content_digest(b"different bytes"));
        assert_ne!(
            definition_digest(&manifest, &changed).unwrap(),
            pinned,
            "{id}"
        );
    }
    let mut missing = digests.clone();
    missing.remove("logo");
    assert!(definition_digest(&manifest, &missing).is_err());
    let mut extra = digests.clone();
    extra.insert("extra".into(), content_digest(b"x"));
    assert!(definition_digest(&manifest, &extra).is_err());
    let mut malformed = digests;
    malformed.insert("logo".into(), "md5:abc".into());
    assert!(definition_digest(&manifest, &malformed).is_err());
}

#[test]
fn asset_declarations_are_bounded_and_fail_closed() {
    let mut value = gallery();
    value["assets"] = json!({});
    assert!(error(&value).contains("1..=16"));
    let mut value = gallery();
    value["assets"]["logo"]["sha256"] = json!("pinned");
    assert!(parse(&value).is_err());
    let mut value = gallery();
    value["assets"]["logo"] = json!("assets/logo.png");
    assert!(parse(&value).is_err());
    let mut value = gallery();
    value["assets"]["Bad_Id"] = json!({"path":"assets/x.png"});
    assert!(error(&value).contains("invalid id"));
    let mut value = gallery();
    value["assets"]["copy"] = json!({"path":"assets/logo.png"});
    assert!(error(&value).contains("reuses path"));
    let mut value = gallery();
    for index in 0..17 {
        value["assets"][format!("a{index}")] = json!({"path": format!("assets/{index}.png")});
    }
    assert!(error(&value).contains("1..=16"));
    for path in [
        "/etc/passwd",
        "../outside.png",
        "assets/../widget.json",
        "assets/./logo.png",
        "widget.json",
        "logo.png",
        "https://example.com/logo.png",
        "data:image/png;base64,AAAA",
        "assets\\logo.png",
        "assets/.hidden.png",
    ] {
        let mut value = gallery();
        value["assets"]["logo"]["path"] = json!(path);
        assert!(error(&value).contains("relative path"), "{path}");
    }
}

#[test]
fn image_components_require_alt_and_declared_sources() {
    assert!(parse(&single(json!({}))).is_ok());
    assert!(parse(&single(json!({"src":{"source":"data","pointer":"/pick"}}))).is_ok());
    assert!(
        parse(&single(
            json!({"alt":{"source":"data","pointer":"/caption"}})
        ))
        .is_ok()
    );
    for (fields, message) in [
        (json!({"alt":null}), "missing field"),
        (json!({"src":null}), "missing field"),
        (json!({"alt":" "}), "image alt"),
        (json!({"src":"photo2"}), "undeclared asset"),
        (json!({"src":"assets/logo.png"}), "undeclared asset"),
        (json!({"src":{"source":"data","pointer":"/free"}}), "enum"),
        (
            json!({"src":{"source":"data","pointer":"/other"}}),
            "undeclared asset",
        ),
        (
            json!({"src":{"source":"data","pointer":"/count"}}),
            "incompatible",
        ),
        (
            json!({"src":{"source":"data","pointer":"/absent"}}),
            "not declared",
        ),
        (
            json!({"alt":{"source":"data","pointer":"/count"}}),
            "incompatible",
        ),
        (
            json!({"url":"https://example.com/x.png"}),
            "unsupported view node field",
        ),
        (json!({"size":"xl"}), "size"),
        (json!({"size":"md","width":10,"height":10}), "not both"),
        (json!({"width":10}), "together"),
        (json!({"width":0,"height":10}), "1..=1024"),
        (json!({"width":1025,"height":10}), "1..=1024"),
        (json!({"width":1.5,"height":10}), "1..=1024"),
        (json!({"shape":"oval"}), "shape"),
    ] {
        let message_seen = error(&single(fields.clone()));
        assert!(message_seen.contains(message), "{fields}: {message_seen}");
    }
    // A manifest without assets cannot show an image at all.
    let mut value = single(json!({}));
    value.as_object_mut().unwrap().remove("assets");
    assert!(error(&value).contains("undeclared asset"));
}

#[test]
fn bound_image_sources_must_be_declared_at_render_time() {
    let manifest = parse(&single(json!({"src":{"source":"data","pointer":"/pick"}}))).unwrap();
    let mut data = manifest.initial_data.clone();
    data["pick"] = json!("logo");
    manifest.validate_data(&data).unwrap();
    data["pick"] = json!("unknown");
    assert!(manifest.validate_data(&data).is_err());
    data["pick"] = json!(7);
    assert!(manifest.validate_data(&data).is_err());
}

#[test]
fn the_editor_schema_describes_assets_and_images() {
    let schema = noa_widget::schema_document();
    assert_eq!(schema["properties"]["assets"]["maxProperties"], 16);
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("assets"))
    );
    let image = schema["$defs"]["node"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["properties"]["type"]["enum"] == json!(["image"]))
        .unwrap();
    assert_eq!(image["required"], json!(["type", "id", "src", "alt"]));
    assert_eq!(image["properties"]["width"]["maximum"], 1024);
}

#[test]
fn loading_checks_existence_format_size_and_dimensions() {
    let package = Package::new("limits");
    let manifest = single(json!({}));
    assert!(package.load(&manifest).unwrap_err().contains("logo.png"));
    package.write("assets/logo.png", &png_header(8, 8, 64));
    // Formats come from magic bytes: a PNG named .jpg is a PNG.
    package.write("assets/photo.jpg", &png_header(8, 8, 64));
    let loaded = package.load(&manifest).unwrap();
    assert!(loaded.iter().all(|a| a.format == ImageFormat::Png));
    for (bytes, message) in [
        (b"GIF89a\x01\0\x01\0".to_vec(), "unsupported image format"),
        (b"<svg/>".to_vec(), "unsupported image format"),
        (png_header(2049, 8, 64), "2048"),
        (png_header(8, 0, 64), "dimensions"),
        (png_header(8, 8, MAX_ASSET_BYTES + 1), "512 KiB"),
    ] {
        package.write("assets/photo.jpg", &bytes);
        let seen = package.load(&manifest).unwrap_err();
        assert!(seen.contains(message), "{message}: {seen}");
    }
}

#[test]
fn a_package_holds_at_most_two_mebibytes_of_assets() {
    let package = Package::new("total");
    let mut value = single(json!({}));
    let mut declared = serde_json::Map::new();
    for index in 0..5 {
        let name = format!("assets/{index}.png");
        package.write(&name, &png_header(8, 8, 500 * 1024));
        declared.insert(format!("a{index}"), json!({"path": name}));
    }
    declared.insert("logo".into(), json!({"path":"assets/0.png"}));
    value["assets"] = Value::Object(declared.clone());
    value["assets"]["photo"] = json!({"path":"assets/1.png"});
    // Five 500 KiB images: over 2 MiB.
    value["assets"].as_object_mut().unwrap().remove("a0");
    value["assets"].as_object_mut().unwrap().remove("a1");
    assert!(package.load(&value).unwrap_err().contains("2 MiB"));
    value["assets"].as_object_mut().unwrap().remove("a4");
    assert_eq!(package.load(&value).unwrap().len(), 4);
}

#[test]
fn links_and_special_files_are_refused_at_every_component() {
    use std::os::unix::fs::symlink;
    let package = Package::new("links");
    let outside = Package::new("links-outside");
    outside.write("assets/logo.png", &png_header(8, 8, 64));
    package.write("assets/photo.jpg", &png_header(8, 8, 64));
    let manifest = single(json!({}));
    symlink(
        outside.0.join("assets/logo.png"),
        package.0.join("assets/logo.png"),
    )
    .unwrap();
    assert!(
        package
            .load(&manifest)
            .unwrap_err()
            .contains("symbolic link")
    );
    fs::remove_file(package.0.join("assets/logo.png")).unwrap();
    fs::remove_file(package.0.join("assets/photo.jpg")).unwrap();
    fs::remove_dir(package.0.join("assets")).unwrap();
    symlink(outside.0.join("assets"), package.0.join("assets")).unwrap();
    assert!(
        package
            .load(&manifest)
            .unwrap_err()
            .contains("symbolic link")
    );
    fs::remove_file(package.0.join("assets")).unwrap();
    fs::create_dir(package.0.join("assets")).unwrap();
    package.write("assets/photo.jpg", &png_header(8, 8, 64));
    fs::create_dir(package.0.join("assets/logo.png")).unwrap();
    assert!(
        package
            .load(&manifest)
            .unwrap_err()
            .contains("regular file")
    );
}
