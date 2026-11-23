//! Agent sessions: chats with `kind = 'agent'` that work in one folder, picked
//! by the user or a workspace Relay makes (`<data>\workspaces\<session id>`).
//! See docs/design/agent-mode.md.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::db::ChatSummary;
use crate::Core;

pub fn workspaces_root(core: &Core) -> PathBuf {
    core.data_dir.join("workspaces")
}

/// A folder picked for a new session must be there.
pub fn check_folder(folder: &str) -> Result<(), String> {
    let path = Path::new(folder);
    if !path.is_absolute() {
        return Err("Pick a folder with a full path".into());
    }
    if !path.is_dir() {
        return Err(format!("{folder} isn't a folder that exists"));
    }
    Ok(())
}

/// Before a session's message goes out: its folder is there. Without a picked
/// folder Relay makes the session's workspace (on the first message, and
/// again if it was deleted since).
pub fn ensure_folder(core: &Core, chat: &ChatSummary) -> Result<(), String> {
    match &chat.folder {
        Some(folder) if !chat.folder_owned => {
            if Path::new(folder).is_dir() {
                Ok(())
            } else {
                Err(format!("This session's folder, {folder}, no longer exists"))
            }
        }
        _ => {
            let path = workspaces_root(core).join(&chat.id);
            std::fs::create_dir_all(&path).map_err(|e| format!("Couldn't make the session's workspace: {e}"))?;
            if chat.folder.is_none() {
                core.db.set_folder(&chat.id, &path.to_string_lossy(), true)?;
            }
            Ok(())
        }
    }
}

/// Deletes a deleted session's workspace: only one Relay made, and only
/// where Relay makes them.
pub fn remove_workspace(core: &Core, chat: &ChatSummary) -> Result<(), String> {
    let Some(folder) = chat.folder.as_deref().filter(|_| chat.folder_owned) else { return Ok(()) };
    let path = Path::new(folder);
    if !path.exists() {
        return Ok(());
    }
    let (Ok(path), Ok(root)) = (path.canonicalize(), workspaces_root(core).canonicalize()) else { return Ok(()) };
    if path.parent() != Some(root.as_path()) || path.file_name().and_then(|n| n.to_str()) != Some(chat.id.as_str()) {
        return Err("That folder isn't a workspace Relay made, so it stays".into());
    }
    std::fs::remove_dir_all(&path).map_err(|e| format!("The session is gone, but its workspace couldn't be deleted: {e}"))
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub name: String,
    pub exists: bool,
    /// The checked-out branch (or a short commit when detached), in a git repository.
    pub branch: Option<String>,
}

pub fn folder_info(path: &Path) -> FolderInfo {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let exists = path.is_dir();
    FolderInfo { name, exists, branch: if exists { git_branch(path) } else { None } }
}

/// Reads `.git/HEAD` in the folder or the nearest one above it (a `.git`
/// file, as in worktrees and submodules, points to the real directory).
pub fn git_branch(folder: &Path) -> Option<String> {
    let mut dir = Some(folder);
    while let Some(d) = dir {
        let dot = d.join(".git");
        let git_dir = if dot.is_dir() {
            Some(dot)
        } else if dot.is_file() {
            let text = read_small(&dot)?;
            let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim().to_string();
            let target = PathBuf::from(target);
            Some(if target.is_absolute() { target } else { d.join(target) })
        } else {
            None
        };
        if let Some(git_dir) = git_dir {
            return head_name(&read_small(&git_dir.join("HEAD"))?);
        }
        dir = d.parent();
    }
    None
}

fn head_name(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref:") {
        let r = r.trim();
        return Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string());
    }
    // Detached: a commit.
    (head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head[..7].to_string())
}

/// A small text file (HEAD, a `.git` file): at most 4 KB.
fn read_small(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(4096).read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_come_from_head() {
        assert_eq!(head_name("ref: refs/heads/main\n"), Some("main".into()));
        assert_eq!(head_name("ref: refs/heads/feature/x"), Some("feature/x".into()));
        assert_eq!(head_name("0123456789abcdef0123456789abcdef01234567\n"), Some("0123456".into()));
        assert_eq!(head_name("garbage"), None);
    }

    #[test]
    fn branch_is_found_above_and_through_git_files() {
        let tmp = std::env::temp_dir().join(format!("relay-agent-test-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/dev\n").unwrap();
        std::fs::create_dir_all(repo.join("src").join("deep")).unwrap();
        assert_eq!(git_branch(&repo.join("src").join("deep")), Some("dev".into()));

        // A worktree: `.git` is a file pointing elsewhere.
        let real = tmp.join("real-git");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
        let wt = tmp.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
        assert_eq!(git_branch(&wt), Some("wt".into()));

        let info = folder_info(&repo);
        assert_eq!(info, FolderInfo { name: "repo".into(), exists: true, branch: Some("dev".into()) });
        assert!(!folder_info(&tmp.join("missing")).exists);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
