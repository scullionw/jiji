//! Reading diffs: what names one, the small cache of the ones already
//! read, and the fetch that fills it off the UI thread.

use std::path::Path;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Task};

use super::AppModel;
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

/// Diffs are cached per repo operation: re-selecting a change renders
/// without a fetch, and snapshot republishes that record no new operation
/// (view switches, converged refreshes) skip the backend entirely.
#[derive(Default)]
pub struct DiffCache {
    entries: Vec<(DiffKey, Arc<jiji_core::snapshot::ChangeDiff>)>,
    inflight: std::collections::HashSet<DiffKey>,
}

const MAX_CACHED_DIFFS: usize = 16;

impl DiffCache {
    pub fn get(&mut self, key: &DiffKey) -> Option<Arc<jiji_core::snapshot::ChangeDiff>> {
        let at = self.entries.iter().position(|(cached, _)| cached == key)?;
        // Most recently used last.
        let entry = self.entries.remove(at);
        let diff = entry.1.clone();
        self.entries.push(entry);
        Some(diff)
    }

    pub fn insert(&mut self, key: DiffKey, diff: Arc<jiji_core::snapshot::ChangeDiff>) {
        self.inflight.remove(&key);
        self.entries.retain(|(cached, _)| *cached != key);
        self.entries.push((key, diff));
        if self.entries.len() > MAX_CACHED_DIFFS {
            self.entries.remove(0);
        }
    }
}

impl AppModel {
    /// Read one diff, from the cache when it has it. The backend call and
    /// the row-model build both happen off the UI thread.
    pub fn fetch_diff(
        &mut self,
        key: DiffKey,
        cx: &mut Context<Self>,
    ) -> Task<
        HostResult<(
            Arc<jiji_core::snapshot::ChangeDiff>,
            crate::diff::model::DiffDoc,
        )>,
    > {
        let cached = self.diffs.get(&key);
        let host = self.host.clone();
        let fetch_key = key.clone();
        let fetched = cx.background_spawn(async move {
            let diff = match cached {
                Some(diff) => diff,
                None => {
                    let path = Path::new(&fetch_key.repo);
                    let backend = host.backend();
                    Arc::new(match &fetch_key.from {
                        Some(from) => backend.compare_diff(path, from, &fetch_key.id),
                        None => backend.change_diff(path, &fetch_key.id),
                    }?)
                }
            };
            let doc = crate::diff::model::DiffDoc::build(&diff);
            Ok::<_, CommandError>((diff, doc))
        });
        cx.spawn(async move |this, cx| {
            let (diff, doc) = fetched.await?;
            let _ = this.update(cx, |this, _| this.diffs.insert(key, diff.clone()));
            Ok((diff, doc))
        })
    }

    /// Warm the cache for a diff the reader is one step away from.
    pub fn prefetch_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) {
        if self.diffs.get(&key).is_some() || !self.diffs.inflight.insert(key.clone()) {
            return;
        }
        let host = self.host.clone();
        let fetch_key = key.clone();
        let fetched = cx.background_spawn(async move {
            let path = Path::new(&fetch_key.repo);
            match &fetch_key.from {
                Some(from) => host.backend().compare_diff(path, from, &fetch_key.id),
                None => host.backend().change_diff(path, &fetch_key.id),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = fetched.await;
            let _ = this.update(cx, |this, _| match result {
                Ok(diff) => this.diffs.insert(key, Arc::new(diff)),
                Err(_) => {
                    this.diffs.inflight.remove(&key);
                }
            });
        })
        .detach();
    }
}
