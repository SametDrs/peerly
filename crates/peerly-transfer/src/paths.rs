//! Receiver-side path safety. Every manifest path is untrusted input.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use peerly_proto::transfer::Manifest;

use crate::TransferError;

/// Suffix of files that are still being written / not yet verified.
pub(crate) const PART_SUFFIX: &str = ".peerly-part";

const MAX_COMPONENT_LEN: usize = 255;

const RESERVED: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "CONIN$",
    "CONOUT$",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
];

/// Validate one path component. Returns a human-readable reason on failure.
fn check_component(c: &str) -> Result<(), &'static str> {
    if c.is_empty() {
        return Err("empty component");
    }
    if c == "." || c == ".." {
        return Err("relative component");
    }
    if c.len() > MAX_COMPONENT_LEN {
        return Err("component too long");
    }
    for ch in c.chars() {
        if ch.is_control() {
            return Err("control character");
        }
        // `:` also covers drive letters (`C:`) and NTFS alternate data streams.
        if matches!(ch, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') {
            return Err("forbidden character");
        }
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Err("trailing dot or space");
    }
    // Windows reserved device names, with or without any extension (`con.txt`, `NUL .tar.gz`).
    let base = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(base)) {
        return Err("reserved name");
    }
    Ok(())
}

/// Validate a `/`-separated relative manifest path and return its components.
pub(crate) fn validate_rel_path(p: &str) -> Result<Vec<&str>, TransferError> {
    let unsafe_path = |why: &str| TransferError::UnsafePath(format!("{p:?}: {why}"));
    if p.is_empty() {
        return Err(unsafe_path("empty path"));
    }
    if p.starts_with('/') {
        return Err(unsafe_path("absolute path"));
    }
    let comps: Vec<&str> = p.split('/').collect();
    for c in &comps {
        check_component(c).map_err(unsafe_path)?;
    }
    Ok(comps)
}

/// Key used for duplicate detection. Case-insensitive file systems (Windows,
/// macOS) would silently merge `A.txt` and `a.txt`, so compare folded there.
fn dup_key(p: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        p.to_lowercase()
    } else {
        p.to_owned()
    }
}

/// Full structural validation of a manifest received from the peer.
pub(crate) fn validate_manifest(m: &Manifest) -> Result<(), TransferError> {
    let proto = |s: String| TransferError::Protocol(s);
    let mut seen = HashSet::new();
    let mut dirs = HashSet::new();
    let mut total: u64 = 0;
    for (i, f) in m.files.iter().enumerate() {
        if f.index as usize != i {
            return Err(proto(format!("file index {} at position {i}", f.index)));
        }
        let comps = validate_rel_path(&f.path)?;
        if !seen.insert(dup_key(&f.path)) {
            return Err(proto(format!("duplicate path {:?}", f.path)));
        }
        for n in 1..comps.len() {
            dirs.insert(dup_key(&comps[..n].join("/")));
        }
        total = total
            .checked_add(f.size)
            .ok_or_else(|| proto("total size overflow".into()))?;
    }
    for d in &m.empty_dirs {
        let comps = validate_rel_path(d)?;
        if !seen.insert(dup_key(d)) {
            return Err(proto(format!("duplicate path {d:?}")));
        }
        for n in 1..comps.len() {
            dirs.insert(dup_key(&comps[..n].join("/")));
        }
    }
    // A file must not also be used as a directory (`a` and `a/b`).
    if let Some(f) = m.files.iter().find(|f| dirs.contains(&dup_key(&f.path))) {
        return Err(proto(format!(
            "path {:?} is both a file and a directory",
            f.path
        )));
    }
    if total != m.total_size {
        return Err(proto(format!(
            "total_size {} != sum of files {total}",
            m.total_size
        )));
    }
    Ok(())
}

/// Where each manifest entry lands on disk.
#[derive(Debug)]
pub(crate) struct Plan {
    /// Final path of every file, by index.
    pub files: Vec<PathBuf>,
    /// Directories to create (parents of files + empty dirs), deduplicated.
    pub dirs: Vec<PathBuf>,
}

pub(crate) fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok() || part_path(p).symlink_metadata().is_ok()
}

/// `photo.jpg` -> `photo (n).jpg`; directories and dotfiles get the suffix at the end.
fn numbered(name: &str, is_file: bool, n: u32) -> String {
    match name.rfind('.') {
        Some(dot) if is_file && dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// Map the manifest onto `dest` (which must exist). Top-level items that already
/// exist are renamed `name (1).ext` / `folder (1)`, so nothing is ever overwritten.
/// Must be called on a validated manifest. Blocking (touches the file system).
pub(crate) fn plan(dest: &Path, m: &Manifest) -> Plan {
    let top = |p: &str| p.split('/').next().unwrap_or(p).to_owned();
    let mut tops: Vec<(String, bool)> = Vec::new();
    let mut known = HashSet::new();
    for (p, is_file) in m
        .files
        .iter()
        .map(|f| (f.path.as_str(), !f.path.contains('/')))
        .chain(m.empty_dirs.iter().map(|d| (d.as_str(), false)))
    {
        let t = top(p);
        if known.insert(t.clone()) {
            tops.push((t, is_file));
        }
    }
    let mut taken: HashSet<String> = tops.iter().map(|(t, _)| dup_key(t)).collect();
    let mut rename: HashMap<String, String> = HashMap::new();
    for (t, is_file) in &tops {
        if !exists(&dest.join(t)) {
            continue;
        }
        let mut n = 1;
        let alt = loop {
            let cand = numbered(t, *is_file, n);
            if !taken.contains(&dup_key(&cand)) && !exists(&dest.join(&cand)) {
                break cand;
            }
            n += 1;
        };
        taken.insert(dup_key(&alt));
        rename.insert(t.clone(), alt);
    }
    let map = |p: &str| -> PathBuf {
        let mut out = dest.to_path_buf();
        for (i, c) in p.split('/').enumerate() {
            match rename.get(c) {
                Some(r) if i == 0 => out.push(r),
                _ => out.push(c),
            }
        }
        out
    };
    let files: Vec<PathBuf> = m.files.iter().map(|f| map(&f.path)).collect();
    let mut dir_set = HashSet::new();
    let mut dirs = Vec::new();
    let parents = files
        .iter()
        .filter_map(|f| f.parent().map(Path::to_path_buf));
    for d in parents.chain(m.empty_dirs.iter().map(|d| map(d))) {
        if dir_set.insert(d.clone()) {
            dirs.push(d);
        }
    }
    Plan { files, dirs }
}

/// Create the planned directories and verify (after resolving any links) that
/// each one is still inside `dest`. Blocking.
pub(crate) fn create_dirs(dest: &Path, plan: &Plan) -> Result<(), TransferError> {
    let root = dest.canonicalize()?;
    for d in &plan.dirs {
        std::fs::create_dir_all(d)?;
        if !d.canonicalize()?.starts_with(&root) {
            return Err(TransferError::UnsafePath(d.display().to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerly_proto::transfer::FileEntry;

    fn manifest(paths: &[&str], dirs: &[&str]) -> Manifest {
        Manifest {
            name: "x".into(),
            files: paths
                .iter()
                .enumerate()
                .map(|(i, p)| FileEntry {
                    index: i as u32,
                    path: (*p).into(),
                    size: 1,
                })
                .collect(),
            empty_dirs: dirs.iter().map(|d| (*d).into()).collect(),
            total_size: paths.len() as u64,
        }
    }

    #[test]
    fn accepts_normal_paths() {
        for p in [
            "a.txt",
            "photos/2024/IMG 001.jpg",
            ".bashrc",
            "über/ñ.txt",
            "a..b",
            "con-tent.txt",
            "COM10",
        ] {
            validate_rel_path(p).unwrap_or_else(|e| panic!("{p}: {e}"));
        }
    }

    #[test]
    fn rejects_malicious_paths() {
        let bad = [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a/./b",
            "./a",
            "a//b",
            "a/",
            "C:/x",
            "C:x",
            "c:\\windows",
            "a\\..\\b",
            "\\\\server\\share",
            "a:stream",
            "nul\0x",
            "a\nb",
            "CON",
            "con.txt",
            "a/Nul.tar.gz",
            "LPT9",
            "com1",
            "AUX .txt",
            "trail.",
            "trail ",
            "dir./x",
            "x/CONIN$",
            "a?b",
            "a*b",
            "a|b",
            "a\"b",
            "a<b",
        ];
        for p in bad {
            assert!(
                matches!(validate_rel_path(p), Err(TransferError::UnsafePath(_))),
                "{p:?} should be rejected"
            );
        }
        let long = "a".repeat(256);
        assert!(validate_rel_path(&long).is_err());
    }

    #[test]
    fn rejects_bad_manifests() {
        let proto = |m: &Manifest| matches!(validate_manifest(m), Err(TransferError::Protocol(_)));
        assert!(validate_manifest(&manifest(&["a/b", "a/c", "d"], &["e", "a/f"])).is_ok());
        assert!(proto(&manifest(&["a", "a"], &[])));
        assert!(proto(&manifest(&["a"], &["a"])));
        assert!(proto(&manifest(&["a", "a/b"], &[])));
        assert!(proto(&manifest(&["a/b", "a"], &[])));
        let mut m = manifest(&["a", "b"], &[]);
        m.files[1].index = 0;
        assert!(proto(&m));
        let mut m = manifest(&["a"], &[]);
        m.total_size = 5;
        assert!(proto(&m));
        let m = manifest(&["ok", "../evil"], &[]);
        assert!(matches!(
            validate_manifest(&m),
            Err(TransferError::UnsafePath(_))
        ));
        let m = manifest(&["ok"], &["/abs"]);
        assert!(matches!(
            validate_manifest(&m),
            Err(TransferError::UnsafePath(_))
        ));
    }

    #[test]
    fn numbering() {
        assert_eq!(numbered("a.jpg", true, 1), "a (1).jpg");
        assert_eq!(numbered("a.tar.gz", true, 2), "a.tar (2).gz");
        assert_eq!(numbered(".bashrc", true, 1), ".bashrc (1)");
        assert_eq!(numbered("photos.v2", false, 1), "photos.v2 (1)");
    }

    #[test]
    fn plan_renames_existing_top_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("photos")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("b.txt.peerly-part"), b"x").unwrap();
        let m = manifest(
            &["photos/x.jpg", "a.txt", "a (1).txt", "b.txt"],
            &["photos/empty"],
        );
        let p = plan(dir.path(), &m);
        let rel: Vec<String> = p
            .files
            .iter()
            .map(|f| {
                f.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            rel,
            ["photos (1)/x.jpg", "a (2).txt", "a (1).txt", "b (1).txt"]
        );
        assert!(
            p.dirs
                .contains(&dir.path().join("photos (1)").join("empty"))
        );
    }
}
