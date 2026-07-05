//! Embeds two build-time env vars consumed only by the About panel
//! (`app_actions::version_string()`): `NOA_GIT_HASH` (7-char short hash, or
//! empty when git is unavailable) and `NOA_BUILD_DATE` (UTC `YYYY-MM-DD`,
//! independent of git). Both are always emitted so `env!()` at the call site
//! stays compilable regardless of the build environment.

use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rustc-env=NOA_GIT_HASH={}", git_short_hash());
    println!("cargo:rustc-env=NOA_BUILD_DATE={}", build_date());

    // Only these paths affect the two env vars above; anything else in the
    // workspace must not trigger a build.rs re-run. `packed-refs` only
    // exists after `git gc` packs loose refs — cargo treats a watched path
    // that doesn't exist as always-changed, so it must be watched
    // conditionally or every build would re-run this script (NFR-3/AC-10).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    if std::path::Path::new("../../.git/packed-refs").exists() {
        println!("cargo:rerun-if-changed=../../.git/packed-refs");
    }
}

/// The repo's short hash, or `""` when not in a git checkout (tarball build,
/// git missing, etc.) — R-5 keeps that failure non-fatal.
fn git_short_hash() -> String {
    let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
    else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    String::from_utf8(output.stdout)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// UTC `YYYY-MM-DD`: `SOURCE_DATE_EPOCH` when set (reproducible builds),
/// otherwise the current UTC date — independent of git availability (R-4).
fn build_date() -> String {
    match env::var("SOURCE_DATE_EPOCH") {
        Ok(epoch) => match epoch.trim().parse::<i64>() {
            Ok(secs) => utc_date_from_unix(secs),
            Err(_) => current_utc_date(),
        },
        Err(_) => current_utc_date(),
    }
}

fn current_utc_date() -> String {
    let Ok(output) = Command::new("date").args(["-u", "+%Y-%m-%d"]).output() else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    String::from_utf8(output.stdout)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Hand-rolled civil-from-days conversion (Howard Hinnant's algorithm) so no
/// new date/time crate is needed just to format `SOURCE_DATE_EPOCH` (NFR-1).
fn utc_date_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_date_from_unix_matches_known_epoch_values() {
        assert_eq!(utc_date_from_unix(0), "1970-01-01");
        assert_eq!(utc_date_from_unix(1_751_673_600), "2025-07-05");
    }
}
