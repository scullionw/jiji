//! The graph after a rewrite: each change's row moves from where it stood
//! to where the rewrite put it, a row that is new fades in while the rows
//! around it part, and a row the rewrite took away folds away where it
//! was — the rewrite read as one movement rather than a jump. A row's
//! rails and station travel with it and take their new columns at once:
//! rails are drawn row by row, and rows moving at different speeds could
//! not keep a line joined across them while its columns turned.
//!
//! Only a rewrite moves rows: a new operation in the same repository and
//! view. Opening, switching the view or the density, and every refresh
//! that changed nothing are instant.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use jiji_core::snapshot::RepoSnapshot;

use crate::logic::graph::{GraphModel, GraphRow};
use crate::ui::motion::{ease_out, STRUCTURE};

/// A new row waits this share of the move before fading in, so the rows
/// making room for it read first.
const ARRIVAL_WAIT: f32 = 0.25;

pub(super) struct Motion {
    since: Instant,
    /// Each change's row top before the rewrite.
    before: HashMap<String, f32>,
    /// What was on screen, for the rows the rewrite took away.
    pub old_model: Rc<GraphModel>,
    pub old_snapshot: Arc<RepoSnapshot>,
    /// Those rows: their place in the old model, and their top in it.
    pub gone: Vec<(usize, f32)>,
}

/// The change a row stands for.
fn key<'a>(snapshot: &'a RepoSnapshot, row: &GraphRow) -> Option<&'a str> {
    match row {
        GraphRow::Node(node) => Some(snapshot.nodes[node.node].id.as_str()),
        GraphRow::Elision(_) => None,
    }
}

/// What each row is known by across a rewrite: its change, or for the
/// elided history under a change, that change's id and a `~`, so it moves
/// with the row it hangs from.
pub(super) fn row_keys(snapshot: &RepoSnapshot, model: &GraphModel) -> Vec<Option<String>> {
    let mut above: Option<&str> = None;
    model
        .rows
        .iter()
        .map(|row| match key(snapshot, row) {
            Some(id) => {
                above = Some(id);
                Some(id.to_owned())
            }
            None => above.map(|id| format!("{id}~")),
        })
        .collect()
}

/// Each row's top, and the total height under them.
pub(super) fn tops(model: &GraphModel, height: impl Fn(&GraphRow) -> f32) -> Vec<f32> {
    let mut tops = Vec::with_capacity(model.rows.len() + 1);
    let mut total = 0.;
    for row in &model.rows {
        tops.push(total);
        total += height(row);
    }
    tops.push(total);
    tops
}

fn head_operation(snapshot: &RepoSnapshot) -> Option<&str> {
    snapshot.operations.first().map(|op| op.id.as_str())
}

impl Motion {
    /// The move from the graph on screen to the one `new` draws, when
    /// `new` is a rewrite of it that put rows somewhere else.
    pub fn between(
        old: (&Arc<RepoSnapshot>, &Rc<GraphModel>),
        new: (&RepoSnapshot, &GraphModel),
        height: impl Fn(&GraphRow) -> f32,
        now: Instant,
    ) -> Option<Self> {
        let ((old_snapshot, old_model), (snapshot, model)) = (old, new);
        if old_snapshot.repo_path != snapshot.repo_path
            || old_snapshot.view != snapshot.view
            || head_operation(old_snapshot) == head_operation(snapshot)
        {
            return None;
        }
        let old_tops = tops(old_model, &height);
        let old_keys = row_keys(old_snapshot, old_model);
        let before: HashMap<String, f32> = old_keys
            .iter()
            .zip(&old_tops)
            .filter_map(|(key, top)| Some((key.clone()?, *top)))
            .collect();
        let new_tops = tops(model, &height);
        let mut moved = false;
        let mut kept = std::collections::HashSet::new();
        for (key, top) in row_keys(snapshot, model).into_iter().zip(&new_tops) {
            let Some(key) = key else {
                continue;
            };
            moved |= before.get(&key).is_none_or(|was| was != top);
            kept.insert(key);
        }
        let gone: Vec<(usize, f32)> = old_keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.as_ref().is_some_and(|key| !kept.contains(key)))
            .map(|(ix, _)| (ix, old_tops[ix]))
            .collect();
        (moved || !gone.is_empty()).then(|| Self {
            since: now,
            before,
            old_model: old_model.clone(),
            old_snapshot: old_snapshot.clone(),
            gone,
        })
    }

    /// How far through the move `now` is, from 0 to 1.
    pub fn progress(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.since).as_secs_f32() / STRUCTURE.as_secs_f32()).min(1.)
    }

    /// Where the row for `id`, bound for `to`, is drawn at `progress`, and
    /// how strongly: a new row fades in once the others have begun to
    /// make room for it.
    pub fn place(&self, id: Option<&str>, to: f32, progress: f32) -> (f32, f32) {
        match id.and_then(|id| self.before.get(id)) {
            Some(from) => (from + (to - from) * ease_out(progress), 1.),
            None if id.is_some() => (
                to,
                ease_out(((progress - ARRIVAL_WAIT) / (1. - ARRIVAL_WAIT)).clamp(0., 1.)),
            ),
            None => (to, 1.),
        }
    }

    /// Whether the row for `id` is on its way into the view between
    /// `top` and `bottom`, from where it stood before.
    pub fn came_from(&self, id: &str, top: f32, bottom: f32) -> bool {
        self.before
            .get(id)
            .is_some_and(|from| *from >= top && *from < bottom)
    }

    /// The change a row in the new graph stands for.
    pub fn key<'a>(snapshot: &'a RepoSnapshot, row: &GraphRow) -> Option<&'a str> {
        key(snapshot, row)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::logic::graph::build_graph_model;

    fn graph(snapshot: &RepoSnapshot) -> (Arc<RepoSnapshot>, Rc<GraphModel>) {
        (
            Arc::new(snapshot.clone()),
            Rc::new(build_graph_model(snapshot)),
        )
    }

    #[test]
    fn a_rewrite_moves_rows_from_where_they_stood_and_nothing_else_moves_them() {
        let before = jiji_core::mock::mock_snapshot(Path::new("/repo"));
        // Abandon the change under the working copy: the working copy stays
        // put, everything below it moves up a row, and the change folds away.
        let mut after = before.clone();
        let gone = after.nodes.iter().position(|n| n.id == "lwnouzpy").unwrap();
        let parents = after.nodes[gone].parents.clone();
        after.nodes.remove(gone);
        for node in &mut after.nodes {
            if node.parents.iter().any(|parent| parent == "lwnouzpy") {
                node.parents = parents.clone();
            }
        }
        for stream in &mut after.workstreams {
            stream.node_ids.retain(|id| id != "lwnouzpy");
        }
        let mut op = after.operations[0].clone();
        op.id = "fresh-operation".into();
        after.operations.insert(0, op);

        let height = |_: &GraphRow| 26.;
        let now = Instant::now();
        let (old_snapshot, old_model) = graph(&before);
        let (_, new_model) = graph(&after);
        let motion = Motion::between(
            (&old_snapshot, &old_model),
            (&after, &new_model),
            height,
            now,
        )
        .expect("a rewrite moves rows");
        assert_eq!(motion.gone.len(), 1);
        let new_tops = tops(&new_model, height);
        let row_of = |id: &str| {
            new_model
                .rows
                .iter()
                .position(|row| Motion::key(&after, row) == Some(id))
                .unwrap()
        };
        let moved = row_of("mvkortsq");
        let (to, from) = (new_tops[moved], new_tops[moved] + 26.);
        // It starts where it stood and ends where it went, decelerating.
        assert_eq!(motion.place(Some("mvkortsq"), to, 0.), (from, 1.));
        let (halfway, _) = motion.place(Some("mvkortsq"), to, 0.5);
        assert!(to < halfway && halfway < to + 13., "{halfway}");
        assert_eq!(motion.place(Some("mvkortsq"), to, 1.), (to, 1.));
        // A row that was not there waits, then fades in.
        assert_eq!(motion.place(Some("brand-new"), 52., 0.2), (52., 0.));
        assert_eq!(motion.place(Some("brand-new"), 52., 1.), (52., 1.));
        assert_eq!(motion.progress(now + STRUCTURE / 2), 0.5);
        assert_eq!(motion.progress(now + Duration::from_secs(1)), 1.);

        // The same operation again moves nothing; nor does another view.
        assert!(Motion::between(
            (&old_snapshot, &old_model),
            (&before, &old_model),
            height,
            now
        )
        .is_none());
        let mut other_view = after.clone();
        other_view.view = jiji_core::snapshot::SnapshotView::All;
        assert!(Motion::between(
            (&old_snapshot, &old_model),
            (&other_view, &new_model),
            height,
            now
        )
        .is_none());
    }
}
