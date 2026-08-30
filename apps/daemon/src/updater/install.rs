//! Checksum verification and atomic binary replacement for installed updates.
//!
//! `atomic_replace` follows the same trick `rustup`/`cargo install` use for
//! self-replacing a running executable: write the new content to a sibling temp
//! file, then `rename()` it over the target. On Unix, `rename()` onto an existing
//! path is atomic and doesn't disturb a process that already has the old inode
//! open (it keeps running against the old file until it exits) — so this is safe
//! to do even while `reins`/`reinsd` are themselves running.

use sha2::{Digest, Sha256};
use std::path::Path;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn verify_checksum(bytes: &[u8], expected_hex: &str) -> bool {
    sha256_hex(bytes).eq_ignore_ascii_case(expected_hex.trim())
}

/// Backs up `target` to `target.bak` (overwriting any previous backup), writes
/// `new_content` to `target.new`, then renames `target.new` onto `target`.
///
/// If `target` doesn't exist yet, no backup is made (nothing to roll back to).
pub fn atomic_replace(target: &Path, new_content: &[u8]) -> std::io::Result<()> {
    if target.exists() {
        std::fs::copy(target, backup_path(target))?;
    }
    let staged = staged_path(target);
    std::fs::write(&staged, new_content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staged, target)?;
    Ok(())
}

/// Restores `target.bak` over `target`. A no-op (not an error) if there is no
/// backup — callers use this defensively after a partial multi-file update, and
/// not every target necessarily got as far as being replaced.
pub fn rollback(target: &Path) -> std::io::Result<()> {
    let backup = backup_path(target);
    if backup.exists() {
        std::fs::rename(&backup, target)?;
    }
    Ok(())
}

fn backup_path(target: &Path) -> std::path::PathBuf {
    let mut p = target.as_os_str().to_owned();
    p.push(".bak");
    p.into()
}

fn staged_path(target: &Path) -> std::path::PathBuf {
    let mut p = target.as_os_str().to_owned();
    p.push(".new");
    p.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "reins-updater-test-{}-{}",
            std::process::id(),
            name
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // sha256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_checksum_accepts_matching_hash_case_insensitively() {
        let bytes = b"hello reins";
        let hex = sha256_hex(bytes);
        assert!(verify_checksum(bytes, &hex));
        assert!(verify_checksum(bytes, &hex.to_uppercase()));
        assert!(!verify_checksum(bytes, "deadbeef"));
    }

    #[test]
    fn atomic_replace_swaps_content_and_backs_up_the_original() {
        let target = temp_file("target-swap", b"old content");
        atomic_replace(&target, b"new content").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new content");
        assert_eq!(std::fs::read(backup_path(&target)).unwrap(), b"old content");

        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(backup_path(&target));
    }

    #[test]
    fn rollback_restores_the_backup() {
        let target = temp_file("target-rollback", b"old content");
        atomic_replace(&target, b"new content").unwrap();
        rollback(&target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"old content");
        assert!(!backup_path(&target).exists());

        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn rollback_without_a_backup_is_a_harmless_no_op() {
        let target = std::env::temp_dir().join(format!(
            "reins-updater-test-{}-no-backup-exists",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(backup_path(&target));
        assert!(rollback(&target).is_ok());
    }
}
