//! The tab strip: what each tab is called, switching and closing, and
//! opening a repository into the right one.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AppContext as _, Context, PromptLevel};
use jiji_core::snapshot::{RepoSnapshot, SnapshotView};

use super::{AppEvent, AppModel, ClosedTab, Section, MAX_CLOSED};
use crate::host::HostResult;

/// The tab strip's label: the repo's name, its path's last segment while
/// the name has not arrived, or the empty-tab placeholder.
pub fn tab_label(name: Option<&str>, path: Option<&str>) -> String {
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    path.and_then(|path| path.trim_end_matches('/').rsplit('/').next())
        .filter(|base| !base.is_empty())
        .unwrap_or("New Tab")
        .to_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabTitle {
    pub label: String,
    /// The enclosing directories that tell this tab apart from others
    /// sharing its label; `None` while the label is unique.
    pub hint: Option<String>,
}

/// Labels for a strip of tabs, disambiguated the way editors do for
/// same-named files: repos sharing a name each gain the shortest run of
/// parent directories that no other shares.
pub fn tab_titles(tabs: &[(u64, Option<&str>, Option<&str>)]) -> Vec<(u64, TabTitle)> {
    let mut titles: Vec<(u64, TabTitle)> = tabs
        .iter()
        .map(|&(id, name, path)| {
            (
                id,
                TabTitle {
                    label: tab_label(name, path),
                    hint: None,
                },
            )
        })
        .collect();
    let parents: Vec<Option<Vec<&str>>> = tabs
        .iter()
        .map(|&(_, _, path)| {
            path.map(|path| {
                let mut segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
                segments.pop();
                segments
            })
        })
        .collect();
    let tail = |segments: &[&str], depth: usize| {
        segments[segments.len().saturating_sub(depth)..].join("/")
    };
    for ix in 0..tabs.len() {
        let Some(mine) = &parents[ix] else { continue };
        let group: Vec<usize> = (0..tabs.len())
            .filter(|&other| {
                other != ix
                    && parents[other].is_some()
                    && titles[other].1.label == titles[ix].1.label
            })
            .collect();
        if group.is_empty() {
            continue;
        }
        let mut depth = 1;
        while depth < mine.len()
            && group
                .iter()
                .any(|&other| tail(parents[other].as_ref().unwrap(), depth) == tail(mine, depth))
        {
            depth += 1;
        }
        let hint = tail(mine, depth);
        titles[ix].1.hint = Some(if hint.is_empty() { "/".into() } else { hint });
    }
    titles
}

impl AppModel {
    pub(super) fn switch_to_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.active_id == id || !self.tabs.iter().any(|tab| tab.id == id) {
            return;
        }
        self.active_id = id;
        self.intent = None;
        let tab = self.tab_mut();
        tab.ui.selected_range = None;
        if tab.snapshot.is_some() && !tab.seen {
            Self::select_working_copy(tab);
        }
        self.persist_tabs();
        self.follow_repo(cx);
        cx.emit(AppEvent::Snapshot);
        cx.emit(AppEvent::Navigation);
        cx.notify();
    }

    /// Bring a repo tab's content up: open fresh when it has no snapshot,
    /// or quietly re-sync a tab that already has one so a missed watcher
    /// beat cannot leave stale state on screen.
    pub(super) fn ensure_tab_loaded(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_by_id(id) else {
            return;
        };
        let Some(path) = tab.path.clone() else {
            return;
        };
        if tab.snapshot.is_some() {
            self.bg(cx, move |host| {
                let _ = host.refresh_snapshot(&path);
            })
            .detach();
        } else {
            self.open_repo_into(id, path, cx);
        }
    }

    pub fn activate_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.active_id == id {
            return;
        }
        self.switch_to_tab(id, cx);
        self.ensure_tab_loaded(id, cx);
    }

    pub fn new_tab(&mut self, cx: &mut Context<Self>) {
        let id = self.create_tab(None, None);
        self.switch_to_tab(id, cx);
    }

    /// Close a tab; its host session drops (the watcher stops, a live
    /// auto-land job for the repo detaches into its resumable record). The
    /// last tab never closes into nothing — it gives way to an empty tab,
    /// except a lone empty tab, which just stays.
    pub fn close_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if self.tabs.len() == 1 && self.tabs[ix].path.is_none() {
            return;
        }
        let was_active = self.active_id == id;
        let tab = self.tabs.remove(ix);
        if let Some(path) = tab.path {
            self.closed.retain(|closed| closed.path != path);
            self.closed.push(ClosedTab { path: path.clone() });
            if self.closed.len() > MAX_CLOSED {
                self.closed.remove(0);
            }
            self.bg(cx, move |host| host.close_repo(&path)).detach();
        }
        if self.tabs.is_empty() {
            self.create_tab(None, None);
        }
        if was_active {
            let neighbor = self.tabs[ix.min(self.tabs.len() - 1)].id;
            self.active_id = 0;
            self.switch_to_tab(neighbor, cx);
            self.ensure_tab_loaded(neighbor, cx);
        }
        self.persist_tabs();
        cx.notify();
    }

    pub fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        self.close_tab(self.active_id, cx);
    }

    /// The label of the closed tab ⌘⇧T would reopen.
    pub fn reopenable_tab(&self) -> Option<String> {
        self.closed
            .iter()
            .rev()
            .find(|closed| {
                !self
                    .tabs
                    .iter()
                    .any(|tab| tab.path.as_deref() == Some(&closed.path))
            })
            .map(|closed| tab_label(None, Some(&closed.path)))
    }

    /// Bring back the most recently closed repo tab (⌘⇧T), newest first
    /// on repeat — skipping repos reopened another way meanwhile.
    pub fn reopen_closed_tab(&mut self, cx: &mut Context<Self>) {
        while let Some(closed) = self.closed.pop() {
            let open = self
                .tabs
                .iter()
                .any(|tab| tab.path.as_deref() == Some(&closed.path));
            if !open {
                self.open_repo_at(closed.path, cx);
                return;
            }
        }
    }

    /// Cycle the active tab (⌃Tab / ⌘⇧] with +1, reversed with -1).
    pub fn cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        if count < 2 {
            return;
        }
        let Some(ix) = self.tabs.iter().position(|tab| tab.id == self.active_id) else {
            return;
        };
        let next = (ix as isize + delta).rem_euclid(count) as usize;
        self.activate_tab(self.tabs[next].id, cx);
    }

    /// Open a repo somewhere sensible: the tab that already shows it (or is
    /// already opening it), the active tab while it is free, or a new tab.
    ///
    /// Opens land later, so several can be asked for before the first does
    /// (two folders opened together from Finder). A tab an open is headed
    /// for counts as taken: each open gets a tab of its own.
    pub fn open_repo_at(&mut self, path: String, cx: &mut Context<Self>) {
        let existing = self
            .tabs
            .iter()
            .find(|tab| tab.path.as_deref() == Some(&path) || tab.opening.as_deref() == Some(&path))
            .map(|tab| tab.id);
        if let Some(existing) = existing {
            self.activate_tab(existing, cx);
            return;
        }
        let id = if self.tab().is_free() {
            self.active_id
        } else {
            let id = self.create_tab(None, None);
            self.switch_to_tab(id, cx);
            id
        };
        self.open_repo_into(id, path, cx);
    }

    /// Open `path` into a tab. The tab can lose the screen, or close,
    /// while the open is in flight: everything after the open names the
    /// opened repo explicitly instead of trusting whichever tab is active
    /// by then.
    pub(super) fn open_repo_into(&mut self, tab_id: u64, path: String, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_by_id(tab_id) {
            tab.opening = Some(path.clone());
            tab.ui.error = None;
        }
        cx.notify();
        let opened = self.bg(cx, {
            let path = path.clone();
            move |host| host.open_repo(&path)
        });
        cx.spawn(async move |this, cx| {
            let result = opened.await;
            let _ = this.update(cx, |this, cx| this.finish_open(tab_id, result, cx));
        })
        .detach();
    }

    pub(super) fn finish_open(
        &mut self,
        tab_id: u64,
        result: HostResult<Arc<RepoSnapshot>>,
        cx: &mut Context<Self>,
    ) {
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let Some(tab) = self.tab_by_id(tab_id) else {
                    return;
                };
                tab.ui.error = Some(error.message);
                if error.code == "git_repo_without_jj" {
                    if let Some(root) = error.path.clone() {
                        // Still this folder's tab while the offer is up.
                        cx.notify();
                        self.offer_jj_setup(tab_id, root, cx);
                        return;
                    }
                }
                tab.opening = None;
                cx.notify();
                return;
            }
        };
        let repo_path = snapshot.repo_path.clone();
        if !self.tabs.iter().any(|tab| tab.id == tab_id) {
            // Closed mid-open: the session the host just started has no
            // tab to feed, so drop it — unless another tab holds the repo.
            if self.tab_for_path(&repo_path).is_none() {
                self.bg(cx, move |host| host.close_repo(&repo_path))
                    .detach();
            }
            return;
        }
        // The requested path may be an alias of the canonical root of a
        // repo already open in another tab — fold into that tab instead of
        // showing the same repo twice.
        let dupe = self
            .tabs
            .iter()
            .find(|tab| tab.id != tab_id && tab.path.as_deref() == Some(&repo_path))
            .map(|tab| tab.id);
        if let Some(dupe) = dupe {
            let was_active = self.active_id == tab_id;
            self.tabs.retain(|tab| tab.id != tab_id);
            if was_active {
                self.active_id = 0;
                self.switch_to_tab(dupe, cx);
            }
            self.apply_snapshot(snapshot, cx);
            self.persist_tabs();
            cx.notify();
            return;
        }
        if let Some(tab) = self.tab_by_id(tab_id) {
            tab.path = Some(repo_path.clone());
            tab.opening = None;
            tab.ui.section = Section::Workbench;
        }
        self.apply_snapshot(snapshot.clone(), cx);
        self.persist_tabs();
        self.recent_repos = self.store.remember_repo(&repo_path, &snapshot.repo_name);
        cx.notify();

        // The lens choice sticks per repo; a fresh session opens on the
        // default, so re-apply the persisted one. A choice that stopped
        // working (a custom revset naming a deleted bookmark) falls back
        // silently.
        let lens = self.store.repo_view(&repo_path);
        if lens != SnapshotView::Workstreams && lens != snapshot.view {
            let applied = self.bg(cx, {
                let repo_path = repo_path.clone();
                move |host| host.set_snapshot_view(&repo_path, lens)
            });
            cx.spawn(async move |this, cx| {
                let result = applied.await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(snapshot) => this.apply_snapshot(snapshot, cx),
                    // A tab closed meanwhile refuses because its session
                    // is gone, not because the lens broke.
                    Err(_) if this.tab_for_path(&repo_path).is_some() => {
                        this.store
                            .set_repo_view(&repo_path, &SnapshotView::Workstreams);
                    }
                    Err(_) => {}
                });
            })
            .detach();
        }
    }

    /// A plain Git repository: offer to set jj up in it, the way `jj git
    /// init --colocate` does, then open it. Nothing touches the folder
    /// unless the user says yes; declining leaves the refusal in the
    /// status bar. The tab stays the folder's (`opening`) until the answer
    /// is in, so nothing else opens into it meanwhile.
    pub(super) fn offer_jj_setup(&mut self, tab_id: u64, git_root: String, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            self.stop_opening(tab_id, cx);
            return;
        };
        let name = git_root
            .rsplit(['/', '\\'])
            .find(|segment| !segment.is_empty())
            .unwrap_or(&git_root)
            .to_owned();
        let detail = format!(
            "{git_root} is a Git repository without jj.\n\nJiji can set jj up here the way \
             `jj git init --colocate` does: a .jj folder next to .git, with your branches \
             imported as bookmarks. Git keeps working alongside it."
        );
        let answer = cx.update_window(window, |_, window, cx| {
            window.prompt(
                PromptLevel::Info,
                &format!("Set up jj in \u{201c}{name}\u{201d}?"),
                Some(&detail),
                &["Set up jj", "Cancel"],
                cx,
            )
        });
        let Ok(answer) = answer else {
            self.stop_opening(tab_id, cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                let _ = this.update(cx, |this, cx| this.stop_opening(tab_id, cx));
                return;
            }
            let Ok(init) = this.update(cx, |this, cx| {
                if let Some(tab) = this.tab_by_id(tab_id) {
                    tab.ui.error = None;
                }
                cx.notify();
                this.bg(cx, move |host| host.init_jj_repo(&git_root))
            }) else {
                return;
            };
            let result = init.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(root) => this.open_repo_into(tab_id, root, cx),
                Err(error) => {
                    if let Some(tab) = this.tab_by_id(tab_id) {
                        tab.opening = None;
                        tab.ui.error = Some(error.message);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// An open that will not land after all: its tab is free again.
    fn stop_opening(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_by_id(tab_id) {
            tab.opening = None;
            cx.notify();
        }
    }

    /// Remove a repo from the recents list (the welcome screen's ×).
    pub fn forget_recent_repo(&mut self, path: &str, cx: &mut Context<Self>) {
        self.recent_repos = self.store.forget_repo(path);
        cx.notify();
    }

    /// The open dialog: a folder, opened like any other route.
    pub fn choose_repo(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| {
                        this.open_repo_at(path.to_string_lossy().into_owned(), cx)
                    });
                }
            }
        })
        .detach();
    }

    /// Folders dropped on the window (or handed over by the `jiji`
    /// launcher) open like picks from the dialog, one tab each.
    pub fn open_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let target = crate::host::repo_root_for(&path).unwrap_or(path);
            self.open_repo_at(target.to_string_lossy().into_owned(), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{git_repo, install, jj_repo, scratch};
    use gpui::TestAppContext;

    /// Where each tab's open in flight is headed, in strip order.
    fn headed(model: &AppModel) -> Vec<Option<String>> {
        model.tabs.iter().map(|tab| tab.opening.clone()).collect()
    }

    /// The repo each tab shows, in strip order.
    fn shown(model: &AppModel) -> Vec<Option<String>> {
        model.tabs.iter().map(|tab| tab.path.clone()).collect()
    }

    fn open(model: &gpui::Entity<AppModel>, paths: &[&String], cx: &mut TestAppContext) {
        let paths = paths.iter().map(PathBuf::from).collect();
        model.update(cx, |model, cx| model.open_paths(paths, cx));
    }

    #[gpui::test]
    fn folders_opened_together_get_a_tab_each(cx: &mut TestAppContext) {
        let dir = scratch("tabs-together");
        let model = install(&dir, cx);
        let (alpha, beta) = (jj_repo(&dir, "alpha"), jj_repo(&dir, "beta"));

        // Neither open has landed when the second is asked for: the first
        // already holds the empty tab the app started on.
        open(&model, &[&alpha, &beta], cx);
        model.read_with(cx, |model, _| {
            assert_eq!(headed(model), [Some(alpha.clone()), Some(beta.clone())]);
        });

        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            assert_eq!(shown(model), [Some(alpha.clone()), Some(beta.clone())]);
            assert_eq!(headed(model), [None, None]);
            // The last one asked for has the screen.
            assert_eq!(model.tab().path, Some(beta.clone()));
            assert_eq!(model.store.tabs().repos.len(), 2);
            assert_eq!(model.recent_repos.len(), 2);
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_folder_asked_for_twice_opens_once_and_a_failed_open_frees_its_tab(
        cx: &mut TestAppContext,
    ) {
        let dir = scratch("tabs-twice");
        let model = install(&dir, cx);
        let name = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let (missing, other) = (name("missing"), name("other"));

        // Still on its way: the second ask goes to the tab it is headed for.
        open(&model, &[&missing, &other, &missing], cx);
        model.read_with(cx, |model, _| {
            assert_eq!(headed(model), [Some(missing.clone()), Some(other.clone())]);
            assert_eq!(model.tab().opening, Some(missing.clone()));
        });

        // Nothing is at either path, so both opens fail, each in its own
        // tab, which is then free for the next open.
        cx.run_until_parked();
        let third = name("third");
        model.read_with(cx, |model, _| {
            assert_eq!(headed(model), [None, None]);
            assert!(model.tabs.iter().all(|tab| tab.ui.error.is_some()));
        });
        open(&model, &[&third], cx);
        model.read_with(cx, |model, _| {
            assert_eq!(headed(model), [Some(third.clone()), None])
        });
        cx.run_until_parked();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A plain Git folder asked for, up to the point where the app is
    /// offering to set jj up in it.
    fn offered(name: &str, cx: &mut TestAppContext) -> (gpui::Entity<AppModel>, PathBuf, String) {
        let dir = scratch(name);
        let model = install(&dir, cx);
        // The offer is a question asked on the app's window.
        cx.add_window(crate::shell::Shell::new);
        let plain = git_repo(&dir, "plain");
        open(&model, &[&plain], cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        (model, dir, plain)
    }

    #[gpui::test]
    fn a_folder_being_offered_jj_setup_keeps_its_tab(cx: &mut TestAppContext) {
        let (model, dir, plain) = offered("tabs-offer-accept", cx);

        // While the question is up, another repo arrives: it must not
        // take the tab the offer is about.
        let other = jj_repo(&dir, "other");
        open(&model, &[&other], cx);
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            assert_eq!(headed(model), [Some(plain.clone()), None]);
            assert_eq!(shown(model), [None, Some(other.clone())]);
        });

        cx.simulate_prompt_answer("Set up jj");
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            assert_eq!(shown(model), [Some(plain.clone()), Some(other.clone())]);
            assert_eq!(headed(model), [None, None]);
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn declining_jj_setup_frees_the_tab_and_keeps_the_refusal(cx: &mut TestAppContext) {
        let (model, dir, plain) = offered("tabs-offer-decline", cx);

        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            assert_eq!((shown(model), headed(model)), (vec![None], vec![None]));
            assert!(model.tab().ui.error.is_some());
            assert!(!std::path::Path::new(&plain).join(".jj").exists());
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tab_labels_fall_back_to_the_path_then_a_placeholder() {
        assert_eq!(tab_label(Some("jiji"), Some("/x/y")), "jiji");
        assert_eq!(tab_label(None, Some("/work/api/")), "api");
        assert_eq!(tab_label(Some(""), None), "New Tab");
    }

    #[test]
    fn same_named_repos_gain_the_shortest_distinguishing_parents() {
        let titles = tab_titles(&[
            (1, Some("api"), Some("/home/me/work/api")),
            (2, Some("api"), Some("/home/me/oss/api")),
            (3, Some("web"), Some("/home/me/work/web")),
            (4, None, None),
        ]);
        assert_eq!(titles[0].1.hint.as_deref(), Some("work"));
        assert_eq!(titles[1].1.hint.as_deref(), Some("oss"));
        assert_eq!(titles[2].1.hint, None);
        assert_eq!(titles[3].1.label, "New Tab");

        // Sharing the immediate parent too: one more level tells them apart.
        let deeper = tab_titles(&[
            (1, Some("api"), Some("/a/x/src/api")),
            (2, Some("api"), Some("/a/y/src/api")),
        ]);
        assert_eq!(deeper[0].1.hint.as_deref(), Some("x/src"));
        assert_eq!(deeper[1].1.hint.as_deref(), Some("y/src"));
    }
}
