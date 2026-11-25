//! Which git worktree a directory belongs to, and what to call it.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// Branches that say nothing about the work: the row shows the folder name.
const DEFAULT_BRANCHES: [&str; 4] = ["main", "master", "develop", "trunk"];

/// How long a worktree's display name (its branch) is trusted.
const NAME_TTL: Duration = Duration::from_secs(60);

/// Directories that are never treated as a project, even if they hold a .git
/// (Homebrew's prefix is a git repo).
const SKIP_PREFIXES: [&str; 9] = [
    "/System", "/usr", "/bin", "/sbin", "/Library", "/private", "/opt/homebrew", "/Applications", "/dev",
];

/// Resolves directories to worktree roots, caching every directory it visits.
pub struct RepoIndex {
    home: String,
    /// dir → worktree root (None: not in a worktree).
    roots: HashMap<String, Option<String>>,
    names: HashMap<String, (String, Instant)>,
    fs: Box<dyn RepoFs>,
}

/// File-system access, swappable in tests.
pub trait RepoFs: Send {
    /// Whether `<dir>/.git` exists (a directory, or a file for linked worktrees).
    fn has_git(&self, dir: &str) -> bool;
    /// Contents of the worktree's HEAD file.
    fn read_head(&self, root: &str) -> Option<String>;
}

pub struct RealFs;

impl RepoFs for RealFs {
    fn has_git(&self, dir: &str) -> bool {
        Path::new(dir).join(".git").symlink_metadata().is_ok()
    }

    fn read_head(&self, root: &str) -> Option<String> {
        let dot_git = Path::new(root).join(".git");
        let head = if dot_git.is_dir() {
            dot_git.join("HEAD")
        } else {
            // Linked worktree: ".git" is a file "gitdir: <path>".
            let s = std::fs::read_to_string(&dot_git).ok()?;
            let gitdir = s.strip_prefix("gitdir:")?.trim();
            let p = Path::new(gitdir);
            let p = if p.is_absolute() { p.to_path_buf() } else { Path::new(root).join(p) };
            p.join("HEAD")
        };
        std::fs::read_to_string(head).ok()
    }
}

impl RepoIndex {
    pub fn new(home: String) -> Self {
        Self::with_fs(home, Box::new(RealFs))
    }

    pub fn with_fs(home: String, fs: Box<dyn RepoFs>) -> Self {
        RepoIndex { home: home.trim_end_matches('/').to_string(), roots: HashMap::new(), names: HashMap::new(), fs }
    }

    fn skipped(&self, dir: &str) -> bool {
        dir == "/"
            || dir == self.home
            || dir.starts_with(&format!("{}/Library/", self.home))
            || dir == format!("{}/Library", self.home)
            || SKIP_PREFIXES.iter().any(|p| dir == *p || dir.starts_with(&format!("{p}/")))
    }

    /// The worktree root containing `dir`, if any.
    pub fn root_of(&mut self, dir: &str) -> Option<String> {
        let dir = dir.trim_end_matches('/');
        if dir.is_empty() {
            return None;
        }
        if let Some(r) = self.roots.get(dir) {
            return r.clone();
        }
        // Walk up until a cached answer, a .git, or a skipped directory.
        let mut visited = Vec::new();
        let mut cur = dir.to_string();
        let found = loop {
            if let Some(r) = self.roots.get(&cur) {
                break r.clone();
            }
            if self.skipped(&cur) {
                visited.push(cur.clone());
                break None;
            }
            if self.fs.has_git(&cur) {
                break Some(cur.clone());
            }
            visited.push(cur.clone());
            match cur.rfind('/') {
                Some(0) | None => break None,
                Some(i) => cur.truncate(i),
            }
        };
        for v in visited {
            self.roots.insert(v, found.clone());
        }
        if let Some(root) = &found {
            self.roots.insert(root.clone(), found.clone());
        }
        found
    }

    /// Row name of a worktree: its branch, or the folder name on a default
    /// branch or a detached HEAD.
    pub fn name_of(&mut self, root: &str) -> String {
        if let Some((n, at)) = self.names.get(root)
            && at.elapsed() < NAME_TTL
        {
            return n.clone();
        }
        let name = self
            .fs
            .read_head(root)
            .and_then(|h| branch_from_head(&h))
            .filter(|b| !DEFAULT_BRANCHES.contains(&b.as_str()))
            .unwrap_or_else(|| basename(root).to_string());
        self.names.insert(root.to_string(), (name.clone(), Instant::now()));
        name
    }

    /// Forget cached roots and names (the engine does it every hour, so a
    /// deleted or newly created repo is picked up and the maps stay small).
    pub fn clear(&mut self) {
        self.roots.clear();
        self.names.clear();
    }
}

pub fn branch_from_head(head: &str) -> Option<String> {
    head.trim().strip_prefix("ref: refs/heads/").map(str::to_string)
}

pub fn basename(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    p.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(p)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Fake file system: repos and their HEAD, plus a counter of `has_git`
    /// calls. Clones share both, so a test can add a repo after handing one
    /// to a `RepoIndex`.
    #[derive(Clone, Default)]
    pub struct FakeFs {
        pub heads: Arc<Mutex<HashMap<String, String>>>,
        pub calls: Arc<Mutex<u32>>,
    }

    impl FakeFs {
        pub fn repo(self, root: &str, head: &str) -> Self {
            self.heads.lock().unwrap().insert(root.to_string(), head.to_string());
            self
        }
    }

    impl RepoFs for FakeFs {
        fn has_git(&self, dir: &str) -> bool {
            *self.calls.lock().unwrap() += 1;
            self.heads.lock().unwrap().contains_key(dir)
        }
        fn read_head(&self, root: &str) -> Option<String> {
            self.heads.lock().unwrap().get(root).cloned()
        }
    }

    fn index(fs: FakeFs) -> RepoIndex {
        RepoIndex::with_fs("/Users/me".into(), Box::new(fs))
    }

    #[test]
    fn finds_the_root_and_caches_every_visited_dir() {
        let fs = FakeFs::default().repo("/Users/me/git/app", "ref: refs/heads/feat/login\n");
        let calls = fs.calls.clone();
        let mut ix = index(fs);
        assert_eq!(ix.root_of("/Users/me/git/app/web/src").as_deref(), Some("/Users/me/git/app"));
        let n = *calls.lock().unwrap();
        assert_eq!(ix.root_of("/Users/me/git/app/web").as_deref(), Some("/Users/me/git/app"));
        assert_eq!(ix.root_of("/Users/me/git/app/web/src/").as_deref(), Some("/Users/me/git/app"));
        assert_eq!(*calls.lock().unwrap(), n, "answered from cache");
        assert_eq!(ix.name_of("/Users/me/git/app"), "feat/login");
    }

    #[test]
    fn clear_picks_up_a_new_repo_and_a_new_branch() {
        let fs = FakeFs::default().repo("/w/old", "ref: refs/heads/one");
        let mut ix = index(fs.clone());
        assert_eq!(ix.root_of("/w/new/src"), None);
        assert_eq!(ix.name_of("/w/old"), "one");
        let _ = fs.clone().repo("/w/new", "ref: refs/heads/feat").repo("/w/old", "ref: refs/heads/two");
        assert_eq!(ix.root_of("/w/new/src"), None, "answered from cache");
        ix.clear();
        assert_eq!(ix.root_of("/w/new/src").as_deref(), Some("/w/new"));
        assert_eq!(ix.name_of("/w/old"), "two");
    }

    #[test]
    fn home_root_and_system_dirs_are_never_projects() {
        let fs = FakeFs::default()
            .repo("/Users/me", "ref: refs/heads/main")
            .repo("/opt/homebrew", "ref: refs/heads/master")
            .repo("/", "x");
        let mut ix = index(fs);
        assert_eq!(ix.root_of("/Users/me"), None);
        assert_eq!(ix.root_of("/Users/me/Downloads"), None);
        assert_eq!(ix.root_of("/Users/me/Library/Caches/x"), None);
        assert_eq!(ix.root_of("/opt/homebrew/Library"), None);
        assert_eq!(ix.root_of("/"), None);
        assert_eq!(ix.root_of("/usr/local/bin"), None);
    }

    #[test]
    fn nested_repo_wins_over_parent() {
        let fs = FakeFs::default()
            .repo("/Users/me/git/mono", "ref: refs/heads/main")
            .repo("/Users/me/git/mono/vendor/lib", "ref: refs/heads/x");
        let mut ix = index(fs);
        assert_eq!(ix.root_of("/Users/me/git/mono/vendor/lib/src").as_deref(), Some("/Users/me/git/mono/vendor/lib"));
        assert_eq!(ix.root_of("/Users/me/git/mono/src").as_deref(), Some("/Users/me/git/mono"));
    }

    #[test]
    fn default_branch_and_detached_head_show_the_folder() {
        let fs = FakeFs::default()
            .repo("/w/api", "ref: refs/heads/main\n")
            .repo("/w/web", "3f2a1c0d9e8b7a6f5e4d3c2b1a0f9e8d7c6b5a4f\n")
            .repo("/w/docs", "ref: refs/heads/fix-typo");
        let mut ix = RepoIndex::with_fs("/Users/me".into(), Box::new(fs));
        assert_eq!(ix.name_of("/w/api"), "api");
        assert_eq!(ix.name_of("/w/web"), "web");
        assert_eq!(ix.name_of("/w/docs"), "fix-typo");
    }

    #[test]
    fn real_linked_worktree_head() {
        // This crate lives in a git checkout; it may be a linked worktree.
        let manifest = env!("CARGO_MANIFEST_DIR");
        let mut ix = RepoIndex::new(std::env::var("HOME").unwrap());
        let root = ix.root_of(manifest).expect("crate is inside a git checkout");
        assert!(RealFs.read_head(&root).is_some());
        assert!(!ix.name_of(&root).is_empty());
    }
}
