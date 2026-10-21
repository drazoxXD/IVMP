//! Stamps the build into the DLL for the game window's title: `IVMP_BUILD` = build date (UTC)
//! and the git commit, e.g. `2026-10-22 efea231` (`-dirty` with uncommitted changes).

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Year, month, day of a day count since 1970-01-01 (Howard Hinnant's civil_from_days).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let build = format!("{year:04}-{month:02}-{day:02} {commit}{}", if dirty { "-dirty" } else { "" });
    println!("cargo:rustc-env=IVMP_BUILD={build}");
    // A new commit or checkout changes HEAD; the icon is embedded with include_bytes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}
