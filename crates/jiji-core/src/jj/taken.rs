//! Branches taken over from someone else.
//!
//! Taking a branch over makes it an ordinary tracked bookmark, and jj has
//! no place to say that the branch was someone else's. This record does,
//! so that every plan that pushes the branch can name whose it is, and so
//! that giving it back is the reverse of exactly what was taken. It lives
//! with the repository, shared by its workspaces, next to the archive of
//! hidden changes.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use jj_lib::backend::ChangeId;
use serde::{Deserialize, Serialize};

use super::*;
use crate::snapshot::TakenOver;

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    branches: Vec<TakenOver>,
}

fn record_path(workspace: &Workspace) -> PathBuf {
    workspace.repo_path().join("jiji/taken-over.json")
}

/// The record as it is on disk. No file is an empty record.
fn read_record(path: &Path) -> Result<Vec<TakenOver>, BackendError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(mutation_err(err)),
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|err| {
        BackendError::MutationFailed(format!("could not read {}: {err}", path.display()))
    })?;
    if record.version != 1 {
        return Err(BackendError::MutationFailed(format!(
            "{} was written by a newer Jiji (version {})",
            path.display(),
            record.version
        )));
    }
    Ok(record.branches)
}

fn write_record(path: &Path, branches: &[TakenOver]) -> Result<(), BackendError> {
    let dir = path.parent().expect("the record has a directory");
    fs::create_dir_all(dir).map_err(mutation_err)?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(mutation_err)?;
    let record = Record {
        version: 1,
        branches: branches.to_vec(),
    };
    serde_json::to_writer_pretty(&mut file, &record).map_err(mutation_err)?;
    file.write_all(b"\n").map_err(mutation_err)?;
    file.as_file().sync_all().map_err(mutation_err)?;
    file.persist(path).map_err(mutation_err)?;
    #[cfg(unix)]
    File::open(dir).and_then(|f| f.sync_all()).map_err(mutation_err)?;
    Ok(())
}

/// Whether `entry` is, in `view`, what it was recorded as: a local bookmark
/// that tracks the branch on its remote.
fn stands(view: &View, entry: &TakenOver) -> bool {
    let symbol = RemoteRefSymbol {
        name: RefName::new(&entry.bookmark),
        remote: RemoteName::new(&entry.remote),
    };
    view.get_local_bookmark(symbol.name).is_present()
        && view.get_remote_bookmark(symbol).state == RemoteRefState::Tracked
}

/// Whether the branch `entry` names is still on its remote, as far as this
/// repository has seen.
fn on_its_remote(view: &View, entry: &TakenOver) -> bool {
    let symbol = RemoteRefSymbol {
        name: RefName::new(&entry.bookmark),
        remote: RemoteName::new(&entry.remote),
    };
    view.get_remote_bookmark(symbol).target.is_present()
}

/// Drop the entries whose branch is gone from its remote: deleted from
/// here once its pull request had landed, or found deleted by a fetch. A
/// branch of that name made later is another branch, and the user's own.
/// Called after a push and after a fetch, the two that can find a branch
/// gone. An entry whose bookmark was only untracked here stays: the branch
/// is still theirs, and tracking it again is taking it up again.
///
/// The push or the fetch has happened by now, so a record that cannot be
/// read or written does not fail it.
pub(super) fn forget_gone(workspace: &Workspace, view: &View) {
    let path = record_path(workspace);
    if !path.exists() {
        return;
    }
    let Ok(record) = read_record(&path) else {
        return;
    };
    let kept: Vec<TakenOver> = record
        .iter()
        .filter(|entry| on_its_remote(view, entry))
        .cloned()
        .collect();
    if kept.len() != record.len() {
        if let Err(err) = write_record(&path, &kept) {
            tracing::warn!(%err, "could not update the record of taken-over branches");
        }
    }
}

/// For the snapshot: the recorded branches that still stand. An entry
/// whose bookmark is gone, or no longer tracks its remote, is left over
/// (the bookmark was deleted or untracked by hand) and is not shown. A
/// record that cannot be read shows nothing: a snapshot does not fail over
/// it, and the commands that change the record say what is wrong with it.
pub(super) fn taken_over_in_view(workspace: &Workspace, view: &View) -> Vec<TakenOver> {
    read_record(&record_path(workspace))
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| stands(view, entry))
        .collect()
}

/// What a take-over brings up to date: their changes, from `root` up, go
/// onto `onto`.
struct CatchUp {
    /// Their bottom change, the one that sits on the earlier copy.
    root: CommitId,
    /// The earlier copy it sits on.
    earlier: CommitId,
    /// The copy of that change the user has now.
    onto: CommitId,
    /// The change both are copies of, as jj shows it.
    change: String,
    /// How many changes of theirs move.
    own: usize,
}

/// A branch someone opened on a stack is built on the stack's commits as
/// they were then. When the user has rewritten one of them since (amended
/// it, or landed what was under it, which rebases the rest), the branch
/// still carries the earlier copy. Taken over as it is, it would put that
/// copy under a bookmark here, beside the current one: the same change
/// twice, which jj calls divergent, in two stacks that Publish and Land
/// would each take for the whole (seen on the sandbox: Land planned to
/// merge around the user's own branch, and a publish of that branch was
/// refused as divergent). So a take-over brings their changes onto the
/// current copy, here and nowhere else, as jj would have done at the
/// rewrite had the branch been a bookmark then.
///
/// `None` when the branch sits on the user's changes as they are, or on
/// nothing of the user's. An error where there is no one place to bring
/// it to: nothing is taken over then.
fn catch_up(
    workspace: &Workspace,
    repo: &Arc<ReadonlyRepo>,
    name: &str,
    remote: &str,
    head: &CommitId,
) -> Result<Option<CatchUp>, BackendError> {
    let trunk = resolve_revset_text(workspace, repo.as_ref(), TRUNK_AND_ROOT)
        .map_err(BackendError::RevsetInvalid)?
        .ancestors();
    // The user's own: what is under the bookmarks here and the working
    // copies.
    let view = repo.view();
    let mut tips: Vec<CommitId> = view
        .local_bookmarks()
        .filter(|(other, _)| other.as_str() != name)
        .flat_map(|(_, target)| target.added_ids().cloned())
        .collect();
    tips.extend(view.wc_commit_ids().values().cloned());
    let mine = RevsetExpression::commits(tips).ancestors().minus(&trunk);
    // What the branch brings that is not here already. Children before
    // parents.
    let arriving = RevsetExpression::commits(vec![head.clone()])
        .ancestors()
        .minus(&trunk)
        .minus(&mine);
    let arriving = collect_revset(arriving, repo.as_ref(), MAX_MUTABLE_NODES)?;
    if arriving.is_empty() {
        return Ok(None);
    }
    let commit_of = |id: &CommitId| repo.store().get_commit(id).map_err(snapshot_err);
    let mut current: HashMap<ChangeId, Vec<CommitId>> = HashMap::new();
    for id in collect_revset(mine, repo.as_ref(), MAX_MUTABLE_NODES)? {
        current
            .entry(commit_of(&id)?.change_id().clone())
            .or_default()
            .push(id);
    }
    // An arriving commit with the change id of a change of the user's is
    // a copy of it. It has to be an earlier copy of the one the user has:
    // a commit that one was rewritten from, here. A copy that was made
    // elsewhere is a second version of the change, and so is one of a
    // change that has two copies here already. Which version stands is
    // not for a take-over to decide.
    let rewritten_from = |now: &CommitId, copy: &CommitId| -> Result<bool, BackendError> {
        let walk = evolution::walk_predecessors(repo, std::slice::from_ref(now));
        for entry in blocking_iter(walk).take(MAX_EVOLOG_ENTRIES) {
            if entry.map_err(snapshot_err)?.commit.id() == copy {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let mut earlier: HashMap<CommitId, CommitId> = HashMap::new();
    for id in &arriving {
        let commit = commit_of(id)?;
        match current.get(commit.change_id()).map(Vec::as_slice) {
            None => {}
            Some([now]) if rewritten_from(now, id)? => {
                earlier.insert(id.clone(), now.clone());
            }
            Some(_) => {
                return Err(BackendError::MutationFailed(format!(
                    "{name}@{remote} carries a copy of {} that is not an earlier copy of the \
                     one here: there are two versions of that change, and which one stands is \
                     yours to decide. Nothing was changed",
                    short_change_id(repo.as_ref(), &commit)
                )));
            }
        }
    }
    if earlier.is_empty() {
        return Ok(None);
    }
    if earlier.contains_key(head) {
        return Err(BackendError::MutationFailed(format!(
            "{name}@{remote} is at an earlier copy of {}, a change of yours, with nothing of \
             its own on top: there is nothing to take over",
            short_change_id(repo.as_ref(), &commit_of(head)?)
        )));
    }
    // One place to bring it to: their changes sit on an earlier copy in
    // one line, with no copy among them.
    let tangled = || {
        BackendError::MutationFailed(format!(
            "{name}@{remote} was built on earlier copies of your changes, and its own changes \
             do not sit on them in one line: there is no one place to bring them to. Nothing \
             was changed"
        ))
    };
    let mut roots: Vec<Commit> = Vec::new();
    // Parents first: a commit is theirs to move when it is the root or
    // sits on one that is.
    let mut own: Vec<CommitId> = Vec::new();
    for id in arriving.iter().rev() {
        let commit = commit_of(id)?;
        let on_theirs = commit.parent_ids().iter().any(|p| own.contains(p));
        if earlier.contains_key(id) {
            if on_theirs {
                return Err(tangled());
            }
            continue;
        }
        let on_a_copy = commit.parent_ids().iter().any(|p| earlier.contains_key(p));
        if on_a_copy {
            roots.push(commit);
        }
        if on_a_copy || on_theirs {
            own.push(id.clone());
        }
    }
    let [root] = roots.as_slice() else {
        return Err(tangled());
    };
    let [parent] = root.parent_ids() else {
        return Err(tangled());
    };
    Ok(Some(CatchUp {
        root: root.id().clone(),
        earlier: parent.clone(),
        onto: earlier[parent].clone(),
        change: short_change_id(repo.as_ref(), &commit_of(parent)?),
        own: own.len(),
    }))
}

impl JjBackend {
    pub(super) fn take_over_branch_impl(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
        number: u64,
        author: Option<&str>,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let (name, remote) = (name.trim(), remote.trim());
        let symbol = validated_remote_symbol(repo.as_ref(), name, remote)?;
        let remote_ref = repo.view().get_remote_bookmark(symbol);
        if remote_ref.state == RemoteRefState::Tracked {
            return Err(BackendError::MutationFailed(format!(
                "\u{201c}{name}\u{201d} is a bookmark here already: it tracks {name}@{remote}"
            )));
        }
        if repo.view().get_local_bookmark(symbol.name).is_present() {
            return Err(BackendError::MutationFailed(format!(
                "a bookmark \u{201c}{name}\u{201d} exists here and is not that branch (it does \
                 not track {name}@{remote}); rename it first"
            )));
        }
        let Some(head) = remote_ref.target.as_normal().cloned() else {
            return Err(BackendError::MutationFailed(format!(
                "{name}@{remote} is conflicted; fetch again"
            )));
        };
        let catch_up = catch_up(&workspace, &repo, name, remote, &head)?;

        // The record first: an entry for a bookmark that does not stand is
        // ignored, a taken-over bookmark without its entry would be pushed
        // without a word about whose branch it is.
        let record_path = record_path(&workspace);
        let mut record: Vec<TakenOver> = read_record(&record_path)?
            .into_iter()
            .filter(|entry| on_its_remote(repo.view(), entry))
            .filter(|entry| !(entry.bookmark == name && entry.remote == remote))
            .collect();
        record.push(TakenOver {
            bookmark: name.to_owned(),
            remote: remote.to_owned(),
            number,
            author: author.map(str::to_owned),
        });
        write_record(&record_path, &record)?;

        let mut tx = repo.start_transaction();
        pollster::block_on(tx.repo_mut().track_remote_bookmark(symbol)).map_err(mutation_err)?;
        if let Some(catch_up) = &catch_up {
            pollster::block_on(move_commits(
                tx.repo_mut(),
                &MoveCommitsLocation {
                    new_parent_ids: vec![catch_up.onto.clone()],
                    new_child_ids: vec![],
                    target: MoveCommitsTarget::Roots(vec![catch_up.root.clone()]),
                },
                &RebaseOptions::default(),
            ))
            .map_err(mutation_err)?;
            pollster::block_on(tx.repo_mut().rebase_descendants()).map_err(mutation_err)?;
            // jj leaves the commit a rewritten one sat on in view. Here
            // that is the earlier copy, which only their branch had
            // brought back into view: with their changes off it, it goes
            // out of view again, and what it sat on with it. Nothing is
            // rewritten for that, and a copy that another branch still
            // builds on is not a head and stays where it is.
            tx.repo_mut().remove_head(&catch_up.earlier);
        }
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("take over {name}@{remote} (pull request #{number})"),
        )?;
        // The bookmark is where their changes are now: at the remote's
        // commit, or at its rebased copy.
        let at = new_repo
            .view()
            .get_local_bookmark(symbol.name)
            .as_normal()
            .cloned()
            .unwrap_or_else(|| head.clone());
        let summary = match &catch_up {
            None => format!(
                "Took over \u{201c}{name}\u{201d}: it is a bookmark here now, at {}, and \
                 nothing was pushed",
                &head.hex()[..8]
            ),
            Some(catch_up) => format!(
                "Took over \u{201c}{name}\u{201d}: it is a bookmark here now, and nothing was \
                 pushed. It was built on an earlier copy of {}, so its {} rebased onto the \
                 copy you have now, here only; the next Publish pushes that",
                catch_up.change,
                if catch_up.own == 1 {
                    "one change was".to_owned()
                } else {
                    format!("{} changes were", catch_up.own)
                },
            ),
        };
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary,
            target_change: Some(display_id_of(new_repo.as_ref(), &at)?),
        })
    }

    pub(super) fn give_back_branch_impl(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let (name, remote) = (name.trim(), remote.trim());
        let record_path = record_path(&workspace);
        let record = read_record(&record_path)?;
        let Some(entry) = record
            .iter()
            .find(|entry| entry.bookmark == name && entry.remote == remote)
            .cloned()
        else {
            return Err(BackendError::MutationFailed(format!(
                "\u{201c}{name}\u{201d} was not taken over here, so there is nothing to give \
                 back; to stop following a branch of your own, untrack it"
            )));
        };
        let symbol = validated_remote_symbol(repo.as_ref(), name, remote)?;
        let remote_ref = repo.view().get_remote_bookmark(symbol);
        let local = repo.view().get_local_bookmark(symbol.name);
        if remote_ref.state != RemoteRefState::Tracked || local.is_absent() {
            return Err(BackendError::MutationFailed(format!(
                "\u{201c}{name}\u{201d} is no longer a bookmark here that tracks \
                 {name}@{remote}: there is nothing left to give back"
            )));
        }
        // What is here and not there would be left without a bookmark, as
        // a second copy of their changes. Giving back is for a branch
        // that is, here, what its remote has.
        if local != &remote_ref.target {
            return Err(BackendError::MutationFailed(format!(
                "\u{201c}{name}\u{201d} is not what {remote} has for it: there are changes \
                 here that were not published, or commits there that were not brought in. \
                 Publish it, or point it back at {name}@{remote}, and give it back then"
            )));
        }
        // Work of the user's own on top of it would be left resting on
        // someone else's branch, and jj would call it, and the stack under
        // it, immutable history of theirs.
        let Some(head) = local.as_normal().cloned() else {
            return Err(BackendError::MutationFailed(format!(
                "\u{201c}{name}\u{201d} is conflicted; repoint it first"
            )));
        };
        let others: Vec<(String, CommitId)> = repo
            .view()
            .local_bookmarks()
            .filter(|(other, _)| other.as_str() != name)
            .flat_map(|(other, target)| {
                target
                    .added_ids()
                    .map(move |id| (other.as_str().to_owned(), id.clone()))
            })
            .collect();
        let on_top: HashSet<CommitId> = RevsetExpression::commits(
            others.iter().map(|(_, id)| id.clone()).collect(),
        )
        .intersection(&RevsetExpression::commits(vec![head.clone()]).descendants())
        .evaluate(repo.as_ref())
        .map_err(snapshot_err)?
        .iter_blocking()
        .collect::<Result<_, _>>()
        .map_err(snapshot_err)?;
        let mut resting: Vec<String> = others
            .into_iter()
            .filter(|(_, id)| on_top.contains(id))
            .map(|(other, _)| other)
            .collect();
        resting.sort();
        resting.dedup();
        if !resting.is_empty() {
            return Err(BackendError::MutationFailed(format!(
                "{} of yours {} on \u{201c}{name}\u{201d}: given back, that work would rest on \
                 someone else's branch. Move it off first, or keep \u{201c}{name}\u{201d}",
                name_list(
                    &resting
                        .iter()
                        .map(|other| format!("\u{201c}{other}\u{201d}"))
                        .collect::<Vec<_>>()
                ),
                if resting.len() == 1 { "sits" } else { "sit" },
            )));
        }

        let mut tx = repo.start_transaction();
        tx.repo_mut().untrack_remote_bookmark(symbol);
        tx.repo_mut()
            .set_local_bookmark_target(symbol.name, RefTarget::absent());
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!(
                "give back {name}@{remote} (pull request #{})",
                entry.number
            ),
        )?;
        let left: Vec<TakenOver> = record
            .into_iter()
            .filter(|kept| !(kept.bookmark == name && kept.remote == remote))
            .filter(|kept| on_its_remote(new_repo.view(), kept))
            .collect();
        write_record(&record_path, &left)?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!(
                "Gave back \u{201c}{name}\u{201d}: it is no bookmark here any more, and \
                 {name}@{remote} is as it was"
            ),
            target_change: None,
        })
    }
}
