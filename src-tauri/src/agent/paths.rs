//! Where a path is, for the permission rules: in the session's folder, in a
//! scratch area tools write to on their own, in a credential location, or
//! anywhere else (protected: the user's data).
//!
//! Paths are compared after expanding `~` and environment variables, removing
//! `.` and `..`, and resolving what exists (junctions, symlinks, short names)
//! through the file system; case doesn't matter.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Folder,
    Scratch,
    Credential,
    Protected,
}

/// Expands `~`, `%VAR%`, `$env:VAR`, `${env:VAR}`, `$HOME` and `$VAR` at the
/// start of a path segment.
pub fn expand(raw: &str) -> String {
    let raw = raw.trim().trim_matches('"').trim_matches('\'');
    let mut s = raw.to_string();
    let home = home().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    if s == "~" {
        return home;
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        s = format!("{home}\\{rest}");
    }
    // Git Bash: /c/Users is C:\Users, /tmp the temp folder.
    if let Some(rest) = s.strip_prefix('/') {
        let b = rest.as_bytes();
        if !b.is_empty() && b[0].is_ascii_alphabetic() && (b.len() == 1 || b[1] == b'/') {
            s = format!("{}:\\{}", (b[0] as char).to_ascii_uppercase(), rest.get(2..).unwrap_or(""));
        } else if rest == "tmp" || rest.starts_with("tmp/") {
            if let Some(t) = std::env::var_os("TEMP") {
                s = format!("{}{}", t.to_string_lossy(), &rest[3..]);
            }
        }
    }
    // %VAR%
    let re = regex::Regex::new(r"%([A-Za-z_][A-Za-z0-9_()]*)%").unwrap();
    s = re.replace_all(&s, |c: &regex::Captures| std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())).into_owned();
    // $env:VAR, ${env:VAR}, $HOME, $VAR
    let re = regex::Regex::new(r"\$\{env:([A-Za-z_][A-Za-z0-9_]*)\}|\$env:([A-Za-z_][A-Za-z0-9_]*)|\$([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    s = re
        .replace_all(&s, |c: &regex::Captures| {
            let name = c.get(1).or(c.get(2)).or(c.get(3)).map(|m| m.as_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("HOME") {
                return home.clone();
            }
            std::env::var(name).unwrap_or_else(|_| c[0].to_string())
        })
        .into_owned();
    s
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// `raw` as a full path: relative paths are inside `folder`.
pub fn resolve(folder: &Path, raw: &str) -> PathBuf {
    let expanded = expand(raw);
    let p = PathBuf::from(expanded.replace('/', "\\"));
    let joined = if p.is_absolute() {
        p
    } else if p.has_root() {
        // "\x" on the folder's drive.
        let mut drive = folder.components().take(1).collect::<PathBuf>();
        drive.push(p);
        drive
    } else {
        folder.join(p)
    };
    real(&normalize(&joined))
}

/// Removes `.` and `..` without touching the disk.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never above the root.
                if out.parent().is_some() && !matches!(out.components().next_back(), Some(Component::RootDir | Component::Prefix(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The path as the file system sees it: the longest part that exists is
/// resolved (junctions, symlinks, short names), the rest appended.
pub fn real(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&existing) {
            let mut out = plain(&c);
            for t in tail.iter().rev() {
                out.push(t);
            }
            return out;
        }
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                existing = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Drops the `\\?\` that `canonicalize` adds.
pub fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p.to_path_buf()
}

fn key(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('/', "\\").to_lowercase();
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

/// `p` is `root` or inside it (both full, real paths).
pub fn within(p: &Path, root: &Path) -> bool {
    let (p, r) = (key(p), key(root));
    p == r || (p.starts_with(&r) && (r.ends_with('\\') || p[r.len()..].starts_with('\\')))
}

/// `p` relative to `root` when inside it, else the full path.
pub fn shown(p: &Path, root: &Path) -> String {
    if within(p, root) {
        let (pk, rk) = (p.to_string_lossy().into_owned(), root.to_string_lossy().len());
        let rest = pk.get(rk..).unwrap_or("").trim_start_matches(['\\', '/']);
        if rest.is_empty() {
            return ".".into();
        }
        return rest.replace('\\', "/");
    }
    p.to_string_lossy().into_owned()
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

/// Places that hold keys, tokens and passwords. Never read in Auto or Bypass.
pub fn credential_roots(data_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = home() {
        for p in [".ssh", ".aws", ".azure", ".kube", ".docker\\config.json", ".gnupg", ".netrc", "_netrc", ".git-credentials", ".config\\gh"] {
            out.push(h.join(p));
        }
    }
    if let Some(a) = env_path("APPDATA") {
        for p in [
            "GitHub CLI",
            "Microsoft\\Protect",
            "Microsoft\\Credentials",
            "Microsoft\\Crypto",
            "Mozilla\\Firefox\\Profiles",
            "Bitwarden",
            "KeePassXC",
            "app.relay.desktop",
            "app.relay.dev",
        ] {
            out.push(a.join(p));
        }
    }
    if let Some(l) = env_path("LOCALAPPDATA") {
        for p in [
            "Google\\Chrome\\User Data",
            "Microsoft\\Edge\\User Data",
            "BraveSoftware\\Brave-Browser\\User Data",
            "Vivaldi\\User Data",
            "1Password",
            "Bitwarden",
            "Microsoft\\Credentials",
            "Microsoft\\Vault",
        ] {
            out.push(l.join(p));
        }
    }
    out.extend(data_dirs.iter().cloned());
    out.into_iter().map(|p| real(&p)).collect()
}

/// Places tools write to on their own: temp, caches.
pub fn scratch_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(t) = env_path("TEMP").or_else(|| env_path("TMP")) {
        out.push(t);
    }
    if let Some(h) = home() {
        for p in [".cache", ".cargo\\registry", ".cargo\\git", ".npm", ".pnpm-store", ".yarn", ".nuget\\packages", ".gradle\\caches"] {
            out.push(h.join(p));
        }
    }
    if let Some(l) = env_path("LOCALAPPDATA") {
        for p in ["Temp", "pip\\cache", "npm-cache", "pnpm", "pnpm-cache", "Yarn", "uv", "pypoetry\\Cache", "NuGet\\v3-cache", "node-gyp"] {
            out.push(l.join(p));
        }
    }
    out.into_iter().map(|p| real(&p)).collect()
}

/// Which zone a full, real path is in. Credential locations win over
/// everything (Relay's own data folder holds the checkpoint store).
pub fn zone(p: &Path, folder: &Path, credentials: &[PathBuf], scratch: &[PathBuf]) -> Zone {
    if credentials.iter().any(|c| within(p, c)) {
        return Zone::Credential;
    }
    if within(p, folder) {
        return Zone::Folder;
    }
    if scratch.iter().any(|s| within(p, s)) {
        return Zone::Scratch;
    }
    Zone::Protected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_inside_and_out() {
        let folder = PathBuf::from(r"C:\work\site");
        assert_eq!(normalize(Path::new(r"C:\work\site\a\..\b\.\c.txt")), PathBuf::from(r"C:\work\site\b\c.txt"));
        assert_eq!(normalize(Path::new(r"C:\..\..\x")), PathBuf::from(r"C:\x"));
        let r = resolve(&folder, "src/../../other/notes.txt");
        assert!(r.to_string_lossy().to_lowercase().ends_with(r"work\other\notes.txt"), "{r:?}");
        assert!(!within(&r, &folder));
        assert!(within(&resolve(&folder, "a/b.txt"), &folder));
        assert!(within(Path::new(r"C:\WORK\Site\x"), &folder));
        assert!(!within(Path::new(r"C:\work\site-old\x"), &folder));
        assert_eq!(shown(Path::new(r"C:\work\site\a\b.txt"), &folder), "a/b.txt");
        assert_eq!(shown(&folder, &folder), ".");
    }

    #[test]
    fn home_and_variables_expand() {
        let home = home().unwrap();
        let ssh = resolve(Path::new(r"C:\work"), "~/.ssh/id_rsa");
        assert!(within(&ssh, &real(&home.join(".ssh"))), "{ssh:?}");
        assert_eq!(expand("%USERPROFILE%\\x"), format!("{}\\x", home.display()));
        assert_eq!(expand("$env:USERPROFILE\\x"), format!("{}\\x", home.display()));
        assert_eq!(expand("$HOME/x"), format!("{}/x", home.display()));
        assert_eq!(expand("%NO_SUCH_VAR_RELAY%\\x"), "%NO_SUCH_VAR_RELAY%\\x");
    }

    #[test]
    fn zones_put_credentials_first() {
        let folder = real(&std::env::temp_dir().join("relay-zone-test"));
        let creds = credential_roots(&[]);
        let scratch = scratch_roots();
        let home = home().unwrap();
        assert_eq!(zone(&resolve(&folder, "~/.ssh/id_rsa"), &folder, &creds, &scratch), Zone::Credential);
        assert_eq!(zone(&resolve(&folder, "a.txt"), &folder, &creds, &scratch), Zone::Folder);
        assert_eq!(zone(&real(&home.join("Documents\\x.txt")), &folder, &creds, &scratch), Zone::Protected);
        assert_eq!(zone(&real(&std::env::temp_dir().join("other\\y")), &folder, &creds, &scratch), Zone::Scratch);
    }
}
