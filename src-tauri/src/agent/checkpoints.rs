//! Checkpoints: before every step (a write, an edit, a delete, a command),
//! the folder's protected files are recorded, so any change can be undone,
//! whoever made it (a file tool, a command, a script).
//!
//! Protected: every file in the folder except the skipped folders
//! (`files::SKIP_DIRS`, which can be rebuilt) and files over the size cap.
//! Contents are stored once each by SHA-256 in `<data>\checkpoints\<folder
//! id>\`, never inside the folder. A file whose size and modified time match
//! the last checkpoint isn't read again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::files;
use crate::db::{Db, FileRow};
use crate::Core;

/// Files a folder may hold for checkpoints to be taken (a folder bigger than
/// this, skipped folders aside, isn't a project).
const MAX_FILES: usize = 50_000;
/// Versions of each file pruning keeps (beside every restorable checkpoint).
const KEEP_VERSIONS: usize = 5;
/// The disk keeps at least this free: past it, old steps collapse.
const FREE_FLOOR: u64 = 10 * 1024 * 1024 * 1024;

/// Where checkpoints live, and the limits from Settings.
pub struct Store<'a> {
    pub db: &'a Db,
    pub root: PathBuf,
    /// Files bigger than this aren't protected.
    pub cap: u64,
    /// The soft budget per folder.
    pub budget: u64,
}

impl<'a> Store<'a> {
    pub fn of(core: &'a Core) -> Self {
        let s = core.prefs.get();
        Store {
            db: &core.db,
            root: core.data_dir.join("checkpoints"),
            cap: s.checkpoint_file_mb.max(1) * 1024 * 1024,
            budget: s.checkpoint_budget_gb.max(1) * 1024 * 1024 * 1024,
        }
    }
}

/// A folder's store id: the start of the SHA-256 of its lowercased path.
pub fn folder_id(folder: &Path) -> String {
    let key = folder.to_string_lossy().to_lowercase();
    hex(&Sha256::digest(key.as_bytes()))[..16].to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn blob_path(store: &Path, hash: &str) -> PathBuf {
    store.join("blobs").join(&hash[..2.min(hash.len())]).join(hash)
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// What a scan found: protected files (path, size, mtime) and the files left
/// out for their size.
pub struct Scan {
    pub files: Vec<(String, u64, i64)>,
    pub too_big: Vec<(String, u64)>,
    pub too_many: bool,
}

pub fn scan(folder: &Path, cap: u64) -> Scan {
    let mut out = Scan { files: Vec::new(), too_big: Vec::new(), too_many: false };
    files::walk(folder, true, &mut |p, is_dir| {
        if is_dir {
            return true;
        }
        let Ok(meta) = std::fs::metadata(p) else { return true };
        let rel = p.strip_prefix(folder).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        if rel.is_empty() {
            return true;
        }
        if meta.len() > cap {
            out.too_big.push((rel, meta.len()));
            return true;
        }
        if out.files.len() >= MAX_FILES {
            out.too_many = true;
            return false;
        }
        out.files.push((rel, meta.len(), mtime_ms(&meta)));
        true
    });
    out
}

fn hash_file(path: &Path) -> std::io::Result<(String, Vec<u8>)> {
    let bytes = std::fs::read(path)?;
    Ok((hex(&Sha256::digest(&bytes)), bytes))
}

/// Records the folder as it is now. Returns the checkpoint's id: a new one,
/// or the last one when nothing changed since.
pub fn take(st: &Store, chat_id: &str, folder: &Path, message_id: &str, call_id: &str, label: &str) -> Result<i64, String> {
    let fid = folder_id(folder);
    let store = st.root.join(&fid);
    let scanned = scan(folder, st.cap);
    if scanned.too_many {
        return Err(format!("The folder holds more than {MAX_FILES} files (outside the skipped folders), too many to checkpoint"));
    }
    let last = st.db.checkpoint_last(&fid)?;
    let known: HashMap<&str, &FileRow> = last.as_ref().map(|(_, rows)| rows.iter().map(|r| (r.path.as_str(), r)).collect()).unwrap_or_default();
    let mut rows: Vec<FileRow> = Vec::with_capacity(scanned.files.len());
    for (rel, size, mtime) in &scanned.files {
        if let Some(k) = known.get(rel.as_str()).filter(|k| k.size == *size && k.mtime == *mtime) {
            rows.push((*k).clone());
            continue;
        }
        let path = folder.join(rel);
        // A file that vanished or is locked mid-scan is left out of this one.
        let Ok((hash, bytes)) = hash_file(&path) else { continue };
        let blob = blob_path(&store, &hash);
        if !blob.exists() {
            if let Some(dir) = blob.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't write the checkpoint store: {e}"))?;
            }
            let tmp = blob.with_extension("part");
            std::fs::write(&tmp, &bytes).map_err(|e| format!("Couldn't write the checkpoint store: {e}"))?;
            std::fs::rename(&tmp, &blob).map_err(|e| format!("Couldn't write the checkpoint store: {e}"))?;
        }
        rows.push(FileRow { path: rel.clone(), hash, size: *size, mtime: *mtime });
    }
    // Nothing changed since the last one: this session's steps share it (a
    // session's first checkpoint is its own: its baseline for "All changes").
    if let Some((id, prev)) = last.as_ref().filter(|(id, _)| st.db.checkpoints_of(chat_id).is_ok_and(|own| own.iter().any(|c| c.id == *id))) {
        if prev.len() == rows.len() {
            let before: HashMap<&str, &str> = prev.iter().map(|r| (r.path.as_str(), r.hash.as_str())).collect();
            if rows.iter().all(|r| before.get(r.path.as_str()) == Some(&r.hash.as_str())) {
                return Ok(*id);
            }
        }
    }
    let id = st.db.checkpoint_insert(chat_id, &fid, message_id, call_id, label, &rows)?;
    let _ = prune(st, &fid, chat_id, id);
    Ok(id)
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    /// Files written back (changed or deleted since).
    pub written: usize,
    /// Files created since, removed.
    pub removed: usize,
    /// The checkpoint taken first: restoring to it undoes this restore.
    pub undo: i64,
}

/// Makes the folder's protected files match a checkpoint: changed files go
/// back, deleted ones return, files created since are removed. Skipped
/// folders and big files are left alone. A checkpoint is taken first, so the
/// restore can be undone.
pub fn restore(st: &Store, chat_id: &str, folder: &Path, checkpoint: i64) -> Result<Restored, String> {
    let info = st.db.checkpoint(checkpoint)?.ok_or("That checkpoint no longer exists")?;
    let fid = folder_id(folder);
    if info.folder != fid {
        return Err("That checkpoint belongs to another folder".into());
    }
    if !info.restorable {
        return Err("That step is no longer restorable: its checkpoint was pruned to save space".into());
    }
    let undo = take(st, chat_id, folder, "", "", "Before a restore")?;
    let target = st.db.checkpoint_files(checkpoint)?;
    let store = st.root.join(&fid);
    let now = scan(folder, st.cap);
    let wanted: HashMap<&str, &FileRow> = target.iter().map(|r| (r.path.as_str(), r)).collect();
    let current = st.db.checkpoint_files(undo)?;
    let current: HashMap<&str, &str> = current.iter().map(|r| (r.path.as_str(), r.hash.as_str())).collect();
    let mut out = Restored { undo, ..Default::default() };
    // Files created since go.
    let mut emptied: HashSet<PathBuf> = HashSet::new();
    for (rel, _, _) in &now.files {
        if !wanted.contains_key(rel.as_str()) {
            let p = folder.join(rel);
            if std::fs::remove_file(&p).is_ok() {
                out.removed += 1;
                if let Some(parent) = p.parent() {
                    emptied.insert(parent.to_path_buf());
                }
            }
        }
    }
    // Changed and deleted files come back.
    for row in &target {
        if current.get(row.path.as_str()) == Some(&row.hash.as_str()) {
            continue;
        }
        let blob = blob_path(&store, &row.hash);
        let bytes = std::fs::read(&blob).map_err(|_| format!("The saved copy of {} is missing from the checkpoint store", row.path))?;
        let dest = folder.join(&row.path);
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
        out.written += 1;
    }
    // Folders left empty by removed files go too (up to the folder itself).
    let needed: HashSet<PathBuf> = target.iter().filter_map(|r| folder.join(&r.path).parent().map(Path::to_path_buf)).collect();
    let mut dirs: Vec<PathBuf> = emptied.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for mut d in dirs {
        while d.starts_with(folder) && d != folder && !needed.iter().any(|n| n.starts_with(&d)) {
            if std::fs::remove_dir(&d).is_err() {
                break;
            }
            match d.parent() {
                Some(p) => d = p.to_path_buf(),
                None => break,
            }
        }
    }
    Ok(out)
}

/// Writes one stored version of a file back (after a checkpoint, so it can be undone).
pub fn restore_file(st: &Store, chat_id: &str, folder: &Path, rel: &str, hash: &str) -> Result<i64, String> {
    let fid = folder_id(folder);
    let versions = st.db.file_versions(&fid, rel)?;
    if !versions.iter().any(|v| v.hash == hash) {
        return Err("That version isn't in the checkpoint store".into());
    }
    let bytes = read_version(st, folder, hash)?;
    let undo = take(st, chat_id, folder, "", "", &format!("Before restoring {rel}"))?;
    let dest = super::paths::normalize(&folder.join(rel));
    if !super::paths::within(&dest, folder) {
        return Err("That path isn't in the folder".into());
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
    Ok(undo)
}

pub fn read_version(st: &Store, folder: &Path, hash: &str) -> Result<Vec<u8>, String> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("That isn't a version Relay knows".into());
    }
    let store = st.root.join(folder_id(folder));
    std::fs::read(blob_path(&store, hash)).map_err(|_| "That version's copy is missing from the checkpoint store".to_string())
}

/// Keeps a folder's store within its budget. In order: any file's latest
/// version and the newest checkpoint are never pruned; past the soft budget,
/// other sessions' checkpoints collapse to per-file history (the last 5
/// versions of each file); this session's steps stay restorable up to a hard
/// ceiling (twice the budget, or 10 GB left free on the disk), past which its
/// oldest steps collapse too. Collapsed steps say so on their cards.
pub fn prune(st: &Store, fid: &str, chat_id: &str, newest: i64) -> Result<(), String> {
    let budget = st.budget;
    let mut size = st.db.checkpoint_store_size(fid)?;
    if size <= budget {
        return Ok(());
    }
    let store = st.root.join(fid);
    // Other sessions first.
    let others: Vec<i64> = st.db.checkpoints_in(fid)?.into_iter().filter(|c| c.restorable && c.chat_id != chat_id && c.id != newest).map(|c| c.id).collect();
    if !others.is_empty() {
        st.db.checkpoints_collapse(&others)?;
        st.db.checkpoints_trim(fid, KEEP_VERSIONS)?;
        sweep(st, fid, &store)?;
        size = st.db.checkpoint_store_size(fid)?;
    }
    let ceiling = (budget * 2).min(size + free_space(&store).saturating_sub(FREE_FLOOR));
    // This session's oldest steps, one at a time, while over the ceiling.
    while size > ceiling {
        let mine: Vec<i64> = st.db.checkpoints_in(fid)?.into_iter().filter(|c| c.restorable && c.id != newest).map(|c| c.id).collect();
        let Some(oldest) = mine.first().copied() else { break };
        st.db.checkpoints_collapse(&[oldest])?;
        st.db.checkpoints_trim(fid, KEEP_VERSIONS)?;
        sweep(st, fid, &store)?;
        size = st.db.checkpoint_store_size(fid)?;
    }
    Ok(())
}

/// Deletes stored contents no checkpoint needs any more.
pub fn sweep(st: &Store, fid: &str, store: &Path) -> Result<(), String> {
    let keep = st.db.checkpoint_hashes(fid)?;
    let Ok(dirs) = std::fs::read_dir(store.join("blobs")) else { return Ok(()) };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
    Ok(())
}

/// Removes a deleted session's checkpoints and what only they needed.
pub fn forget_session(st: &Store, chat_id: &str) -> Result<(), String> {
    let folders = st.db.checkpoint_folders_of(chat_id)?;
    st.db.delete_checkpoints_of(chat_id)?;
    for fid in folders {
        let store = st.root.join(&fid);
        sweep(st, &fid, &store)?;
        if st.db.checkpoint_hashes(&fid)?.is_empty() {
            let _ = std::fs::remove_dir_all(&store);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn free_space(p: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    let mut dir = p.to_path_buf();
    while !dir.exists() {
        match dir.parent() {
            Some(d) => dir = d.to_path_buf(),
            None => return u64::MAX,
        }
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    if ok == 0 {
        u64::MAX
    } else {
        free
    }
}
#[cfg(not(windows))]
fn free_space(_: &Path) -> u64 {
    u64::MAX
}

/// What changed in the folder since the session's first checkpoint, by
/// file: created, changed, deleted. `big` lists files left out for their size.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    pub files: Vec<Change>,
    pub big: Vec<(String, u64)>,
    /// The session took no checkpoint yet (it changed nothing).
    pub none: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub path: String,
    /// "created" | "changed" | "deleted"
    pub kind: String,
    /// How many versions the store holds.
    pub versions: usize,
}

pub fn changes(st: &Store, chat_id: &str, folder: &Path) -> Result<Changes, String> {
    let fid = folder_id(folder);
    let cap = st.cap;
    let first = st.db.checkpoints_of(chat_id)?.into_iter().find(|c| c.folder == fid);
    let now = scan(folder, cap);
    let Some(first) = first else { return Ok(Changes { files: Vec::new(), big: now.too_big, none: true }) };
    let base = st.db.checkpoint_files(first.id)?;
    let base: HashMap<&str, &FileRow> = base.iter().map(|r| (r.path.as_str(), r)).collect();
    // Hash what may have changed (size or time differs from the baseline, or a newer checkpoint).
    let last = st.db.checkpoint_last(&fid)?.map(|(_, rows)| rows).unwrap_or_default();
    let last: HashMap<&str, &FileRow> = last.iter().map(|r| (r.path.as_str(), r)).collect();
    let mut files = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (rel, size, mtime) in &now.files {
        seen.insert(rel.as_str());
        let hash_now = || -> Option<String> {
            if let Some(l) = last.get(rel.as_str()).filter(|l| l.size == *size && l.mtime == *mtime) {
                return Some(l.hash.clone());
            }
            hash_file(&folder.join(rel)).ok().map(|(h, _)| h)
        };
        let kind = match base.get(rel.as_str()) {
            None => Some("created"),
            Some(b) if b.size == *size && b.mtime == *mtime => None,
            Some(b) => (hash_now().as_deref() != Some(b.hash.as_str())).then_some("changed"),
        };
        if let Some(kind) = kind {
            files.push(Change { path: rel.clone(), kind: kind.into(), versions: st.db.file_versions(&fid, rel)?.len() });
        }
    }
    for rel in base.keys() {
        if !seen.contains(rel) {
            files.push(Change { path: rel.to_string(), kind: "deleted".into(), versions: st.db.file_versions(&fid, rel)?.len() });
        }
    }
    files.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
    Ok(Changes { files, big: now.too_big, none: false })
}

/// Every change since the session started, as diffs: each file from its
/// copy in the session's first checkpoint to now. Binary files and files
/// whose copy is gone get a note instead of a diff.
pub fn all_changes(st: &Store, chat_id: &str, folder: &Path) -> Result<Vec<serde_json::Value>, String> {
    let list = changes(st, chat_id, folder)?;
    let fid = folder_id(folder);
    let base: HashMap<String, String> = match st.db.checkpoints_of(chat_id)?.into_iter().find(|c| c.folder == fid) {
        Some(first) => st.db.checkpoint_files(first.id)?.into_iter().map(|r| (r.path, r.hash)).collect(),
        None => HashMap::new(),
    };
    let mut out = Vec::new();
    for c in list.files.iter().take(500) {
        let old = match (c.kind.as_str(), base.get(&c.path)) {
            ("created", _) => Some(String::new()),
            (_, Some(hash)) => read_version(st, folder, hash).ok().map(|b| super::files::decode(&b)).unwrap_or(None),
            _ => None,
        };
        let now = if c.kind == "deleted" { Some(String::new()) } else { std::fs::read(folder.join(&c.path)).ok().and_then(|b| super::files::decode(&b)) };
        let entry = match (old, now) {
            (Some(o), Some(n)) => serde_json::json!({ "path": c.path, "kind": c.kind, "diff": super::diff::lines(&o.replace("\r\n", "\n"), &n.replace("\r\n", "\n")) }),
            _ => serde_json::json!({ "path": c.path, "kind": c.kind, "note": "Not text, or its first version is no longer stored." }),
        };
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("relay-cp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let folder = base.join("project");
        std::fs::create_dir_all(folder.join("src")).unwrap();
        (folder, base.join("store"))
    }

    #[test]
    fn sessions_dont_share_checkpoints() {
        let db = Db::memory();
        let (folder, root) = dirs("share");
        std::fs::write(folder.join("a.txt"), "one").unwrap();
        let st = Store { db: &db, root: root.clone(), cap: 1 << 20, budget: 1 << 30 };
        let a1 = take(&st, "a", &folder, "m", "c1", "x").unwrap();
        assert_eq!(take(&st, "a", &folder, "m", "c2", "x").unwrap(), a1, "nothing changed: the same session shares");
        let b1 = take(&st, "b", &folder, "m", "c3", "x").unwrap();
        assert_ne!(b1, a1, "another session starts with its own");
        std::fs::write(folder.join("new.txt"), "two").unwrap();
        let c = changes(&st, "b", &folder).unwrap();
        assert_eq!(c.files.iter().map(|f| (f.path.as_str(), f.kind.as_str())).collect::<Vec<_>>(), vec![("new.txt", "created")]);
        let all = all_changes(&st, "b", &folder).unwrap();
        assert_eq!(all[0]["diff"]["added"], 1);
        let _ = std::fs::remove_dir_all(folder.parent().unwrap());
    }

    #[test]
    fn take_restore_and_undo() {
        let db = Db::memory();
        let (folder, root) = dirs("restore");
        let st = Store { db: &db, root: root.clone(), cap: 1024 * 1024, budget: 1 << 30 };
        std::fs::write(folder.join("src/a.txt"), "one").unwrap();
        std::fs::write(folder.join("keep.txt"), "keep").unwrap();
        let first = take(&st, "s1", &folder, "m", "c1", "before step 1").unwrap();
        // Nothing changed: the same checkpoint serves the next step.
        assert_eq!(take(&st, "s1", &folder, "m", "c2", "before step 2").unwrap(), first);
        // A script edits, deletes and creates files.
        std::fs::write(folder.join("src/a.txt"), "two").unwrap();
        std::fs::remove_file(folder.join("keep.txt")).unwrap();
        std::fs::create_dir_all(folder.join("new/deep")).unwrap();
        std::fs::write(folder.join("new/deep/b.txt"), "b").unwrap();
        let ch = changes(&st, "s1", &folder).unwrap();
        let kinds: Vec<(&str, &str)> = ch.files.iter().map(|c| (c.path.as_str(), c.kind.as_str())).collect();
        assert_eq!(kinds, vec![("keep.txt", "deleted"), ("new/deep/b.txt", "created"), ("src/a.txt", "changed")]);
        let r = restore(&st, "s1", &folder, first).unwrap();
        assert_eq!((r.written, r.removed), (2, 1));
        assert_eq!(std::fs::read_to_string(folder.join("src/a.txt")).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(folder.join("keep.txt")).unwrap(), "keep");
        assert!(!folder.join("new").exists(), "folders left empty go too");
        // The restore itself can be undone.
        restore(&st, "s1", &folder, r.undo).unwrap();
        assert_eq!(std::fs::read_to_string(folder.join("src/a.txt")).unwrap(), "two");
        assert!(folder.join("new/deep/b.txt").exists());
        // Per-file history and one version back.
        let fid = folder_id(&folder);
        let versions = db.file_versions(&fid, "src/a.txt").unwrap();
        assert_eq!(versions.len(), 2);
        restore_file(&st, "s1", &folder, "src/a.txt", &versions[0].hash).unwrap();
        assert_eq!(std::fs::read_to_string(folder.join("src/a.txt")).unwrap(), "one");
        assert!(restore_file(&st, "s1", &folder, "../x.txt", &versions[0].hash).is_err());
        // Forgetting the session empties its store.
        forget_session(&st, "s1").unwrap();
        assert!(db.checkpoints_of("s1").unwrap().is_empty());
        assert!(!root.join(&fid).exists());
        let _ = std::fs::remove_dir_all(folder.parent().unwrap());
    }

    #[test]
    fn pruning_collapses_other_sessions_first_and_keeps_latest_versions() {
        let db = Db::memory();
        let (folder, root) = dirs("prune");
        // A tiny budget: every step is over it.
        let st = Store { db: &db, root: root.clone(), cap: 1024 * 1024, budget: 0 };
        let mut ids = Vec::new();
        for i in 0..8 {
            std::fs::write(folder.join("f.txt"), format!("version {i} {}", "x".repeat(100))).unwrap();
            let chat = if i < 4 { "old" } else { "now" };
            ids.push(take(&st, chat, &folder, "", "", "step").unwrap());
        }
        let fid = folder_id(&folder);
        let all = db.checkpoints_in(&fid).unwrap();
        // The old session's steps collapsed; the newest checkpoint stays restorable.
        assert!(all.iter().filter(|c| c.chat_id == "old").all(|c| !c.restorable));
        assert!(all.iter().any(|c| c.id == *ids.last().unwrap() && c.restorable));
        // The file's newest versions are kept, older ones pruned.
        let versions = db.file_versions(&fid, "f.txt").unwrap();
        assert!(versions.len() >= KEEP_VERSIONS.min(4) && versions.len() <= 8, "{}", versions.len());
        let latest = &db.checkpoint_files(*ids.last().unwrap()).unwrap()[0].hash;
        assert!(read_version(&st, &folder, latest).is_ok());
        let _ = std::fs::remove_dir_all(folder.parent().unwrap());
    }

    #[test]
    fn scans_skip_rebuildable_folders_and_big_files() {
        let d = std::env::temp_dir().join(format!("relay-cp-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.txt"), "a").unwrap();
        std::fs::write(d.join(".env"), "SECRET=1").unwrap();
        std::fs::write(d.join("node_modules/x/b.js"), "b").unwrap();
        std::fs::write(d.join("big.bin"), vec![0u8; 2048]).unwrap();
        let s = scan(&d, 1024);
        let names: Vec<&str> = s.files.iter().map(|f| f.0.as_str()).collect();
        assert!(names.contains(&"src/a.txt") && names.contains(&".env"), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("node_modules")));
        assert_eq!(s.too_big, vec![("big.bin".to_string(), 2048)]);
        assert_eq!(folder_id(Path::new(r"C:\A\b")), folder_id(Path::new(r"c:\a\B")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
