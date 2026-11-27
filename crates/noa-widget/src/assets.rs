//! Images packaged with a widget definition. Assets are read from the
//! package directory only, one path component at a time without following
//! symbolic links, and are identified by their content. This module checks
//! format, size and dimensions from the file headers; it never decodes pixels
//! (the renderer does, and re-checks what it decodes).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::Path;

use serde::Serialize;
use serde_json::json;

use crate::discovery::{at, entry_mode, open_directory, open_file, open_root};
use crate::{AssetRef, Manifest, WidgetError, check_id, content_digest};

pub const MAX_ASSETS: usize = 16;
pub const MAX_ASSET_BYTES: usize = 512 * 1024;
pub const MAX_PACKAGE_ASSET_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ASSET_DIMENSION: u32 = 2048;
pub const MAX_PATH_BYTES: usize = 256;
/// Every asset path starts with this directory inside the package.
pub const ASSETS_DIR: &str = "assets";
const MAX_PATH_COMPONENTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

/// One verified asset. `digest` is the sha256 content digest of `bytes`.
#[derive(Clone, PartialEq, Eq)]
pub struct Asset {
    pub id: String,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub digest: String,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Asset")
            .field("id", &self.id)
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("digest", &self.digest)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl Asset {
    /// Verify bytes that claim to be asset `id`. With `expected_digest`, the
    /// bytes must hash to it (a transported asset); the format, size and
    /// dimension limits always apply.
    pub fn verify(
        id: &str,
        bytes: Vec<u8>,
        expected_digest: Option<&str>,
    ) -> Result<Self, WidgetError> {
        check_id(id)?;
        let digest = content_digest(&bytes);
        if expected_digest.is_some_and(|expected| expected != digest) {
            return Err(WidgetError::new(format!(
                "asset {id:?} does not match its digest"
            )));
        }
        let (format, width, height) =
            inspect(&bytes).map_err(|e| WidgetError::new(format!("asset {id:?}: {e}")))?;
        Ok(Self {
            id: id.to_owned(),
            format,
            width,
            height,
            digest,
            bytes,
        })
    }
}

/// Syntax and count checks for a manifest's `assets` declaration. These need
/// no package directory, so inline definitions get the same messages.
pub(crate) fn validate_declarations(
    assets: Option<&BTreeMap<String, AssetRef>>,
) -> Result<(), WidgetError> {
    let Some(assets) = assets else {
        return Ok(());
    };
    if assets.is_empty() || assets.len() > MAX_ASSETS {
        return Err(WidgetError::new(format!(
            "assets must declare 1..={MAX_ASSETS} images; omit the field when there are none"
        )));
    }
    let mut paths = BTreeSet::new();
    for (id, asset) in assets {
        check_id(id)?;
        path_components(&asset.path).map_err(|e| WidgetError::new(format!("asset {id:?}: {e}")))?;
        if !paths.insert(asset.path.as_str()) {
            return Err(WidgetError::new(format!(
                "asset {id:?} reuses path {:?}",
                asset.path
            )));
        }
    }
    Ok(())
}

/// An asset path is `assets/<name>` or a few nested names below `assets/`,
/// relative to the package directory. No absolute paths, `.`/`..`, hidden
/// names, backslashes or control characters.
fn path_components(path: &str) -> Result<Vec<&str>, WidgetError> {
    let invalid = || {
        WidgetError::new(format!(
            "path {path:?} must be a relative path below {ASSETS_DIR}/ (at most {MAX_PATH_BYTES} bytes, no '..', hidden names or backslashes)"
        ))
    };
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return Err(invalid());
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 || parts.len() > MAX_PATH_COMPONENTS || parts[0] != ASSETS_DIR {
        return Err(invalid());
    }
    for part in &parts {
        if part.is_empty()
            || part.starts_with('.')
            || part.chars().any(|c| c == '\\' || c.is_control())
        {
            return Err(invalid());
        }
    }
    Ok(parts)
}

/// Load and verify a manifest's assets from its package directory (the
/// directory that contains `widget.json`). Used by offline validation and
/// preview, where the package is named by a path.
pub fn load(package: &Path, manifest: &Manifest) -> Result<Vec<Asset>, WidgetError> {
    if manifest.assets.is_none() {
        return Ok(Vec::new());
    }
    let directory = open_root(package)?;
    load_at(&directory, package, manifest)
}

/// Load assets relative to an already opened package directory.
pub(crate) fn load_at(
    directory: &OwnedFd,
    directory_path: &Path,
    manifest: &Manifest,
) -> Result<Vec<Asset>, WidgetError> {
    let Some(declared) = &manifest.assets else {
        return Ok(Vec::new());
    };
    let mut total = 0usize;
    let mut assets = Vec::with_capacity(declared.len());
    for (id, asset) in declared {
        let path = directory_path.join(&asset.path);
        let bytes = read_asset(directory, &asset.path).map_err(|e| at(&path, e))?;
        total += bytes.len();
        if total > MAX_PACKAGE_ASSET_BYTES {
            return Err(WidgetError::new(format!(
                "{}: assets exceed {} MiB per package",
                directory_path.display(),
                MAX_PACKAGE_ASSET_BYTES / (1024 * 1024)
            )));
        }
        assets.push(Asset::verify(id, bytes, None).map_err(|e| at(&path, e))?);
    }
    Ok(assets)
}

fn read_asset(directory: &OwnedFd, path: &str) -> Result<Vec<u8>, WidgetError> {
    let parts = path_components(path)?;
    let (name, parents) = parts.split_last().expect("at least two components");
    let mut owned: Option<OwnedFd> = None;
    for part in parents {
        let parent = owned.as_ref().unwrap_or(directory);
        reject_link(parent, part)?;
        owned = Some(open_directory(parent, OsStr::new(part))?);
    }
    let parent = owned.as_ref().unwrap_or(directory);
    reject_link(parent, name)?;
    let file = open_file(parent, OsStr::new(name))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(WidgetError::new("asset must be a regular file"));
    }
    if metadata.len() > MAX_ASSET_BYTES as u64 {
        return Err(WidgetError::new(format!(
            "asset exceeds {} KiB",
            MAX_ASSET_BYTES / 1024
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ASSET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(WidgetError::new(format!(
            "asset exceeds {} KiB",
            MAX_ASSET_BYTES / 1024
        )));
    }
    Ok(bytes)
}

fn reject_link(parent: &OwnedFd, name: &str) -> Result<(), WidgetError> {
    // O_NOFOLLOW also refuses a link swapped in after this check; this
    // check gives the common case a clear message instead of ELOOP.
    if entry_mode(parent, OsStr::new(name))? & libc::S_IFMT == libc::S_IFLNK {
        return Err(WidgetError::new(format!(
            "{name:?}: symbolic links are not allowed in asset paths"
        )));
    }
    Ok(())
}

/// Content digests by asset id, the form stored with a pinned instance.
pub fn digests(assets: &[Asset]) -> BTreeMap<String, String> {
    assets
        .iter()
        .map(|asset| (asset.id.clone(), asset.digest.clone()))
        .collect()
}

/// The pinned identity of a definition. Without `assets` it is exactly
/// [`Manifest::digest`]; with assets it also covers each asset's content
/// digest, so changing one image byte changes the definition.
pub fn definition_digest(
    manifest: &Manifest,
    asset_digests: &BTreeMap<String, String>,
) -> Result<String, WidgetError> {
    let declared = manifest.asset_ids();
    if declared.len() != asset_digests.len()
        || asset_digests
            .keys()
            .any(|id| !declared.contains(id.as_str()))
    {
        return Err(WidgetError::new(
            "asset digests do not match the manifest's declared assets",
        ));
    }
    if manifest.assets.is_none() {
        return Ok(manifest.digest());
    }
    for digest in asset_digests.values() {
        if !is_content_digest(digest) {
            return Err(WidgetError::new("invalid asset digest"));
        }
    }
    let manifest_digest = manifest.digest();
    // BTreeMap serializes in key order: the wrapper is canonical.
    let identity = json!({"assetDigests": asset_digests, "manifestDigest": manifest_digest});
    Ok(content_digest(
        &serde_json::to_vec(&identity).expect("JSON values serialize"),
    ))
}

fn is_content_digest(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Format by magic bytes (never by file extension) and the dimensions the
/// header declares. Animated WebP and arithmetic-coded/lossless JPEG are
/// refused: the renderer shows still images only.
pub fn inspect(bytes: &[u8]) -> Result<(ImageFormat, u32, u32), WidgetError> {
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(WidgetError::new(format!(
            "image exceeds {} KiB",
            MAX_ASSET_BYTES / 1024
        )));
    }
    let (format, size) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        (ImageFormat::Png, png_size(bytes))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        (ImageFormat::Jpeg, jpeg_size(bytes)?)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        (ImageFormat::Webp, webp_size(bytes)?)
    } else {
        return Err(WidgetError::new(
            "unsupported image format: use PNG, JPEG or WebP",
        ));
    };
    let (width, height) =
        size.ok_or_else(|| WidgetError::new(format!("{format:?} header is truncated or invalid")))?;
    if width == 0 || height == 0 || width > MAX_ASSET_DIMENSION || height > MAX_ASSET_DIMENSION {
        return Err(WidgetError::new(format!(
            "image is {width}x{height}; dimensions must be 1..={MAX_ASSET_DIMENSION} pixels"
        )));
    }
    Ok((format, width, height))
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        bytes.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        bytes.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    // The first chunk is always IHDR (13 bytes of data).
    if bytes.get(8..16)? != b"\0\0\0\x0dIHDR" {
        return None;
    }
    let word = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    Some((word(16)?, word(20)?))
}

fn jpeg_size(bytes: &[u8]) -> Result<Option<(u32, u32)>, WidgetError> {
    let mut at = 2;
    loop {
        if bytes.get(at) != Some(&0xff) {
            return Ok(None);
        }
        while bytes.get(at) == Some(&0xff) {
            at += 1;
        }
        let Some(&marker) = bytes.get(at) else {
            return Ok(None);
        };
        at += 1;
        match marker {
            0x01 | 0xd0..=0xd7 => continue,
            // End of image or scan data before a frame header.
            0xd8..=0xda => return Ok(None),
            // Baseline, extended and progressive Huffman frames.
            0xc0..=0xc2 => {
                return Ok(be16(bytes, at + 5).zip(be16(bytes, at + 3)));
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(WidgetError::new(
                    "unsupported JPEG encoding: use baseline or progressive JPEG",
                ));
            }
            _ => {}
        }
        let Some(length) = be16(bytes, at) else {
            return Ok(None);
        };
        if length < 2 {
            return Ok(None);
        }
        at += length as usize;
    }
}

fn webp_size(bytes: &[u8]) -> Result<Option<(u32, u32)>, WidgetError> {
    let Some(riff) = bytes.get(4..8) else {
        return Ok(None);
    };
    let riff = u32::from_le_bytes(riff.try_into().expect("four bytes")) as usize;
    if riff.checked_add(8).is_none_or(|end| end > bytes.len()) {
        return Ok(None);
    }
    Ok(match bytes.get(12..16) {
        Some(b"VP8 ") => {
            if bytes.get(23..26) != Some(&[0x9d, 0x01, 0x2a]) {
                return Ok(None);
            }
            le16(bytes, 26)
                .zip(le16(bytes, 28))
                .map(|(w, h)| (w & 0x3fff, h & 0x3fff))
        }
        Some(b"VP8L") => {
            if bytes.get(20) != Some(&0x2f) {
                return Ok(None);
            }
            bytes.get(21..25).map(|b| {
                let bits = u32::from_le_bytes(b.try_into().expect("four bytes"));
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            })
        }
        Some(b"VP8X") => {
            let Some(&flags) = bytes.get(20) else {
                return Ok(None);
            };
            if flags & 0x02 != 0 {
                return Err(WidgetError::new(
                    "animated WebP is not supported; use a still image",
                ));
            }
            le24(bytes, 24)
                .zip(le24(bytes, 27))
                .map(|(w, h)| (w + 1, h + 1))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_below_the_assets_directory() {
        for good in [
            "assets/logo.png",
            "assets/icons/a b.webp",
            "assets/x/y/z.jpg",
        ] {
            path_components(good).unwrap();
        }
        for bad in [
            "",
            "logo.png",
            "/assets/logo.png",
            "assets/",
            "assets//logo.png",
            "assets/../widget.json",
            "assets/./logo.png",
            "assets/.hidden.png",
            "assets\\logo.png",
            "assets/a\\b.png",
            "assets/a/b/c/d.png",
            "other/logo.png",
            "assets/line\nbreak.png",
        ] {
            assert!(path_components(bad).is_err(), "{bad:?}");
        }
        assert!(path_components(&format!("assets/{}", "a".repeat(300))).is_err());
    }

    #[test]
    fn headers_give_format_and_dimensions() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(3u32.to_be_bytes());
        png.extend(2u32.to_be_bytes());
        assert_eq!(inspect(&png).unwrap(), (ImageFormat::Png, 3, 2));
        let mut huge = png.clone();
        huge[16..20].copy_from_slice(&4096u32.to_be_bytes());
        assert!(inspect(&huge).unwrap_err().to_string().contains("2048"));
        // SOI, an APP0 segment, then a baseline frame of 5x7.
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 11, 8, 0, 7, 0, 5, 1, 1, 0x11, 0,
        ];
        assert_eq!(inspect(&jpeg).unwrap(), (ImageFormat::Jpeg, 5, 7));
        let mut arithmetic = jpeg;
        arithmetic[9] = 0xc9;
        assert!(inspect(&arithmetic).is_err());
        let mut webp = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        webp.extend(((9u32 - 1) | ((4u32 - 1) << 14)).to_le_bytes());
        let riff = (webp.len() - 8) as u32;
        webp[4..8].copy_from_slice(&riff.to_le_bytes());
        assert_eq!(inspect(&webp).unwrap(), (ImageFormat::Webp, 9, 4));
        let mut animated = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\x02\0\0\0\0\0\0\0\0\0".to_vec();
        let riff = (animated.len() - 8) as u32;
        animated[4..8].copy_from_slice(&riff.to_le_bytes());
        assert!(
            inspect(&animated)
                .unwrap_err()
                .to_string()
                .contains("animated")
        );
        assert!(inspect(b"GIF89a....").is_err());
        assert!(inspect(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(inspect(&png[..12]).is_err());
    }
}
