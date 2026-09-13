//! Turning the user's selection of files and folders into a flat list of
//! `(name, path)` pairs, where `name` is the path the receiver will write to.
//!
//! Rules:
//! - A file `a/b/img.jpg` becomes `img.jpg`.
//! - A directory `a/scans` becomes `scans/...` with its tree preserved.
//! - Several arguments may be given; their names must not collide.
//! - Symlinks are skipped.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use walkdir::WalkDir;

/// A file selected for sharing.
#[derive(Debug, Clone)]
pub struct Source {
    /// Name inside the share (relative, `/`-separated).
    pub name: String,
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Size in bytes at selection time.
    pub size: u64,
}

/// Expand the given paths into the files they contain.
pub fn collect(inputs: &[PathBuf]) -> Result<Vec<Source>> {
    ensure!(!inputs.is_empty(), "nothing to share: no paths given");
    let mut by_name: BTreeMap<String, Source> = BTreeMap::new();
    for input in inputs {
        let path = input
            .canonicalize()
            .with_context(|| format!("{} does not exist", input.display()))?;
        let root = path
            .parent()
            .with_context(|| format!("{} has no parent directory", path.display()))?;
        for entry in WalkDir::new(&path).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let meta = entry.metadata()?;
            let full = entry.into_path();
            let relative = full.strip_prefix(root)?;
            let name = relative_name(relative)?;
            if let Some(prev) = by_name.get(&name) {
                bail!(
                    "two selected paths would both be named `{name}` in the share: {} and {}",
                    prev.path.display(),
                    full.display()
                );
            }
            by_name.insert(
                name.clone(),
                Source {
                    name,
                    path: full,
                    size: meta.len(),
                },
            );
        }
    }
    ensure!(
        !by_name.is_empty(),
        "nothing to share: no regular files found"
    );
    Ok(by_name.into_values().collect())
}

/// Convert a relative path into a `/`-separated share name, rejecting
/// anything that is not a plain sequence of normal components.
fn relative_name(relative: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .with_context(|| format!("non-UTF-8 file name in {}", relative.display()))?;
                ensure!(
                    !part.contains('/') && !part.contains('\\'),
                    "invalid file name component {part:?}"
                );
                parts.push(part);
            }
            other => bail!(
                "unexpected path component {other:?} in {}",
                relative.display()
            ),
        }
    }
    ensure!(!parts.is_empty(), "empty relative path");
    Ok(parts.join("/"))
}

/// Resolve a share name to a destination path under `root`, refusing names
/// that could escape it.
pub fn destination(root: &Path, name: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for part in name.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "refusing unsafe file name {name:?} in share"
        );
        ensure!(
            !part.contains('\\') && !part.contains('\0'),
            "refusing unsafe file name {name:?} in share"
        );
        path.push(part);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_rejects_traversal() {
        let root = Path::new("/tmp/out");
        assert!(destination(root, "../etc/passwd").is_err());
        assert!(destination(root, "a//b").is_err());
        assert!(destination(root, "./a").is_err());
        assert_eq!(
            destination(root, "scans/roll/0001.tiff").unwrap(),
            PathBuf::from("/tmp/out/scans/roll/0001.tiff")
        );
    }

    #[test]
    fn collect_names_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("scans/roll")).unwrap();
        std::fs::write(d.join("scans/roll/a.tiff"), b"aaa").unwrap();
        std::fs::write(d.join("scans/b.jpg"), b"bb").unwrap();
        std::fs::write(d.join("single.jpg"), b"c").unwrap();

        let sources = collect(&[d.join("scans"), d.join("single.jpg")]).unwrap();
        let names: Vec<_> = sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["scans/b.jpg", "scans/roll/a.tiff", "single.jpg"]
        );
        let total: u64 = sources.iter().map(|s| s.size).sum();
        assert_eq!(total, 6);
    }

    #[test]
    fn collect_rejects_name_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("x")).unwrap();
        std::fs::create_dir_all(d.join("y")).unwrap();
        std::fs::write(d.join("x/same.jpg"), b"1").unwrap();
        std::fs::write(d.join("y/same.jpg"), b"2").unwrap();
        assert!(collect(&[d.join("x/same.jpg"), d.join("y/same.jpg")]).is_err());
    }
}
