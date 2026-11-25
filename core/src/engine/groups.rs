//! Group registry: maps owners (worktrees, folders, apps, docker stacks,
//! System) to stable ids, the rows of the `groups` table.

use std::collections::{HashMap, HashSet};

use crate::collect::classify::Owner;
use crate::collect::repo::{RepoIndex, basename};
use crate::model::{Gid, Group, GroupKind};

#[derive(Default)]
pub struct Registry {
    by_key: HashMap<(GroupKind, String), Gid>,
    groups: HashMap<Gid, Group>,
    next: Gid,
    /// Groups created or renamed since the last flush.
    dirty: Vec<Gid>,
    home: String,
    /// App name → gid: apps are one row per name, whatever the bundle path.
    app_names: HashMap<String, Gid>,
    /// Groups seen since the last `purged` call (the hourly maintenance).
    seen: HashSet<Gid>,
}

/// macOS's own agents (Dock, NotificationCenter, WindowManager…) live in
/// bundles under /System/Library: they are System, not apps. Apps the user
/// opens (/System/Applications/Activity Monitor.app) stay apps.
pub(crate) fn is_system_bundle(path: &str) -> bool {
    ["/System/Library/", "/usr/", "/Library/Apple/"].iter().any(|p| path.starts_with(p))
}

impl Registry {
    /// Starts from the groups already in the database, so ids stay stable.
    pub fn new(home: &str, existing: Vec<Group>) -> Self {
        let mut r = Registry { next: 1, home: home.trim_end_matches('/').to_string(), ..Default::default() };
        for g in existing {
            r.next = r.next.max(g.id + 1);
            if g.kind == GroupKind::App {
                // Lowest id wins if the database has two apps with one name.
                let e = r.app_names.entry(g.name.clone()).or_insert(g.id);
                *e = (*e).min(g.id);
            }
            r.by_key.insert((g.kind, g.path.clone()), g.id);
            r.groups.insert(g.id, g);
        }
        r
    }

    pub fn get(&self, gid: Gid) -> Option<&Group> {
        self.groups.get(&gid)
    }

    /// Id for (kind, path), creating or renaming the group as needed.
    pub fn ensure(&mut self, kind: GroupKind, path: &str, name: &str, parent: Option<Gid>) -> Gid {
        let id = self.ensure_unseen(kind, path, name, parent);
        self.seen.insert(id);
        id
    }

    fn ensure_unseen(&mut self, kind: GroupKind, path: &str, name: &str, parent: Option<Gid>) -> Gid {
        if let Some(&id) = self.by_key.get(&(kind, path.to_string())) {
            let g = self.groups.get_mut(&id).expect("registry in sync");
            if g.name != name || g.parent != parent {
                g.name = name.to_string();
                g.parent = parent;
                self.dirty.push(id);
            }
            return id;
        }
        let id = self.next;
        self.next += 1;
        let g = Group { id, kind, name: name.to_string(), path: path.to_string(), parent };
        self.by_key.insert((kind, path.to_string()), id);
        self.groups.insert(id, g);
        self.dirty.push(id);
        id
    }

    pub fn of_owner(&mut self, owner: &Owner, repos: &mut RepoIndex) -> Gid {
        match owner {
            Owner::Worktree(root) => {
                let name = repos.name_of(root);
                self.ensure(GroupKind::Worktree, root, &name, None)
            }
            Owner::Folder(dir) => {
                let name = if **dir == *self.home { "~".to_string() } else { basename(dir).to_string() };
                self.ensure(GroupKind::Worktree, dir, &name, None)
            }
            Owner::App(bundle) if is_system_bundle(bundle) => self.ensure(GroupKind::System, "", "System", None),
            Owner::App(bundle) => {
                let name = basename(bundle).trim_end_matches(".app");
                // Two copies of one app (an updater's staged copy, per-host
                // helper bundles) are one row: the first path seen is kept.
                if let Some(&id) = self.app_names.get(name) {
                    self.seen.insert(id);
                    return id;
                }
                let id = self.ensure(GroupKind::App, bundle, name, None);
                self.app_names.insert(name.to_string(), id);
                id
            }
            Owner::System => self.ensure(GroupKind::System, "", "System", None),
        }
    }

    /// Groups to write at the next flush (and forget them as dirty).
    pub fn take_dirty(&mut self) -> Vec<Group> {
        let mut ids = std::mem::take(&mut self.dirty);
        ids.sort_unstable();
        ids.dedup();
        ids.iter().filter_map(|id| self.groups.get(id).cloned()).collect()
    }

    /// Put groups back as dirty after a failed flush.
    pub fn mark_dirty(&mut self, groups: &[Group]) {
        self.dirty.extend(groups.iter().map(|g| g.id));
    }

    /// The daily purge deleted these groups' rows. One seen since the last
    /// call is still alive (its newest minutes weren't rolled up yet): it is
    /// written again at the next flush. The others are forgotten, and get a
    /// new id if they come back.
    pub fn purged(&mut self, gids: &[Gid]) {
        for &gid in gids {
            if self.seen.contains(&gid) {
                self.dirty.push(gid);
            } else if let Some(g) = self.groups.remove(&gid) {
                self.by_key.remove(&(g.kind, g.path));
                if self.app_names.get(&g.name) == Some(&gid) {
                    self.app_names.remove(&g.name);
                }
            }
        }
        self.seen.clear();
    }

    pub fn parent_of(&self, gid: Gid) -> Option<Gid> {
        self.groups.get(&gid).and_then(|g| g.parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::repo::tests::FakeFs;

    #[test]
    fn ids_are_stable_and_renames_are_flushed() {
        let existing = vec![Group { id: 7, kind: GroupKind::App, name: "Safari".into(), path: "/Applications/Safari.app".into(), parent: None }];
        let mut r = Registry::new("/Users/me", existing);
        let mut repos = RepoIndex::with_fs("/Users/me".into(), Box::new(FakeFs::default().repo("/w/a", "ref: refs/heads/feat/x")));
        assert_eq!(r.of_owner(&Owner::App("/Applications/Safari.app".into()), &mut repos), 7);
        assert!(r.take_dirty().is_empty());
        let a = r.of_owner(&Owner::Worktree("/w/a".into()), &mut repos);
        assert_eq!(a, 8);
        assert_eq!(r.get(a).unwrap().name, "feat/x");
        let home = r.of_owner(&Owner::Folder("/Users/me".into()), &mut repos);
        assert_eq!(r.get(home).unwrap().name, "~");
        let sys = r.of_owner(&Owner::System, &mut repos);
        assert_eq!(r.take_dirty().iter().map(|g| g.id).collect::<Vec<_>>(), vec![8, 9, 10]);
        assert_eq!(r.of_owner(&Owner::System, &mut repos), sys);
        // A docker stack moving under a worktree is re-flushed.
        let s = r.ensure(GroupKind::Docker, "shop@/w/a", "shop", None);
        r.take_dirty();
        r.ensure(GroupKind::Docker, "shop@/w/a", "shop", Some(a));
        assert_eq!(r.take_dirty()[0].id, s);
        assert_eq!(r.parent_of(s), Some(a));
    }

    #[test]
    fn apps_merge_by_name_and_system_bundles_are_system() {
        let mut r = Registry::new("/Users/me", vec![]);
        let mut repos = RepoIndex::with_fs("/Users/me".into(), Box::new(FakeFs::default()));
        let a = r.of_owner(&Owner::App("/x/host-1/app.noindex/Helper Agents.app".into()), &mut repos);
        let b = r.of_owner(&Owner::App("/x/host-2/app.noindex/Helper Agents.app".into()), &mut repos);
        assert_eq!(a, b);
        let sys = r.of_owner(&Owner::System, &mut repos);
        assert_eq!(r.of_owner(&Owner::App("/System/Library/CoreServices/Dock.app".into()), &mut repos), sys);
        let am = r.of_owner(&Owner::App("/System/Applications/Utilities/Activity Monitor.app".into()), &mut repos);
        assert!(!is_system_bundle("/System/Applications/Calculator.app"));
        assert!(!is_system_bundle("/System/Volumes/Preboot/Cryptexes/App/System/Applications/Safari.app"));
        assert_eq!(r.get(am).unwrap().kind, GroupKind::App);
        // Restart: the name map is rebuilt from the database.
        let groups: Vec<Group> = r.take_dirty();
        let mut r2 = Registry::new("/Users/me", groups);
        assert_eq!(r2.of_owner(&Owner::App("/x/host-3/app.noindex/Helper Agents.app".into()), &mut repos), a);
    }

    #[test]
    fn a_purged_app_frees_its_name() {
        let mut r = Registry::new("/Users/me", vec![]);
        let mut repos = RepoIndex::with_fs("/Users/me".into(), Box::new(FakeFs::default()));
        let old = r.of_owner(&Owner::App("/Volumes/Installer/Editor.app".into()), &mut repos);
        r.take_dirty();
        // Seen in the last hour: kept, and written again.
        r.purged(&[old]);
        assert_eq!(r.take_dirty().iter().map(|g| g.id).collect::<Vec<_>>(), [old]);
        // Not seen since: forgotten, so the copy in /Applications isn't
        // merged into a row the database no longer has.
        r.purged(&[old]);
        let new = r.of_owner(&Owner::App("/Applications/Editor.app".into()), &mut repos);
        assert_ne!(new, old);
        assert_eq!(r.get(new).unwrap().path, "/Applications/Editor.app");
        assert_eq!(r.get(old), None);
    }
}
