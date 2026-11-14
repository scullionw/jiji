//! Drag-and-drop policy for the graph: what dropping one change onto
//! another means, decided in one place before anything runs. The rows, the
//! floating plan card, the status bar and the drop itself all render this
//! shared plan instead of inventing their own rules. Dropping onto a row
//! makes that row the new parent — `jj rebase -s` by default, `jj rebase
//! -r` when the change moves alone — and the preview is the plan step:
//! releasing is the confirmation, the operation log the undo. The backend
//! re-checks everything; this is the affordance rule, not the enforcement.

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot};

use super::inspect::{descendants_of, find_node};
use crate::logic::text::{counted, short_id};

/// Which backend call a drop makes: with descendants, or only this change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropOp {
    Rebase,
    Move,
}

/// What releasing the pointer here would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropPlan {
    Allowed {
        op: DropOp,
        /// The gesture had to degrade to a lone move because the target
        /// sits inside the dragged stack — the card says so before the
        /// drop.
        forced_alone: bool,
        summary: String,
        consequences: Vec<String>,
    },
    Refused(String),
}

impl DropPlan {
    pub fn allowed(&self) -> bool {
        matches!(self, DropPlan::Allowed { .. })
    }
}

/// Immutable rows stay put — same rule as the change header's verbs.
/// Divergent copies drag fine: their node ids are commit-keyed, which is
/// exactly how a mutation picks one side deliberately.
pub fn can_drag(node: &GraphNode) -> bool {
    node.kind != NodeKind::Immutable
}

/// The changes a drop would rewrite: the source, plus its descendants when
/// they come along. The graph washes this set while a row is in hand.
pub fn drag_affected_ids(snapshot: &RepoSnapshot, source: &str, moves_alone: bool) -> Vec<String> {
    let mut ids = vec![source.to_owned()];
    if !moves_alone {
        ids.extend(
            descendants_of(snapshot, source)
                .into_iter()
                .map(|n| n.id.clone()),
        );
    }
    ids
}

pub fn plan_drop(
    snapshot: &RepoSnapshot,
    source_id: &str,
    target_id: &str,
    alone: bool,
) -> DropPlan {
    let Some(source) = find_node(snapshot, source_id).filter(|node| can_drag(node)) else {
        return DropPlan::Refused("Immutable changes cannot be rebased".into());
    };
    let Some(target) = find_node(snapshot, target_id) else {
        // A refresh can land mid-drag and take the row with it.
        return DropPlan::Refused("That change left the snapshot".into());
    };
    if target.id == source.id {
        return DropPlan::Refused("A change cannot become its own parent".into());
    }

    let descendants = descendants_of(snapshot, source_id);
    let target_in_stack = descendants.iter().any(|d| d.id == target.id);
    // Dropping below its own descendants has exactly one legal meaning —
    // the lone move that reorders the stack — so the gesture degrades to
    // it and the plan says so instead of refusing.
    let forced_alone = target_in_stack && !alone;
    let op = if target_in_stack || (alone && !descendants.is_empty()) {
        DropOp::Move
    } else {
        DropOp::Rebase
    };

    let sole_parent = source.parents.len() == 1 && source.parents[0] == target.id;
    if sole_parent && op == DropOp::Rebase {
        return DropPlan::Refused(format!(
            "Already the parent of {} — nothing would move",
            short_id(&source.id, 4)
        ));
    }

    let dest = short_id(&target.id, 4);
    let parents_label = source
        .parents
        .iter()
        .map(|id| short_id(id, 4))
        .collect::<Vec<_>>()
        .join(", ");
    let is_child_target = target.parents.contains(&source.id);

    let summary = if op == DropOp::Rebase {
        if descendants.is_empty() {
            format!("Rebase onto {dest}")
        } else {
            format!(
                "Rebase onto {dest} — with {}",
                counted(descendants.len(), "descendant")
            )
        }
    } else if sole_parent {
        format!(
            "Extract {} — its descendants skip past it",
            short_id(&source.id, 4)
        )
    } else if is_child_target {
        format!("Move onto {dest} — swapping their order")
    } else if target_in_stack {
        format!("Move onto {dest} — reordering the stack")
    } else {
        format!("Move onto {dest} — only this change")
    };

    let mut consequences = Vec::new();
    if forced_alone {
        consequences.push(format!(
            "{dest} is inside the dragged stack, so this change moves alone."
        ));
    }
    if op == DropOp::Move && !descendants.is_empty() {
        consequences.push(if sole_parent {
            format!(
                "It stays on {parents_label}; {} reparent onto {parents_label}.",
                counted(descendants.len(), "descendant")
            )
        } else {
            format!(
                "{} stay{} behind, reparented onto {parents_label}.",
                counted(descendants.len(), "descendant"),
                if descendants.len() == 1 { "s" } else { "" }
            )
        });
    }
    if source.parents.len() > 1 {
        consequences.push(format!(
            "Its {} parents are replaced by the destination — the merge is dissolved.",
            source.parents.len()
        ));
    }
    let working_copy = &snapshot.working_copy;
    if source.id == *working_copy {
        consequences.push("The working copy moves with it.".into());
    } else if descendants.iter().any(|d| d.id == *working_copy) {
        consequences.push(
            if op == DropOp::Rebase {
                "The working copy follows the rebase."
            } else {
                "The working copy stays behind."
            }
            .into(),
        );
    }

    DropPlan::Allowed {
        op,
        forced_alone,
        summary,
        consequences,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::{MockBackend, RepoBackend};

    fn snapshot() -> RepoSnapshot {
        let dir = std::env::temp_dir().join(format!("jiji-dnd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        MockBackend::default().open(&dir).unwrap()
    }

    /// A mutable change with a mutable child: (parent, child).
    fn stacked_pair(snapshot: &RepoSnapshot) -> (&GraphNode, &GraphNode) {
        snapshot
            .nodes
            .iter()
            .filter(|node| node.kind != NodeKind::Immutable && node.parents.len() == 1)
            .find_map(|child| {
                let parent = find_node(snapshot, &child.parents[0])?;
                (parent.kind != NodeKind::Immutable).then_some((parent, child))
            })
            .expect("the mock snapshot has a mutable stack")
    }

    #[test]
    fn immutable_rows_stay_put_and_nothing_parents_itself() {
        let snapshot = snapshot();
        let immutable = snapshot
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Immutable)
            .unwrap();
        let (parent, child) = stacked_pair(&snapshot);
        assert!(!can_drag(immutable) && can_drag(child));
        assert!(!plan_drop(&snapshot, &immutable.id, &child.id, false).allowed());
        assert_eq!(
            plan_drop(&snapshot, &child.id, &child.id, false),
            DropPlan::Refused("A change cannot become its own parent".into())
        );
        // Onto the parent it already has: nothing would move.
        assert!(matches!(
            plan_drop(&snapshot, &child.id, &parent.id, false),
            DropPlan::Refused(reason) if reason.starts_with("Already the parent")
        ));
        assert!(!plan_drop(&snapshot, &child.id, "gone", false).allowed());
    }

    #[test]
    fn dropping_into_its_own_stack_degrades_to_a_lone_move() {
        let snapshot = snapshot();
        let (parent, child) = stacked_pair(&snapshot);
        let plan = plan_drop(&snapshot, &parent.id, &child.id, false);
        let DropPlan::Allowed {
            op,
            forced_alone,
            summary,
            consequences,
        } = plan
        else {
            panic!("a reorder is allowed");
        };
        assert_eq!(op, DropOp::Move);
        assert!(forced_alone);
        assert!(summary.contains("swapping their order"), "{summary}");
        assert!(consequences[0].contains("moves alone"));
    }

    #[test]
    fn a_rebase_takes_descendants_and_alt_leaves_them() {
        let snapshot = snapshot();
        let (parent, _) = stacked_pair(&snapshot);
        let elsewhere = snapshot
            .nodes
            .iter()
            .find(|node| {
                node.id != parent.id
                    && !parent.parents.contains(&node.id)
                    && !descendants_of(&snapshot, &parent.id)
                        .iter()
                        .any(|d| d.id == node.id)
            })
            .unwrap();
        let descendants = descendants_of(&snapshot, &parent.id).len();
        let with = plan_drop(&snapshot, &parent.id, &elsewhere.id, false);
        assert!(matches!(
            &with,
            DropPlan::Allowed { op: DropOp::Rebase, summary, .. }
                if summary.contains(&format!("with {descendants} descendant"))
        ));
        let alone = plan_drop(&snapshot, &parent.id, &elsewhere.id, true);
        assert!(matches!(
            &alone,
            DropPlan::Allowed { op: DropOp::Move, forced_alone: false, summary, .. }
                if summary.ends_with("only this change")
        ));
        assert_eq!(
            drag_affected_ids(&snapshot, &parent.id, true),
            [parent.id.clone()]
        );
        assert_eq!(
            drag_affected_ids(&snapshot, &parent.id, false).len(),
            descendants + 1
        );
    }
}
