//! Sender-side manifest construction.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use peerly_proto::transfer::{FileEntry, Manifest};

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

use crate::paths::{self, MAX_EMPTY_DIRS, MAX_FILES, MAX_PATH_LEN};

fn utf8_name(p: &Path) -> io::Result<String> {
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("unsupported file name: {}", p.display())))?;
    // Fail up front with the reason: the receiver would refuse these names.
    paths::validate_rel_path(&name)
        .map_err(|e| invalid(format!("cannot send {}: {e}", p.display())))?;
    Ok(name)
}

struct Builder {
    files: Vec<FileEntry>,
    sources: Vec<PathBuf>,
    empty_dirs: Vec<String>,
    total: u64,
}

impl Builder {
    fn add_file(&mut self, src: PathBuf, rel: String, size: u64) -> io::Result<()> {
        if self.files.len() >= MAX_FILES {
            return Err(invalid(format!("too many files (max {MAX_FILES})")));
        }
        if rel.len() > MAX_PATH_LEN {
            return Err(invalid(format!(
                "path too long (max {MAX_PATH_LEN} bytes): {rel}"
            )));
        }
        self.files.push(FileEntry {
            index: self.files.len() as u32,
            path: rel,
            size,
        });
        self.sources.push(src);
        self.total += size;
        Ok(())
    }

    /// Recursively add a directory's contents. Symlinks (and Windows junctions,
    /// which std reports as symlinks) are skipped, never followed: following them
    /// could leak files outside the chosen folder or loop forever. Other special
    /// files (sockets, devices, ...) are skipped too.
    fn add_dir(&mut self, dir: &Path, rel: &str) -> io::Result<()> {
        let mut entries = std::fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        let mut added = false;
        for e in entries {
            let ft = e.file_type()?; // does not follow symlinks
            if ft.is_symlink() {
                continue;
            }
            let path = e.path();
            let child = format!("{rel}/{}", utf8_name(&path)?);
            if ft.is_dir() {
                self.add_dir(&path, &child)?;
                added = true;
            } else if ft.is_file() {
                let size = e.metadata()?.len();
                self.add_file(path, child, size)?;
                added = true;
            }
        }
        if !added {
            if self.empty_dirs.len() >= MAX_EMPTY_DIRS {
                return Err(invalid(format!(
                    "too many empty folders (max {MAX_EMPTY_DIRS})"
                )));
            }
            if rel.len() > MAX_PATH_LEN {
                return Err(invalid(format!(
                    "path too long (max {MAX_PATH_LEN} bytes): {rel}"
                )));
            }
            self.empty_dirs.push(rel.to_owned());
        }
        Ok(())
    }
}

/// Build the manifest for `paths` (files and/or folders). Returns the manifest
/// and the source path of every file, by index. Blocking.
pub(crate) fn build(paths: &[PathBuf]) -> io::Result<(Manifest, Vec<PathBuf>)> {
    if paths.is_empty() {
        return Err(invalid("nothing to send".into()));
    }
    let mut b = Builder {
        files: Vec::new(),
        sources: Vec::new(),
        empty_dirs: Vec::new(),
        total: 0,
    };
    let mut tops = HashSet::new();
    let mut items = 0usize;
    let mut single_name = String::new();
    for p in paths {
        let meta = std::fs::symlink_metadata(p)?;
        if meta.file_type().is_symlink() {
            continue; // see `add_dir`: symlinks are never followed
        }
        let name = utf8_name(p)?;
        if !tops.insert(name.to_lowercase()) {
            return Err(invalid(format!("two items are named {name:?}")));
        }
        if meta.is_dir() {
            b.add_dir(p, &name)?;
        } else if meta.is_file() {
            b.add_file(p.clone(), name.clone(), meta.len())?;
        } else {
            continue;
        }
        items += 1;
        single_name = name;
    }
    if items == 0 {
        return Err(invalid(
            "nothing to send (only symlinks or special files)".into(),
        ));
    }
    let name = if items == 1 {
        single_name
    } else {
        format!("{items} items")
    };
    let manifest = Manifest {
        name,
        files: b.files,
        empty_dirs: b.empty_dirs,
        total_size: b.total,
    };
    Ok((manifest, b.sources))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_bidi_names() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("video\u{202E}4pm.exe");
        if std::fs::write(&f, b"x").is_err() {
            return; // file system cannot store the name
        }
        let err = build(std::slice::from_ref(&f)).unwrap_err();
        assert!(err.to_string().contains("formatting"), "{err}");
        let root = d.path().join("dir");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a\u{200B}b"), b"x").unwrap();
        assert!(build(&[root]).is_err());
    }

    #[test]
    fn builds_nested() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("photos");
        std::fs::create_dir_all(root.join("sub/empty")).unwrap();
        std::fs::write(root.join("a.jpg"), b"abc").unwrap();
        std::fs::write(root.join("sub/b.jpg"), b"de").unwrap();
        let single = d.path().join("x.txt");
        std::fs::write(&single, b"").unwrap();

        let (m, src) = build(std::slice::from_ref(&root)).unwrap();
        assert_eq!(m.name, "photos");
        let paths: Vec<_> = m.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["photos/a.jpg", "photos/sub/b.jpg"]);
        assert_eq!(m.empty_dirs, ["photos/sub/empty"]);
        assert_eq!(m.total_size, 5);
        assert_eq!(src[1], root.join("sub").join("b.jpg"));

        let (m, _) = build(&[root, single]).unwrap();
        assert_eq!(m.name, "2 items");
        assert_eq!(m.files.last().unwrap().path, "x.txt");
    }
}
