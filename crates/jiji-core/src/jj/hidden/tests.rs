use super::*;
use jj_lib::config::StackedConfig;

struct TestRepo {
    dir: tempfile::TempDir,
    backend: JjBackend,
}

impl TestRepo {
    fn new() -> Self {
        Self::with_colocation(true)
    }

    fn with_colocation(colocated: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let settings = UserSettings::from_config(StackedConfig::with_defaults()).unwrap();
        if colocated {
            pollster::block_on(Workspace::init_colocated_git(&settings, dir.path(), gix::hash::Kind::Sha1)).unwrap();
        } else {
            pollster::block_on(Workspace::init_internal_git(&settings, dir.path(), gix::hash::Kind::Sha1)).unwrap();
        }
        fs::write(
            dir.path().join(".jj/repo/config.toml"),
            "user.name = 'Test User'\nuser.email = 'test@example.com'\n",
        )
        .unwrap();
        let test = Self {
            dir,
            backend: JjBackend::with_user_config(UserConfigSource::None),
        };
        let base = test.change("base", "shared baseline");
        test.backend
            .create_bookmark(test.path(), "main", &base)
            .unwrap();
        test.backend.new_change(test.path(), &base).unwrap();
        test
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn change(&self, file: &str, contents: &str) -> String {
        fs::write(self.path().join(file), contents).unwrap();
        let id = self.backend.refresh(self.path()).unwrap().working_copy;
        self.backend.describe(self.path(), &id, contents).unwrap();
        id
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn visible(&self) -> HashSet<String> {
        let (_, repo) = self.backend.load_repo_at_head(self.path()).unwrap();
        visible_ids(repo.as_ref()).unwrap()
    }
}

#[test]
fn hidden_working_copy_and_its_old_versions_disappear_and_survive_git_gc() {
    let t = TestRepo::new();
    let id = t.change("idea", "secret older proposal");
    t.backend
        .describe(t.path(), &id, "secret latest proposal")
        .unwrap();
    t.backend.create_bookmark(t.path(), "idea", &id).unwrap();
    // Even an edit not yet recorded by jj must be recoverable.
    fs::write(t.path().join("idea"), "unsnapshotted secret").unwrap();
    let entry = t.backend.hide(t.path(), &id, false, false).unwrap();
    assert!(!t.path().join("idea").exists());
    assert!(!t.git(&["log", "--all", "--format=%s"]).contains("secret"));
    assert!(!t.git(&["branch", "--all"]).contains("idea"));
    assert!(entry.git_roots.len() > 1, "must include earlier versions");
    assert!(!t.visible().contains(&entry.changes[0].commit_id));
    t.backend.refresh(t.path()).unwrap();
    assert!(!t.git(&["log", "--all", "--format=%s"]).contains("secret"));

    t.git(&["reflog", "expire", "--expire=now", "--all"]);
    t.git(&["gc", "--prune=now"]);
    assert!(!t.git(&["log", "--all", "--format=%s"]).contains("secret"));
    t.backend.unhide(t.path(), &id, false).unwrap();
    assert!(t.visible().contains(&entry.changes[0].commit_id));
    assert!(t
        .git(&["log", "--all", "--format=%s"])
        .contains("secret latest proposal"));
    assert!(t.backend.hidden(t.path()).unwrap().is_empty());
    assert!(
        !t.path().join("idea").exists(),
        "unhide must not switch the working copy"
    );
    t.backend.edit_change(t.path(), &id).unwrap();
    assert_eq!(
        fs::read_to_string(t.path().join("idea")).unwrap(),
        "unsnapshotted secret"
    );
}

#[test]
fn stack_selection_stops_at_immutable_history_and_any_member_restores_it() {
    let t = TestRepo::new();
    let a = t.change("a", "secret first idea");
    t.backend.create_bookmark(t.path(), "first", &a).unwrap();
    t.backend.new_change(t.path(), &a).unwrap();
    let b = t.change("b", "secret second idea");
    t.backend.create_bookmark(t.path(), "second", &b).unwrap();
    let entry = t.backend.hide(t.path(), "second", true, false).unwrap();
    assert_eq!(entry.changes.len(), 2);
    assert!(t.path().join("base").exists());
    assert!(!t.path().join("a").exists());
    assert!(!t.path().join("b").exists());
    assert!(!t.git(&["log", "--all", "--format=%s"]).contains("secret"));
    // Independent work done while the old stack is hidden stays in place.
    let current = t.change("alternative", "new approach");
    t.backend.unhide(t.path(), &a, false).unwrap();
    let snapshot = t.backend.open(t.path()).unwrap();
    assert_eq!(snapshot.working_copy, current);
    assert_eq!(
        fs::read_to_string(t.path().join("alternative")).unwrap(),
        "new approach"
    );
    assert!(!t.path().join("a").exists());
    assert!(entry
        .changes
        .iter()
        .all(|c| t.visible().contains(&c.commit_id)));
    assert!(t.git(&["branch", "--all"]).contains("first"));
    assert!(t.git(&["branch", "--all"]).contains("second"));
}

#[test]
fn hiding_a_middle_change_rebases_descendants_and_removes_their_obsolete_refs() {
    let t = TestRepo::new();
    let a = t.change("a", "secret parent");
    t.backend.new_change(t.path(), &a).unwrap();
    let b = t.change("b", "retained child");
    t.backend.create_bookmark(t.path(), "child", &b).unwrap();
    let entry = t.backend.hide(t.path(), &a, false, false).unwrap();
    assert_eq!(entry.changes.len(), 1);
    assert!(!t.path().join("a").exists());
    assert!(t.path().join("b").exists());
    let log = t.git(&["log", "--all", "--format=%s"]);
    assert!(!log.contains("secret parent"));
    assert!(log.contains("retained child"));
    t.backend.unhide(t.path(), &a, false).unwrap();
    assert!(t.visible().contains(&entry.changes[0].commit_id));
    assert!(!t.path().join("a").exists());
    assert!(t.path().join("b").exists());
}

#[test]
fn dry_runs_do_not_snapshot_or_create_archives_and_corrupt_archives_are_not_overwritten() {
    let t = TestRepo::new();
    let id = t.change("idea", "saved proposal");
    let (_, before) = t.backend.load_repo_at_head(t.path()).unwrap();
    fs::write(t.path().join("idea"), "not yet recorded").unwrap();
    t.backend.hide(t.path(), &id, false, true).unwrap();
    let (_, after) = t.backend.load_repo_at_head(t.path()).unwrap();
    assert_eq!(before.op_id(), after.op_id());
    assert!(!t.path().join(".jj/repo/jiji").exists());
    t.backend.hide(t.path(), &id, false, false).unwrap();
    let (_, before) = t.backend.load_repo_at_head(t.path()).unwrap();
    let refs = t.git(&["show-ref"]);
    t.backend.unhide(t.path(), &id, true).unwrap();
    let (_, after) = t.backend.load_repo_at_head(t.path()).unwrap();
    assert_eq!(before.op_id(), after.op_id());
    assert_eq!(refs, t.git(&["show-ref"]));
    fs::write(t.path().join(".jj/repo/jiji/hidden.json"), "broken").unwrap();
    assert!(t
        .backend
        .unhide(t.path(), &id, false)
        .unwrap_err()
        .to_string()
        .contains("could not read"));
    assert_eq!(
        fs::read_to_string(t.path().join(".jj/repo/jiji/hidden.json")).unwrap(),
        "broken"
    );
}

#[test]
fn published_and_custom_git_refs_are_refused_without_hiding() {
    for ref_name in [
        "refs/remotes/origin/idea",
        "refs/archive/idea",
        "refs/tags/idea",
    ] {
        let t = TestRepo::new();
        let id = t.change("idea", "published proposal");
        let (_, repo) = t.backend.load_repo_at_head(t.path()).unwrap();
        let commit = resolve_change_commit(&repo, &id).unwrap();
        t.git(&["update-ref", ref_name, &commit.id().hex()]);
        assert!(
            t.backend.hide(t.path(), &id, false, false).is_err(),
            "{ref_name}"
        );
        assert!(t.visible().contains(&commit.id().hex()));
        assert!(t.backend.hidden(t.path()).unwrap().is_empty());
        assert!(t.path().join("idea").exists());
    }
}

#[test]
fn unhide_refuses_to_overwrite_a_reused_bookmark() {
    let t = TestRepo::new();
    let id = t.change("idea", "old proposal");
    t.backend.create_bookmark(t.path(), "idea", &id).unwrap();
    t.backend.hide(t.path(), &id, false, false).unwrap();
    let other = t.change("other", "replacement proposal");
    t.backend.create_bookmark(t.path(), "idea", &other).unwrap();
    let refs = t.git(&["show-ref"]);
    let err = t.backend.unhide(t.path(), &id, false).unwrap_err();
    assert!(err.to_string().contains("has been reused"));
    assert_eq!(t.git(&["show-ref"]), refs);
    assert_eq!(t.backend.hidden(t.path()).unwrap().len(), 1);
}

#[test]
fn repeating_hide_removes_retention_refs_recreated_by_jj_gc() {
    let t = TestRepo::new();
    let id = t.change("idea", "secret proposal");
    t.backend.hide(t.path(), &id, false, false).unwrap();
    let (_, repo) = t.backend.load_repo_at_head(t.path()).unwrap();
    repo.store()
        .backend()
        .gc(repo.index(), std::time::SystemTime::now())
        .unwrap();
    assert!(t
        .git(&["log", "--all", "--format=%s"])
        .contains("secret proposal"));
    t.backend.hide(t.path(), &id, false, false).unwrap();
    assert!(!t
        .git(&["log", "--all", "--format=%s"])
        .contains("secret proposal"));
    t.backend.unhide(t.path(), &id, false).unwrap();
}

#[test]
fn hidden_prefixes_must_be_unambiguous() {
    let t = TestRepo::new();
    let a = t.change("a", "first proposal");
    t.backend.hide(t.path(), &a, false, false).unwrap();
    let b = t.change("b", "second proposal");
    t.backend.hide(t.path(), &b, false, false).unwrap();
    let path = t.path().join(".jj/repo/jiji/hidden.json");
    let mut archive = read_archive(&path).unwrap();
    archive.entries[0].changes[0].change_id = "z".repeat(32);
    archive.entries[1].changes[0].change_id = format!("z{}", "y".repeat(31));
    assert!(find_entry(&archive, "z")
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
    assert_eq!(find_entry(&archive, "zz").unwrap(), Some(0));
    assert!(t.backend.unhide(t.path(), "nonexistent", false).is_err());
}

#[test]
fn internal_git_repositories_hide_and_restore_without_a_colocated_git_directory() {
    let t = TestRepo::with_colocation(false);
    let id = t.change("idea", "secret proposal");
    let (workspace, repo) = t.backend.load_repo_at_head(t.path()).unwrap();
    let git = HiddenGit::new(&workspace, &repo).unwrap();
    let entry = t.backend.hide(t.path(), &id, false, false).unwrap();
    assert!(!t.path().join(".git").exists());
    assert!(!git
        .run(&["log", "--all", "--format=%s"])
        .unwrap()
        .contains("secret proposal"));
    t.backend.unhide(t.path(), &id, false).unwrap();
    assert!(t.visible().contains(&entry.changes[0].commit_id));
}

#[test]
fn workspaces_share_the_archive_but_cannot_hide_work_used_by_a_peer() {
    use jj_lib::local_working_copy::LocalWorkingCopyFactory;
    use jj_lib::ref_name::WorkspaceName;

    let t = TestRepo::new();
    let id = t.change("idea", "secret proposal");
    let (workspace, repo) = t.backend.load_repo_at_head(t.path()).unwrap();
    let main = resolve_revset_text(&workspace, repo.as_ref(), "main").unwrap();
    let main = commits_in(repo.as_ref(), &main).unwrap()[0]
        .change_id()
        .reverse_hex();
    let peer = tempfile::tempdir().unwrap();
    pollster::block_on(Workspace::init_workspace_with_existing_repo(
        peer.path(),
        workspace.repo_path(),
        &repo,
        &LocalWorkingCopyFactory {},
        WorkspaceName::new("peer").to_owned(),
    ))
    .unwrap();
    t.backend.edit_change(peer.path(), &id).unwrap();
    let error = t.backend.hide(t.path(), &id, false, false).unwrap_err();
    assert!(error.to_string().contains("workspace peer"));
    t.backend.new_change(peer.path(), &main).unwrap();
    let entry = t.backend.hide(t.path(), &id, false, false).unwrap();
    assert_eq!(t.backend.hidden(peer.path()).unwrap()[0].id, entry.id);
    t.backend.unhide(peer.path(), &id, false).unwrap();
    assert!(t.backend.hidden(t.path()).unwrap().is_empty());
    assert!(t.visible().contains(&entry.changes[0].commit_id));
}

#[test]
fn separate_archives_restore_in_ancestry_order() {
    for rewrite_parent in [false, true] {
        let t = TestRepo::new();
        let parent = t.change("parent", "parent proposal");
        t.backend.new_change(t.path(), &parent).unwrap();
        let child = t.change("child", "child proposal");
        t.backend.hide(t.path(), &child, false, false).unwrap();
        if rewrite_parent {
            t.backend
                .describe(t.path(), &parent, "updated parent proposal")
                .unwrap();
        }
        t.backend.hide(t.path(), &parent, false, false).unwrap();
        let error = t.backend.unhide(t.path(), &child, false).unwrap_err();
        assert!(error.to_string().contains("archived ancestor"));
        t.backend.unhide(t.path(), &parent, false).unwrap();
        t.backend.unhide(t.path(), &child, false).unwrap();
        assert!(t.backend.hidden(t.path()).unwrap().is_empty());
    }
}

#[test]
fn an_archived_id_must_not_silently_shadow_a_visible_bookmark() {
    let t = TestRepo::new();
    let old = t.change("old", "old proposal");
    let entry = t.backend.hide(t.path(), &old, false, false).unwrap();
    let current = t.change("current", "current proposal");
    t.backend
        .create_bookmark(t.path(), &entry.id, &current)
        .unwrap();
    let error = t
        .backend
        .hide(t.path(), &entry.id, false, false)
        .unwrap_err();
    assert!(error.to_string().contains("visible work"));
    assert!(t.path().join("current").exists());
    assert_eq!(t.backend.hidden(t.path()).unwrap().len(), 1);
}
