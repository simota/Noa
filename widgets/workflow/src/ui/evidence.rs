//! Read-only proof previews verify the entire bounded file before truncating.
//! A changed file is never presented as the proof accepted by the runner.

use std::path::Path;

use workflow::adapters::fs;

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PREVIEW_CHARS: usize = 120_000;

/// Open only ordinary files through the adapter's descriptor-relative,
/// no-follow reader. The returned string is display text, never terminal input.
pub(super) fn preview(path: &Path, expected_sha: &str) -> Result<String, String> {
    if expected_sha.len() != 64 || !expected_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("証跡のSHA-256が不正です。保存済みの証跡を選び直してください".into());
    }
    let bytes = fs::read(path, MAX_BYTES)?
        .ok_or("証跡ファイルが見つかりません。保存先を確認してください")?;
    if !fs::hash(&bytes).eq_ignore_ascii_case(expected_sha) {
        return Err("証跡ファイルが検証後に変更されています。以前の証跡として表示しません".into());
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "この証跡はUTF-8のテキストではないため、テキスト表示できません")?;
    let mut preview = String::with_capacity(text.len().min(MAX_PREVIEW_CHARS));
    let mut displayed = 0;
    let mut sanitized = false;
    let mut truncated = false;
    for character in text.chars() {
        let replacement = if (character.is_control() && !matches!(character, '\n' | '\t'))
            || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
        {
            Some(format!("[U+{:04X}]", character as u32))
        } else {
            None
        };
        let count = replacement.as_ref().map_or(1, |text| text.len());
        if displayed + count > MAX_PREVIEW_CHARS {
            truncated = true;
            break;
        }
        match replacement {
            Some(value) => {
                sanitized = true;
                preview.push_str(&value);
            }
            None => preview.push(character),
        }
        displayed += count;
    }
    if truncated {
        preview.push_str("\n\n［表示上限：先頭から最大120,000文字を表示しています。ファイル全体のSHA-256は照合済みです。］");
    }
    if sanitized {
        preview.push_str("\n\n［制御文字と双方向表示制御は [U+XXXX] の形で表示しています。］");
    }
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let temporary = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(temporary.path())
            .unwrap()
            .join("evidence.log");
        std::fs::write(&path, contents).unwrap();
        (temporary, path, fs::hash(contents))
    }

    #[test]
    fn matching_text_is_read_without_changing_it() {
        let (_temporary, path, hash) = fixture("検証結果\n2 passed\t0 failed\n".as_bytes());
        assert_eq!(
            preview(&path, &hash).unwrap(),
            "検証結果\n2 passed\t0 failed\n"
        );
        assert!(preview(&path, &hash.to_uppercase()).is_ok());
    }

    #[test]
    fn a_change_beyond_the_visible_prefix_is_still_rejected() {
        let original = "a".repeat(MAX_PREVIEW_CHARS + 20);
        let (_temporary, path, hash) = fixture(original.as_bytes());
        let mut changed = original.into_bytes();
        *changed.last_mut().unwrap() = b'b';
        std::fs::write(&path, changed).unwrap();
        assert!(preview(&path, &hash).unwrap_err().contains("変更"));
    }

    #[test]
    fn parent_and_final_symlinks_are_refused() {
        use std::os::unix::fs::symlink;
        let (temporary, path, hash) = fixture(b"verified");
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let direct = root.join("linked.log");
        symlink(&path, &direct).unwrap();
        assert!(preview(&direct, &hash).is_err());
        let parent = root.join("linked-parent");
        symlink(&root, &parent).unwrap();
        assert!(preview(&parent.join("evidence.log"), &hash).is_err());
    }

    #[test]
    fn an_oversized_file_is_refused_before_reading_it() {
        let (_temporary, path, _) = fixture(b"");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(
            preview(&path, &"0".repeat(64))
                .unwrap_err()
                .contains("上限")
        );
    }

    #[test]
    fn large_utf8_logs_are_truncated_at_character_boundaries() {
        let contents = "証".repeat(MAX_PREVIEW_CHARS + 20);
        let (_temporary, path, hash) = fixture(contents.as_bytes());
        let text = preview(&path, &hash).unwrap();
        assert_eq!(
            text.chars().take(MAX_PREVIEW_CHARS).collect::<String>(),
            "証".repeat(MAX_PREVIEW_CHARS)
        );
        assert!(text.contains("表示上限"));
        assert!(text.chars().count() < MAX_PREVIEW_CHARS + 150);
    }

    #[test]
    fn non_utf8_evidence_is_not_lossily_rewritten() {
        let (_temporary, path, hash) = fixture(&[0xff, 0xfe, 0x00]);
        assert!(preview(&path, &hash).unwrap_err().contains("UTF-8"));
    }

    #[test]
    fn terminal_and_bidi_controls_are_visible_not_executed_or_hidden() {
        let (_temporary, path, hash) =
            fixture("ok\u{001b}[31m\nnext\tvalue\r\u{202e}secret\u{200f}".as_bytes());
        let text = preview(&path, &hash).unwrap();
        for visible in ["[U+001B]", "[U+000D]", "[U+202E]", "[U+200F]"] {
            assert!(text.contains(visible));
        }
        assert!(
            !text
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
        );
        assert!(!text.contains('\u{202e}'));
    }

    #[test]
    fn invalid_hashes_and_missing_files_are_reported() {
        let (_temporary, path, _) = fixture(b"evidence");
        assert!(preview(&path, "not-a-hash").is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(
            preview(&path, &"0".repeat(64))
                .unwrap_err()
                .contains("見つかりません")
        );
    }
}
