//! Small, dependency-free archiver capability probe shared by build.rs and
//! the standalone lightweight operator test.

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PORTABLE_ARCHIVER_PROBE: &str = "DOOM-NATIVE-ARCHIVER-PROBE/1";

/// Return the selected archiver's capability marker after a real archive
/// create/list operation.  Apple's /usr/bin/ar does not accept --version, so
/// this fallback is the required identity check for that tool.  The probe
/// directory is created exactly once and is removed after the attempt.
pub fn archiver_version(path: &Path, required: bool) -> String {
    if let Ok(output) = Command::new(path).arg("--version").output() {
        if output.status.success() {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            let text = text.trim().to_owned();
            if !text.is_empty() {
                return text;
            }
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_nanos();
    let probe_dir = env::temp_dir().join(format!(
        "basanos-doom-ar-probe-{}-{nonce}",
        std::process::id()
    ));
    let member = probe_dir.join("member.o");
    let archive = probe_dir.join("libprobe.a");
    let result = (|| {
        fs::create_dir(&probe_dir).ok()?;
        fs::write(&member, b"basanos-doom-archiver-probe\n").ok()?;
        let create = Command::new(path)
            .arg("-rcS")
            .arg(&archive)
            .arg(&member)
            .output()
            .ok()?;
        let listing = Command::new(path).arg("-t").arg(&archive).output().ok()?;
        let listed_member = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .any(|line| line.trim() == "member.o");
        (create.status.success() && listing.status.success() && archive.is_file() && listed_member)
            .then_some(PORTABLE_ARCHIVER_PROBE.to_owned())
    })();
    let _ = fs::remove_dir_all(&probe_dir);
    if let Some(identity) = result {
        return identity;
    }
    if required {
        panic!(
            "tool {} --version and the portable archive create/list probe both failed",
            path.display()
        );
    }
    format!("failed: portable archive probe for {}", path.display())
}
