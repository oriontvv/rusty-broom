//! Measuring how much a directory actually occupies.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DirStat {
    /// Bytes that would be freed by deleting the path.
    pub bytes: u64,
    pub files: u64,
    /// Newest modification time inside the path.
    #[serde(skip)]
    pub newest: Option<SystemTime>,
}

impl DirStat {
    fn merge(&mut self, other: DirStat) {
        self.bytes += other.bytes;
        self.files += other.files;
        self.newest = self.newest.max(other.newest);
    }
}

/// Disk usage of a single file. On unix the allocated block count is used, so
/// sparse files and small-file overhead are reported the way `du` reports them.
fn file_size(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Recursively measure `path`, stopping early if `cancel` is set.
///
/// Symlinks are never followed: a link is counted as its own small entry so
/// that linked trees (`bazel-out`, `DerivedData` aliases) are not double
/// counted and never traversed outside the project.
pub fn measure(path: &Path, cancel: &Arc<AtomicBool>) -> DirStat {
    let mut total = DirStat::default();
    let Ok(meta) = fs::symlink_metadata(path) else {
        return total;
    };

    if !meta.is_dir() {
        total.files = 1;
        total.bytes = file_size(&meta);
        total.newest = meta.modified().ok();
        return total;
    }

    // Explicit stack instead of recursion: artifact trees can be very deep.
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if let Ok(meta) = entry
                .metadata()
                .or_else(|_| entry.path().symlink_metadata())
            {
                total.merge(DirStat {
                    bytes: file_size(&meta),
                    files: 1,
                    newest: meta.modified().ok(),
                });
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn measures_a_tree() {
        let base = std::env::temp_dir().join(format!("rb-size-{}", std::process::id()));
        let nested = base.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        let mut f = fs::File::create(nested.join("data.bin")).unwrap();
        f.write_all(&[7u8; 4096]).unwrap();
        drop(f);

        let stat = measure(&base, &Arc::new(AtomicBool::new(false)));
        assert_eq!(stat.files, 1);
        assert!(stat.bytes >= 4096, "got {} bytes", stat.bytes);
        assert!(stat.newest.is_some());

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_path_is_zero() {
        let stat = measure(
            Path::new("/definitely/not/here"),
            &Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(stat, DirStat::default());
    }
}
