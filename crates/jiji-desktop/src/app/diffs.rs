//! Reading diffs: what names one, the small cache of the ones already
//! read and laid out, and the reads in flight, which everyone who asks for
//! the same diff shares.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures::future::{FutureExt as _, Shared};
use gpui::{AppContext as _, Context, Task};

use super::AppModel;
use crate::diff::model::Prepared;
use crate::host::{CommandError, HostResult};

/// Which diff: a repo at one operation, a change, and what it is measured
/// from (`None` is its parent).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub repo: String,
    pub op: String,
    pub from: Option<String>,
    pub id: String,
}

impl DiffKey {
    /// The same comparison, whatever operation it was read at.
    pub fn same_comparison(&self, other: &DiffKey) -> bool {
        self.repo == other.repo && self.from == other.from && self.id == other.id
    }
}

/// A read under way, for whoever is waiting on it.
pub type PendingDiff = Shared<Task<HostResult<Prepared>>>;

/// What asking for a diff gets.
pub enum DiffRead {
    /// Read and laid out already: it can go on screen now.
    Ready(Prepared),
    /// On its way, by this request or one before it.
    Pending(PendingDiff),
}

/// Diffs are cached per repo operation, laid out: re-selecting a change
/// shows it without a read or a rebuild, and snapshot republishes that
/// record no new operation (view switches, converged refreshes) skip the
/// backend entirely.
#[derive(Default)]
pub struct DiffCache {
    entries: Vec<(DiffKey, Prepared)>,
    /// Reads under way, by what they read: a selection that arrives while
    /// its diff is being prefetched waits on that read.
    pub(crate) inflight: HashMap<DiffKey, PendingDiff>,
}

const MAX_CACHED_DIFFS: usize = 16;

impl DiffCache {
    pub fn get(&mut self, key: &DiffKey) -> Option<Prepared> {
        let at = self.entries.iter().position(|(cached, _)| cached == key)?;
        // Most recently used last.
        let entry = self.entries.remove(at);
        let prepared = entry.1.clone();
        self.entries.push(entry);
        Some(prepared)
    }

    fn contains(&self, key: &DiffKey) -> bool {
        self.entries.iter().any(|(cached, _)| cached == key)
    }

    fn insert(&mut self, key: DiffKey, prepared: Prepared) {
        self.entries.retain(|(cached, _)| *cached != key);
        self.entries.push((key, prepared));
        if self.entries.len() > MAX_CACHED_DIFFS {
            self.entries.remove(0);
        }
    }
}

impl AppModel {
    /// One diff, from the cache when it has it, else from the read in
    /// flight for it or a new one.
    pub fn fetch_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) -> DiffRead {
        match self.diffs.get(&key) {
            Some(prepared) => DiffRead::Ready(prepared),
            None => DiffRead::Pending(self.read_diff(key, cx)),
        }
    }

    /// Warm the cache for a diff the reader is one step away from.
    pub fn prefetch_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) {
        if !self.diffs.contains(&key) {
            // Nobody waits on it: the read lands in the cache by itself.
            drop(self.read_diff(key, cx));
        }
    }

    /// The backend read and the layout, both off the UI thread, started
    /// once however many ask; the result lands in the cache.
    fn read_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) -> PendingDiff {
        if let Some(pending) = self.diffs.inflight.get(&key) {
            return pending.clone();
        }
        let host = self.host.clone();
        let read = key.clone();
        let pending = cx
            .background_spawn(async move {
                let path = Path::new(&read.repo);
                let backend = host.backend();
                let diff = match &read.from {
                    Some(from) => backend.compare_diff(path, from, &read.id),
                    None => backend.change_diff(path, &read.id),
                }?;
                Ok::<_, CommandError>(Prepared::new(Arc::new(diff)))
            })
            .shared();
        self.diffs.inflight.insert(key.clone(), pending.clone());
        cx.spawn({
            let pending = pending.clone();
            async move |this, cx| {
                let read = pending.await;
                let _ = this.update(cx, |this, _| {
                    this.diffs.inflight.remove(&key);
                    if let Ok(prepared) = read {
                        this.diffs.insert(key, prepared);
                    }
                });
            }
        })
        .detach();
        pending
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use gpui::TestAppContext;

    use super::{DiffKey, DiffRead};

    #[gpui::test]
    fn a_prefetch_and_a_selection_of_one_diff_share_its_read(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("diff-read-shared");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let snapshot = jiji_core::mock::mock_snapshot(Path::new(&repo));
        let key = DiffKey {
            repo: repo.clone(),
            op: snapshot.operations[0].id.clone(),
            from: None,
            id: "mvkortsq".into(),
        };
        // The graph's neighbour, warmed; then selected before it lands.
        model.update(cx, |model, cx| model.prefetch_diff(key.clone(), cx));
        let prefetching = model.read_with(cx, |model, _| model.diffs.inflight[&key].clone());
        let selected = model.update(cx, |model, cx| model.fetch_diff(key.clone(), cx));
        let DiffRead::Pending(selected) = selected else {
            panic!("nothing has landed yet");
        };
        assert!(
            selected.ptr_eq(&prefetching),
            "a second read of the same diff"
        );
        cx.run_until_parked();
        let shown = selected
            .peek()
            .cloned()
            .expect("the read is done")
            .expect("the mock reads it");

        // Shown once, it is highlighted once.
        let executor = cx.executor();
        let highlighting = shown.highlighting(&executor).as_ptr();
        cx.run_until_parked();

        // Back to it later: the same laid-out document, at once, with the
        // colours its first showing gave it.
        let again = model.update(cx, |model, cx| model.fetch_diff(key.clone(), cx));
        let DiffRead::Ready(again) = again else {
            panic!("the cache has it");
        };
        assert!(Arc::ptr_eq(again.doc(), shown.doc()));
        let doc = again.doc();
        assert!((0..doc.lines.len() as u32).any(|line| !doc.marks(line).is_empty()));
        assert_eq!(again.highlighting(&executor).as_ptr(), highlighting);
        model.read_with(cx, |model, _| assert!(model.diffs.inflight.is_empty()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
