# Jiji CLI

The standalone `jiji` executable and the desktop app share `jiji-core` and
`jiji-forge`. Stack discovery, jjpr branch matching, PR segmentation, status
classification, `trunk()` configuration, publishing, and landing use the same
Rust implementation. The CLI runs without a desktop window.

## Install and run

From a checkout:

```sh
cargo install --path crates/jiji-cli --locked
```

For development:

```sh
cargo run -p jiji-cli -- --help
```

On macOS, the app's command palette can install the `jiji` entry point. An
existing app-only launcher gets an **Update jiji command-line tools** action.
This entry point runs the same CLI embedded in the app binary. Reinstall it
after moving the app to a different path. App updates at the same path keep
the CLI current. The standalone Cargo installation updates independently.

`jiji`, `jiji .`, and `jiji open PATH` open the desktop app on macOS. All other
commands run headlessly. A command name takes precedence over a same-named
directory; use `jiji open ./stacks` to open such a directory.

## Discover and inspect stacks

```sh
jiji stacks
jiji stacks --all
jiji stacks --search '#753'
jiji status
jiji status ws/tu3-emit-new-trips
jiji status ws/tu2-match-new-trips
jiji -R /path/to/repo stacks --json
jiji stacks --offline --all
```

Published stacks lead the list; local work is hidden when published stacks
exist. `--all` includes it, and searches also include local work. `status`
shows every PR in landing order, its checks/reviews, and the next landing
actions. Naming an intermediate bookmark inspects the stack up to that PR.
Remote-only stacks can be inspected. `jiji pr fetch NUMBER` creates a local
review bookmark; publishing and landing use the stack's corresponding local
bookmarks.

A pull request that someone else opened on one of the stack's branches is
shown with the stack and is not part of it. `stacks` adds a line to the
stack's entry ("+ #31 by @sam on auth (someone else's)"), and `status` lists
each one under "On this stack, by someone else", after the landing order,
with the branch it is based on. Such a pull request is also a stack of its
own, on GitHub only, which starts at the branch of yours it is based on.
See [Someone else's pull request on your stack](#someone-elses-pull-request-on-your-stack).

Without a head argument, commands select the current workstream's publishable
stack, or the sole stack if there is no current one. An ambiguous selection
fails and asks for a head; it never chooses another stack arbitrarily.
Publishing and shipping also accept a local change ID. `@` selects the current
stack, excluding an undescribed empty working-copy change above its bookmark.

`-R PATH` works before or after the command and accepts a repository root,
nested directory, or file. `--offline` on `stacks` and `status` reads local
structure without a GitHub request; PR information is unavailable in this mode.

## Hide and recover local work

```sh
jiji hide abcxyz
jiji hide topic --stack
jiji hide @ --dry-run
jiji hidden
jiji unhide abcxyz --dry-run
jiji unhide abcxyz
```

`hide` puts unpublished work aside so a fresh task can start without seeing
the earlier attempt in ordinary history. It accepts a change ID, commit ID,
bookmark, or revset resolving to one visible change; the default is `@`.
`--stack` includes that head's mutable ancestors and stops at immutable
history. Without `--stack`, only the selected change is hidden. Descendants
are rebased without the hidden changes, just as with `jj abandon`; conflicts
are represented using jj's normal conflict handling. Local bookmarks on the
hidden changes are saved and removed. Hiding `@` first snapshots edits, then
replaces the working copy with an empty change on the remaining parent(s).

`unhide` accepts an unambiguous prefix of **any saved change or commit ID**.
It restores the entire group from that hide operation, with its original
change IDs, commit IDs, contents, parents, and bookmarks. It leaves your
current working copy and work done in the meantime in place; use
`jj edit ID` to resume the recovered work. Descendants rebased during hide
remain on their new parents. A reused bookmark or a different visible version
of a saved change causes a refusal rather than overwriting work.

Normal `jj log`, `jj log -r 'all()'`, `git log`, `git log --all`, and branch
listings do not expose the hidden changes. Older versions and obsolete
descendants are also removed from Git's retention refs: plain `jj abandon`
alone would still expose them through `git log --all`, as explained in
[jj's Git command table](https://docs.jj-vcs.dev/latest/git-command-table/).
This is a way to set work aside, **not an access-control boundary**. Explicit
commit lookups, jj's operation/evolution history, Git reflogs, and inspecting
the archive can still reveal it.

Recovery records live in `.jj/repo/jiji/hidden.json` (in the shared repository
for additional jj workspaces). Objects are preserved in Git packs protected
by `.keep` files, without adding archive refs or bookmarks. They survive
Git garbage collection and remain local; cloning or pushing does not back up
the archive. Protected packs are retained after unhide too, because they also
protect old jj operation history whose Git retention refs were removed.
**`jj util gc` can recreate those retention refs**, exposing hidden work in
Git's `--all` view again. Run `jiji hide ID` again after jj garbage collection
to remove the regenerated refs. Recovery remains available throughout.

Immutable changes, remote refs, tags or other Git refs that would expose the
work, and affected changes used by another jj workspace are refused. Backing
Git repositories with linked Git worktrees are currently unsupported. Nothing
is pushed or deleted from remotes. `--dry-run` inspects the last recorded state
without snapshotting edits or creating archive/lock files. All three commands
support `-R PATH` and `--json`, and require no GitHub connection.

## Publish, land, and ship

```sh
jiji publish topic --dry-run
jiji publish topic
jiji publish topic --stack      # also link the PRs as a GitHub stack
jiji publish topic --no-stack   # not this time, whatever the setting says
jiji land topic --dry-run --method squash
jiji land topic --method squash
jiji ship topic --dry-run
jiji ship topic
```

`publish` pushes bookmarks, creates PRs, and updates the stack's PR bases,
managed descriptions, and stack comments. `submit` is an alias. New PRs
open ready for review; `--draft` opens them as drafts instead, so GitHub
requests no reviews (code owners included) until they are marked ready.
Existing PRs keep their state either way.

GitHub closes a PR as merged the moment its branch is inside its base
branch, whatever that base is, and a merged PR cannot be reopened. Two
kinds of push do that to a PR by accident, and `publish` plans around both:

- A reorder (or an inserted or removed segment) pushed with the old bases
  in place does it to the PR that moved down. A PR whose base changes is
  pointed at trunk before anything is pushed and at its new base afterwards.
- Bookmarks that slid along a chain do it without any base changing. Slid
  up, the lower branch is pushed onto the commit the upper PR still has as
  its head. Slid down, the upper branch is pushed onto the commit the lower
  one still is on GitHub. A push of several branches is not atomic, so
  either can happen even when both branches are meant to move. When the PR
  at risk is part of the publish, it is pointed at trunk for the push and
  back afterwards. When it is not, because the publish stops at a lower
  bookmark, the plan is blocked (exit 3) and names the bookmark to publish
  through instead.

"Inside" follows every parent, so a PR whose head a merge brings in as a
second parent counts, and so does a head that is immutable here (a tagged
commit). Where a base branch stands on GitHub is known through that branch's
own PR; a base that has none and that the publish moves is treated as a
risk, and the PR on it is parked. And the question is asked of every open PR
based on a branch of the stack: GitHub's batched answer holds at most the
hundred most recently updated open PRs, and when it was cut short `publish`
asks for the ones based on each branch of the stack before it plans
anything, so a PR of the stack itself that the list left out is still known
as that segment's. (A PR based on trunk that the list left out is not found
this way. A publish cannot close it; it may try to open it a second time,
which GitHub refuses.)

Two things stop a publish outright, because parking cannot help:

- A base branch that reads in sync here, and that GitHub has somewhere
  else than it is here. Something pushed to it since the last fetch, to
  commits that are not here, and whether a branch pushed against it would
  end up inside it cannot be told. Fetch, look, and publish again. (A base
  that has moved here is not compared this way. If it has also moved on
  GitHub since the last fetch, its push is refused, and the PR on it was
  not parked for that. Nor is a base that has no PR of its own: where it
  stands on GitHub is known here only as of the last fetch, and a branch
  pushed back onto a commit that it has gained since is closed by GitHub
  as merged into it. Fetch before publishing a branch onto an older
  commit.)
- A GitHub stack that would have to be dissolved while the open-PR list
  was cut short and does not show all of it. The lookups above go by base,
  so the stack's bottom PR (based on trunk) and members past the published
  head can be missing; a stack rebuilt from what is seen would lose them,
  or be dissolved over a PR the plan takes for unpublished. `--no-stack` is
  held back by this too: every member leaves the stack with it, and one
  the list does not show may be someone else's, which the plan could not
  name. A comment on the PRs that are not seen brings them into the list.

If a publish stops after pointing a PR at trunk and before pointing it back,
the failed step says which PR is left there. Publishing again finishes it;
until then that PR shows everything below it as well and should not be
merged.

`land` executes one round: merge the next PR, enable GitHub auto-merge, or join
the merge queue as appropriate, then reconcile PRs that have merged. Run it
again after a GitHub hand-off finishes, or use `watch` to supervise subsequent
rounds. Draft PRs are marked ready as part of landing. Required conditions and
the PR head checks are enforced by the shared engine.

A merge counts as a landing only once the repository agrees. After the
fetch that follows a merge, and before any rebase, push, bookmark deletion
or abandon, `land` runs "Check that trunk holds NAME (#N)": the commit
GitHub names as the PR's merge must be part of trunk here, and must hold
the segment's changes as they are here. A PR that GitHub closed as merged
into another branch, a merge that lacks some of the changes (it names the
files), or a merge commit the fetch did not bring stops the run at that
step, with nothing rewritten. When the repository already knows the answer,
the plan is blocked with the same reason (exit 3) and `status` shows it.

A PR's merge vouches only for the commits it was merged from. When the PR
was merged from another commit than the bookmark has here (the branch name
had an earlier PR, or the changes were edited after the merge), its merge
says nothing about this work, and trunk's tip is asked instead: the changes
have to be there now. So new work under a reused branch name is never taken
as landed because an old PR once carried the same patch, even one that was
reverted since. Publishing again opens a PR for the work. The changes are
measured from the commit below the segment in the stack, so a segment that
merges a side branch in is checked for that branch's content too.

Commits that are themselves part of trunk's history (a merge commit landed
them, or trunk was moved onto the branch) are judged where they entered
trunk's line: that commit has to hold everything it brought in, from where
the branch left trunk up to its head, not the last commit's change alone.
(Where trunk and the branch had merged each other before, the branch left
trunk in more than one place, and one tree has to hold what came in from
each of them: that commit, or else trunk as it is now. Part at the one and
part at the other is not a landing.)
Being in the history is not enough. A merge can take a branch in and keep
none of its content, or part of it, and GitHub marks that PR merged as
well; `land` then says so and leaves the bookmark, which is what still
names the work.

Trunk, for all of this, is the branch the plan names, as the remote has it
after the fetch. `trunk()` is not asked again while a plan runs. It can be
ahead of the remote: with no `trunk()` of your own, Jiji takes the latest
of the remote's branch and the local bookmark, so a commit made on the
local `main` and not pushed becomes `trunk()`. And a fetch can change what
it names: the same default takes the latest of `main`, `master` and
`trunk`, so another of those moving ahead on the remote would otherwise
switch the destination in the middle of a run. Landings are judged against
the plan's branch on the remote, the rest of the stack is rebased onto it,
and a run stops if the remote has no such branch here.

Landing a stack one PR at a time has to end where landing its tip in one go
would. It always does while trunk is where the stack was based, and nearly
always when trunk has moved. It does not when trunk made a change that a
lower PR makes too and a higher PR changes again: one merge of the tip
applies the stack's net change and leaves trunk's version alone, while the
PRs in turn replace it. (Seen on GitHub: a flag that trunk had turned on,
that the lower PR turned on and the upper PR turned off again, ended up off
PR by PR and on with the tip.) So before `land` merges, enqueues or arms the
first PR of a stack that has more above it, it works out both results and
compares them. When they differ the plan is blocked (exit 3) and names the
files; nothing is merged. Rebase the stack onto trunk and publish: the PRs
then show exactly what lands, and the two ways are the same again. Naming a
lower bookmark as the head lands that PR by itself, which is its own tip.

The same is asked after a landing, before `land` rebases what is left of the
stack onto trunk, which is where one change at a time is applied to it. By
then the bottom of the stack has landed, whoever merged it, and trunk is as
the run's fetch just left it. When what is left (two changes or more, up to
the head that was named) would not end where its tip would, the run stops at
that step, with that part of the stack not rebased and nothing pushed, and
names the files. Rebase it onto trunk yourself, look at what it does to
those files, publish, and land again. (A change that the rebase leaves empty, because trunk already has
what it did, has nothing left to land, and `publish` does not push an empty
segment: `land` says so, and asks for its PR to be closed and the change
abandoned.)

That comparison is a guard with limits. It is made here, with jj's merge and
not GitHub's. The one before a first merge is against trunk as the last
fetch left it: a change that reached trunk on GitHub since is not seen until
the rebase after that merge asks again. That second one is about what is
left, measured from the PR that landed, so it cannot see everything the
first would have: when trunk had made, by itself, a change that the landed
PR makes too, and a PR further up changes it again, the two look like one
change by then, and the later PR's version lands. Only the comparison
before the first merge tells them apart, and only when trunk's change had
been fetched. With trunk moved since a stack was started, fetch, rebase and
publish before landing the first of it.

Neither comparison is made when the plan lands with the rebase method
(GitHub replays every commit either way). A plan knows its method when it
was given (`--method rebase`) or when it merges, enqueues or arms a PR
itself; a landing that is only resumed, under the automatic method, is
compared. The one before a first merge is not made when GitHub's own
automation is already about to merge the PR. And there are stacks neither of the two
judges, which land as they always did: one with a merge in it, one of more
than 500 commits, and one that sits on something that is neither part of
trunk's history nor content trunk already holds. In a GitHub stack, GitHub
rebases what is left after a merge itself, before `land` can ask.

What follows the check uses the same commits. The rest of the stack is moved
onto that trunk by replacing the parent that landed and no other: a change
that merges a side branch in keeps the branch, and everything it brings.
Cleanup deletes a landed bookmark only while it still points at the commit
that was checked, and abandons exactly the commits that were checked. A
change edited while the run was going, or a branch someone pushed to after
the merge, has another commit by then: it is left in place and the step says
so. Where the repository deletes head branches on merge, GitHub removes the
landed branch itself a moment after the merge, usually after the run's
fetch: the push of the deletion is then refused, `land` fetches once more,
and with the branch gone from the remote the step is done ("GitHub had
deleted its branch already"). A change started on a landed one while the run
was going is moved onto trunk before its parent is abandoned, so its tree
keeps what landed. One started in the instant after that look stops the
sweep: nothing is abandoned while a change that is not part of the landing
sits on the landed commits, and the step names it. What is left is then done
by hand. The landed bookmark is deleted before its changes are swept, so no
later `land` can be pointed at those commits: move the change that sits on
them onto trunk, and abandon them (their content is on trunk). The step says
so. (All of this holds as far as Jiji's own lock reaches. A `jj` command in
a terminal that commits in the very same moment as the sweep is merged with
it by jj afterwards, as any two commands at once are.)

One case takes two runs. When a branch got commits on GitHub that were not
here ("Update branch") and was then merged there, the first `land` checks
and sweeps the commits that were here, and leaves the bookmark, which its
fetch moved onto GitHub's commit; the step says so. The next `land` plans
from that commit and removes the rest. `watch` rarely meets this: when it
sees the updated branch before the merge, it fetches and lands that branch
itself, in one round. When both happened between two of its looks, it ends
after the first of the two runs, with the bookmark left as described.

A change that carries a jj conflict is not merged, enqueued or armed for
auto-merge. jj does not push one, but a branch pushed some other way can
hold one, and to GitHub the conflict's sides are ordinary files.

`land` also refuses to merge a PR whose branch on GitHub is not the commit
the bookmark has here. A bookmark can read in sync and still be behind
GitHub when something pushed to the branch since the last fetch. `land`
says to fetch and look. The same holds at the end of a round, where the
next PR up is handed to GitHub's auto-merge: the run's own fetch can bring
another commit under that PR's bookmark (someone pushed to the branch, or
replaced it, while the PR below merged), and auto-merge is armed only when
the bookmark still holds the changes the plan listed for it and no others.
Otherwise the step fails and says what is there now.

`watch`, which nobody is watching, fetches once itself and lands what is
there if the bookmark followed; the round's first step then says that a
branch had moved. If the fetch shows that the branch was rewritten or cut
back instead (a change that was in the stack is no longer under its head,
whether jj dropped it or another bookmark still holds it), `watch` ends
there and says so, rather than land what is left. It ends the same way when
a look finds a branch that is still to land shorter than its last look
left it, whoever fetched, and when a job that was stopped is resumed
against such a stack: the job is saved with what it was asked to land.
Squashing or abandoning a change of a watched stack yourself ends the job
too; queue it again. So does taking a whole branch out of the stack (moving
what is above it onto trunk, say) unless its PR has landed: the job was
asked to land that branch as well, and does not report "landed" without it.
Landed means the PR the job recorded for the branch, merged into trunk; an
earlier PR that once merged from a branch of the same name does not count,
nor does that PR merged into some other branch.

A bookmark whose PR landed as a merge commit, and was not cleaned up (the
run stopped after its merge, or someone merged on github.com and a fetch
followed), sits on commits that are trunk history. `land` on the stack above
it, while that stack's bottom PR still targets its branch, checks it, points
that PR at trunk, rebases the stack, and removes the bookmark. `land` on the
bookmark by name checks it and removes it, and nothing else.

Neither touches a branch that other open PRs are based on. A long-lived
branch that was merged into trunk with a merge commit looks just like such
a leftover, and deleting a branch on GitHub moves or closes the PRs based on
it. With other PRs based on it, the stack's plan leaves the branch alone
(and reports, as before, that the bottom PR targets a branch that is not
trunk), and the by-name plan is blocked and names them.

With nothing based on it, `land` cannot tell a long-lived branch from a
landed one: `land develop`, after `develop` was merged into trunk and has
not moved since, removes `develop` here and on GitHub like any landed
branch. Do not name such a branch. GitHub refuses the delete for a
protected branch, and `--dry-run` shows the plan first.

`--method auto|squash|rebase|merge` chooses the landing method. The default is
automatic, using the methods allowed by the repository. `--bypass` explicitly
requests direct merges past checks, reviews, and merge queues, where GitHub
permits the account to do so. Merge conflicts and invalid bases still block.

`ship` sends local work directly to trunk without opening a PR. Execution
fetches first and derives a fresh plan. It also works with a non-GitHub git
remote, and without a GitHub token. The plan then knows of no pull requests
and says so: GitHub still marks merged one whose commits the push puts on
its base (as a rule the one based on trunk), and the PRs above it stay
open. With a token, a read of GitHub that fails stops the ship, here and in
the app: a ship that could not see the PRs would leave them open without a
word.

When the shipped work has open PRs, `ship` finishes them too: after it,
each is merged on GitHub and none is left open. GitHub marks a PR merged
when its head is inside its base branch, and only then. Pushed to trunk as
it stands, a stack's bottom PR is merged and every PR above it, based on
the branch below, stays open; afterwards GitHub refuses to point those at
trunk ("no new commits"). So the plan does it in this order, and names each
PR by number before anything runs (`--dry-run` and `--json` show the same):

- "Push BRANCHES, so each pull request has the commit that ships", when the
  work has to be rebased onto trunk first or a bookmark is ahead of its
  branch. A PR whose head on GitHub is another commit would not be merged
  by the push.
- "Retarget #N (BRANCH) to trunk, so the push merges it", for every PR of
  the work that is based on another branch.
- The push to trunk.
- "Check that GitHub marked #N and #M merged": it does so a few seconds
  after the push, and `ship` waits up to half a minute and reports what it
  found for each. One GitHub has not marked by then is told as still open;
  the work is shipped either way.
- "Retarget #N to trunk; it stays open", for a PR that sits on the shipped
  work without being part of it: the next one up in your own stack when you
  ship only the lower part, or one that someone else opened on it. These
  are not marked merged. Their base branch has nothing left that trunk
  lacks, so each is pointed at trunk. It shows only its own changes when
  trunk got the very commits its branch is built on. Your own PR above
  work that was rebased for the ship shows the replaced commits as well
  until you publish it again; for someone else's, such a ship is refused
  (below).

There is one push, and it either moved trunk or did not. When it is
refused (a protected trunk, or trunk moved on the remote since the fetch),
every PR that was pointed at trunk is pointed back at the base it had, and
the failed step says so PR by PR, and names any that could not be put back.
Branches pushed in the first step stay pushed: each PR then shows the
commits that would have shipped. Fetch, and ship again.

A push can also report an error and have arrived. So before anything is put
back, `ship` asks GitHub where trunk is. With trunk at the shipped commit
the run goes on as if the push had answered. When GitHub cannot be asked,
nothing is touched, and the step says how each PR was left (pointing them
back after a push that did arrive would leave them open with their commits
on trunk). A step that fails before the push is undone with the ones before
it: GitHub can apply a retarget, or take part of a stack apart, and the
answer be an error. Two things are not looked at. A base that someone
changed by hand while the ship ran: the way back is the base the plan saw.
And a trunk that someone pushed to again in the moment after a push of
yours that arrived with an error: `ship` compares trunk with the commit it
shipped, reads that as not arrived, and points the PRs back, though their
commits are on trunk. GitHub then marks merged the one based on trunk; the
ones above it stay open with nothing left to merge, and are yours to
close.

A PR in a GitHub stack cannot be retargeted, and GitHub does not finish a
shipped stack by itself: the bottom PR is marked merged and the rest stay
open, still in the stack and still based on the branch below (seen on
GitHub). So `ship` dissolves the GitHub stack first, as the first of these
steps, and links it again if the push is refused. It does that only when
every open PR of the stack ships. When the stack has a PR that does not (the
upper part of your own stack, or one that someone else added on top), the
plan is blocked and names it: `land` takes a GitHub stack as it is, and
`publish --no-stack` dissolves one on purpose, after which the lower part
ships as a plain chain. That holds also when only the stack's bottom PR
ships and nothing would have to be retargeted. The PR above it would stay
based on the shipped branch, where GitHub neither moves it nor lets anyone
else, and that branch could then not be deleted without GitHub closing the
PR (seen on GitHub: the previous version shipped such a bottom, and its
next `land` closed the PR above).

A ship that has PRs is also blocked, with the reason, in these cases:

- A PR's branch is at another commit on GitHub than its bookmark is here
  although the bookmark reads in sync: fetch, and look at what arrived.
- GitHub answered only its 100 most recently updated open PRs and the work
  has branches on GitHub. `ship` cannot tell which PRs are open for the
  work or rest on it, and does not go by a list it cannot call complete.
- Someone else's PR rests on the work and the ship would replace the
  commits under it: the work has to be rebased onto trunk first, or the
  branch it rests on was rewritten here since it was last pushed, or a
  change under it was rewritten after their branch was built on it (known
  once a fetch has brought their branch). Their PR ends showing only its
  own changes when trunk gets the very commits their branch is built on,
  and nothing here pushes to their branch to make that so otherwise. Ask
  its author to rebase onto the new commits and ship then, or `land` the
  stack, which goes through GitHub and says what their PR shows
  afterwards. A rewrite further up, above the branch their PR rests on,
  replaces nothing under it and does not stop the ship.
- A PR that is not one of the work's has its head among the commits that
  ship: a bookmark was moved onto its commits, or a merge in the work
  brings in the head of someone else's PR that is based on trunk or on a
  branch that ships. The first push that holds that head would make GitHub
  mark it merged, and a push cannot be taken back. (A branch of your own
  that you merged in ships with the work, as before.)

### Someone else's pull request on your stack

Someone else is anyone but this clone: a teammate, or you from another
machine or from github.com. What makes a pull request theirs is that its
branch is not a bookmark here. It is based on one of your stack's branches
(the top one, or one in the middle), or on another such PR, or was added to
your GitHub stack above your own. A PR from a fork is theirs whatever its
branch is called: it is never matched to a bookmark here by name. Track
their branch here (`jj bookmark track NAME@REMOTE`) and it is a bookmark of
yours like any other, and its PR a segment of the stack, to publish and to
land.

jj calls everything an untracked remote bookmark contains immutable. So
once a fetch has brought their branch, the changes of yours under it are
immutable too: `jj` and the app refuse to rewrite them by hand, until that
branch is tracked here or has moved off them. They are still your stack.
`stacks`, `status`, `publish`, `land`, `watch` and `ship` go on treating
them as such, and `status` and the app's change header say why they read as
immutable.

Their PR is never a segment of your stack: nothing here publishes it,
merges it or counts it. What your own commands do under it is planned for
it as steps of its own, and each plan says so by number and author before
anything runs.

`publish` never touches their branch.

- A push that only adds commits on top of the branch they are based on
  changes nothing for them.
- A push that replaces commits of that branch (an amended change, a
  reorder, a bookmark moved back) leaves their PR showing the replaced
  commits as its own, until its branch is rebased. The plan warns, naming
  the PR. Rebasing it is its author's to do: nothing here pushes to their
  branch.
- A bookmark of yours moved onto their commits would, pushed, put their
  PR's head inside its own base, and GitHub would close it as merged. The
  plan is blocked (exit 3) and says whose PR that is.
- After you rewrote a change they had built on, a fetch brings the old
  commit back with their branch, and jj shows the change as divergent. That
  copy is theirs to keep and does not block the publish of yours. A
  divergence between two commits of your own still does.
- When their PR is in a GitHub stack with yours, a change of shape
  dissolves the stack and links it again with their PR back on top, and
  `--no-stack` takes it out with the rest. Both are said in the plan, and
  the dissolve step lists every PR that leaves the stack. GitHub dissolves
  a stack as a whole, by its number: a PR that someone adds to it in the
  moment between that plan and the dissolve itself leaves it with the
  rest, unnamed. (The app plans again when you confirm, and stops when the
  members are no longer the ones it showed you.)

`land` never merges their PR and never pushes to their branch. It does one
thing for it:

- "Retarget #N by @LOGIN (BRANCH) to trunk: its base lands and is deleted",
  before anything is rewritten. GitHub closes every open PR based on a
  branch the moment the branch is deleted, merged or not; a PR it closed
  that way cannot be retargeted, and comes back only once the branch is
  pushed again (seen on GitHub). The step looks at their PR first and
  leaves it when it was closed, when GitHub already moved it, and when its
  author has pointed it at another branch since the plan was made.
- What their PR shows afterwards is said in the plan and by the step. A PR
  shows what its branch has that its base does not, by commit, and their
  branch keeps your commits as they were when it was built. After a squash
  or a rebase landing those are not the commits on trunk: their PR shows
  them as its own until its author rebases the branch ("It shows the landed
  commits its branch still carries as its own until its author rebases
  BRANCH"). After a merge-commit landing they are the very commits, and it
  shows what it showed before. Whatever the method, what is left of your
  stack above a landed branch is rebased onto trunk and pushed, and a PR of
  theirs on one of those branches shows the replaced commits in the same
  way.
- A PR in the GitHub stack your landing is part of gets no step: GitHub
  moves and rebases what is left of a stack itself. A GitHub stack of
  someone else's that starts on a branch this run lands is another matter.
  GitHub lets no one else change a base inside a stack, so its bottom PR
  could not be pointed at trunk, and your branch could not be deleted
  without closing it. The plan is blocked before anything merges, and
  names the PRs and the stack: their author moves or dissolves it first.
- Before `land` deletes a landed branch it asks GitHub which open PRs are
  based on it, whatever the plan knew. While there is one, the branch is
  left in place, here and on GitHub, with its changes, and the step names
  the PR: point it at another branch and land again. This holds for your
  own PRs too. The one that needs it is the next PR of a GitHub stack whose
  bottom reached trunk by a push and not through a merge on GitHub: GitHub
  leaves that PR based on the landed branch, and changes no base in a stack
  for anyone else. The step then says to dissolve the stack
  (`publish --no-stack`, which points the PR at trunk) and land again.
  While a landed branch is kept like this, every later `land` of the stack
  reconciles it again before anything else.
- `land` and `watch` never arm auto-merge on their PR and never count it
  among the stack's rounds.

After the stack has landed, their PR is open and targets trunk (or the
branch of yours that is left below it). It shows only its own changes once
its author has rebased it onto trunk; until then it shows, as well, every
commit of yours that a landing or a rebase replaced under it (seen on
GitHub: two and three commits after squash landings, one commit and one
file after its author rebased).

`ship` leaves their PR open and points it at trunk after the push. It is
not marked merged, and its branch is not touched: the commits it sits on
are the ones that shipped. A ship that would replace those commits first is
refused (see `ship` above).

Limits.

- Their PR shows more than its own changes from the moment commits under
  it are replaced (a publish of a rewrite, a squash or rebase landing, the
  rebase of what is left of your stack after any landing) until its author
  rebases its branch. Each plan that does it says so by number and author;
  nothing here rebases it.
- GitHub has no way to delete a branch only if no PR is based on it. A PR
  opened on a landed branch in the moment between `land`'s question and its
  delete is closed with the branch.
- When you build your own work on top of their commits, those commits are
  below your bookmark and count as someone else's base, not as part of your
  stack: the PR you publish for that work targets trunk, as before. Changes
  of your own further down, under their commits, read as that base too, and
  a stack of yours there is not listed while their branch is not a bookmark
  here.
- A change is told apart as yours ("held") only where jj makes it immutable
  through `untracked_remote_bookmarks()` called plainly, as its default
  `immutable_heads()` does. A change that a tag or your own
  `immutable_heads()` makes immutable stays immutable whoever's branch also
  rests on it. Where their branches cannot be told from your own reasons,
  nothing is taken apart: everything `immutable_heads()` names stays
  immutable, as it is for jj, and the stack under their branch reads as
  landed history. `stacks` does not list it, and `land` answers that its
  bookmark points at immutable history. That is so for an
  `immutable_heads()` that names remote branches some other way
  (`remote_bookmarks()`), for one that calls `untracked_remote_bookmarks`
  with an argument (a remote, a name pattern), and for a configuration
  that defines a helper named `untracked_remote_bookmarks()` itself. Track
  their branch, or wait until it has moved off your changes, and the stack
  is yours to land again.
- The branch of a PR from a fork is never here. `ship` refuses for such a
  PR while a branch under it is rewritten here and not yet published. Once
  the rewrite is published (`publish` says then that it replaces commits
  under their PR), nothing here can tell that their branch is still built
  on the replaced commits, and `ship` goes ahead: their PR shows those
  commits before the ship and after it, until its author rebases.

### GitHub stacks

GitHub has its own stacked pull requests: a chain of PRs it knows as one
stack, shows with a stack map, and merges together. Jiji publishes a plain
chain unless asked:

```sh
jiji publish topic --stack      # link the stack's PRs as a GitHub stack
jiji publish topic              # afterwards: keeps the GitHub stack in step
jiji publish topic --no-stack   # dissolve it; the PRs stay, as a plain chain
```

Linking can also be the rule. The app's Settings have "Link new stacks as
GitHub stacks", off unless turned on, and `jiji` follows the same setting:
with it on, a publish that opens a PR for a chain that is not a GitHub
stack links the chain, without `--stack`. A plain chain that is only
updated stays plain, however often it is published, so a chain from before
the setting, or one dissolved on purpose, is not linked behind your back;
it is linked when it next gains a PR. `--stack` and `--no-stack` are this
publish's own choice and win over the setting in both directions. Where the
repository or its host has no GitHub stacks, a publish under the setting
goes through and its last step says that the PRs stay a plain chain; it
does not fail. The setting is kept in the app's preferences file
(`prefs.json` in the app's data directory, key `publish.linkNewStacks`),
which the command reads and never writes.

`--stack` needs at least two PRs. Once a chain is a GitHub stack, whether
Jiji linked it or someone accepted GitHub's banner on github.com, every
later `publish` reads that from GitHub and maintains it with no flag: PRs
for new segments on top are added to the stack, and a change of shape (a
reorder, an insertion, a removed segment) dissolves the stack first and
links it again at the end, because GitHub cannot reorder a stack or change
the base of a PR inside one. PRs of the stack above the published head that
still sit on it go back into the new stack, on top; a member the new stack
cannot hold is named in a warning before anything runs. If such a publish
stops after dissolving the stack, the failed step says so: the PRs are a
plain chain until a publish with `--stack` goes through. GitHub does not let
a PR that is queued to merge leave its stack; a dissolve that leaves one
behind stops the publish there.
`stacks` and `status` show "GitHub stack N".

Landing a GitHub stack differs in what GitHub allows:

- A merge takes every open PR below the one asked for, in one request, all
  or none. So a round merges every PR that is ready from the bottom up
  ("Merge #12, #13 and #14 together"), and a PR that is not ready ends the
  run there. Name a lower bookmark as the head to land only part. Within
  that request GitHub lands the PRs in turn, each one's own change on top
  of the one before (seen on GitHub), so the comparison with the tip is
  made for a GitHub stack exactly as for a plain chain. GitHub's
  lease in that request covers the head of the PR asked for; Jiji re-reads
  the ones below just before, and the check after the merge says so when
  one of them landed from another commit than the one here.
- That request trusts GitHub to still have the stack. If someone dissolves
  the stack on github.com in the instant between Jiji's last look and the
  request, GitHub merges the PR asked for into the branch below it, not into
  trunk; the check then stops the run with nothing touched here, and the
  content lands when the PR below does. Jiji looks once more right before
  the request to keep that instant short. GitHub offers no way to close it
  short of merging one PR per request, which would make a stack as slow to
  land as a plain chain.
- A stack lands on its own base. `land` refuses a PR whose GitHub stack
  lands on another branch than trunk.
- GitHub then rebases the remaining PRs onto trunk and pushes their
  branches itself. `land` waits for that, fetches, and finds its own rebase
  and push already done. GitHub's rebase keeps jj's change ids, so a
  bookmark that was in sync simply moves to GitHub's commits. One that was
  not would end up twice: the copy here and GitHub's rebase of the old one.
  So `land` does not merge below a bookmark of the same GitHub stack that
  differs from its branch on GitHub; it says to publish first. When the
  merge was made elsewhere and that can no longer be prevented, the plan
  warns, and the run stops after the check if the fetch did bring a second
  copy, before it rewrites anything.
- When the rebase would conflict, GitHub leaves the branch and marks the PR
  as conflicting. `land` then rebases here, where the conflict can be
  resolved. GitHub also reads as conflicting for a moment on its way to a
  rebase that succeeds, so `land` takes the answer only once it has stood
  for ten seconds. When GitHub has done neither by the end of the wait, a
  run that just made the merge stops ("land again in a moment") rather than
  rebase across GitHub's own; a later run that still finds the branch
  unmoved rebases here. The ten seconds are a rule of thumb: a rebase from
  GitHub that arrives later than that can cross the one made here. The push
  from here is then refused, or the change is here twice after the next
  fetch, and `land` stops and names the copies. Nothing is lost.
- GitHub moves the rest of a stack only after a merge made through GitHub.
  When a stack's bottom reached trunk by a push (`jj git push` of trunk, or
  a `ship` by an earlier version), the PR above stays based on the landed
  branch (seen on GitHub). `land` then finds nothing to merge below it,
  keeps the landed branch while that PR is based on it, and says to
  dissolve the stack: `publish --no-stack` points the PR at trunk, and the
  next `land` merges it.
- There is no auto-merge on stacked PRs. A PR still waiting on checks or a
  review is reported as waiting (exit 3 for `land`); `watch` sits through it.
- `--bypass` merges the bottom PR alone. GitHub allows a bypass there only.
- On a trunk protected by a merge queue, the PRs are added to the queue
  through the same endpoint and GitHub lands them from there. An answer of
  "enqueued" to a merge request (GitHub gives it for a PR that is already
  in a queue) is reported as that, not as a merge.
- With squash, the message Jiji composes goes on the top PR of the request.
  GitHub writes the commits for the PRs below it, from each PR's title and
  commits.

`ready`, `draft`, `status`, `watch`, `--json` and `--dry-run` work the same
on a GitHub stack. On a host or repository without GitHub stacks, `--stack`
fails at its last step and says so; everything else is unaffected, and a
link made only because of the setting is left out without failing.
`ship` dissolves a GitHub stack it ships whole, and refuses one it would
ship in part (see `ship` above).

Not yet run against GitHub: `--bypass` on a GitHub stack and landing a
GitHub stack onto a queue-protected trunk are written to GitHub's
documentation of its asynchronous merge endpoint, and no repository with
repository rules or a merge queue was available to run them on. The same
holds for the "enqueued" answer, for a dissolve that leaves a queued PR
behind, for a merge request that finds another one already pending, and
for the refusal on a host without GitHub stacks. Everything else in this
section was run against github.com.

Write commands execute by default. Each prints its concrete plan before
execution, and reports completed, failed, and skipped steps afterward. Use
`--dry-run` to inspect a plan without executing it. Dry runs do not fetch,
snapshot working-copy edits, create lock/record files, push, or write to GitHub.
They inspect jj's last recorded local state. A ship dry run uses the last
fetched trunk; actual execution fetches and replans. Blocked plans never reach
the executor. Normal write commands first refresh local working-copy state.

## Review readiness

```sh
jiji ready topic --dry-run
jiji ready topic
jiji ready ws/tu2            # only the PRs up to and including ws/tu2
jiji draft topic
```

`ready` marks the stack's draft PRs ready for review; `draft` converts its
open PRs back to drafts. Nothing else changes: no fetch, push, retarget,
description, or comment update, and no local repository change. PRs
already in the requested state are listed and left alone, and segments
without a PR are noted. A stack with no open PRs is blocked (exit 3).
Execution re-reads each PR first; one that closed since the plan was read
fails its step and the rest still run. Remote-only stacks work too.

Landing still marks drafts ready on its own, so `ready` is only needed to
ask for review before landing.

## Auto-land and resume

```sh
jiji watch topic
jiji watch topic --resume
jiji watch topic --resume --no-bypass --method auto
jiji watch topic --dry-run
jiji jobs
```

`watch` (alias `auto-land`) runs the shared supervised landing loop in the
foreground, independently of whether the desktop app is open. It keeps going
through checks, reviews, and GitHub merge hand-offs. Ctrl-C or SIGTERM stops at
the next safe point on macOS/Linux; Enter also stops an interactive session.
An in-flight round finishes before stopping. Closed stdin in a pipeline does
not stop the job.

Progress and landing options are saved to `.jj/jiji/cli-autoland.json` inside
the repository. `--state-file PATH` overrides this location for both `watch`
and `jobs`. These records use the shared auto-land format, but are separate
from the desktop's job store. `jobs` reports the last saved state, not proof
that a process is still running.

`--resume` restores that stack's progress and options after a stop, crash, or
restart, then checks fresh repository and GitHub facts. Saved bypass is shown
in the output; `--no-bypass` disables it and `--method auto` restores automatic
method selection. A missing or corrupt resume record is reported, not replaced
silently. A completed job cannot be resumed. Without `--resume`, a new watch
starts with default options unless overridden explicitly.

OS locks prevent overlapping CLI writers in the same repository and concurrent
CLI writers to a shared job file. These locks do not prevent independent jj,
git, or desktop operations; the engine's current-state and PR-head checks still
apply. Lock ownership is released if the CLI exits or crashes.

## Review and authentication

```sh
jiji pr list
jiji pr show 753
jiji pr fetch 753 --bookmark review/predictions
jiji pr rerun 753 --dry-run
jiji pr rerun 753
jiji auth status
jiji auth login --with-token < /path/to/token-file
jiji auth logout
```

PR fetch creates a new local bookmark using the base repository's PR ref, so
fork PRs work too. Existing local bookmark names are refused. CI reruns target
failed GitHub Actions jobs on the PR's current head; refusals return a nonzero
exit code. Both operations support `--dry-run`.

Authentication uses the app's existing resolution chain: system keychain,
environment token (`GITHUB_TOKEN` / `GH_TOKEN`), then the `gh` CLI. Tokens are accepted on stdin for login and
are never printed. Logout removes only Jiji's keychain entry; environment and
`gh` credentials remain managed by their respective tools.

## JSON and exit codes

`--json` produces a single JSON value for ordinary commands. Plan commands
return `{ "plan": ..., "outcome": ... }`; a dry run or blocked plan has a null
outcome. A land plan's actions include `checkLanded` (the trunk check behind
each merge) and, on a GitHub stack, `awaitRestack`; a `mergePr` action lists
the PRs that land with it in `with` and the stack's number in `stack`. The
steps that rewrite name the commits they are about: `rebaseOntoTrunk` has
`off` (the landed parents trunk replaces), `cleanupBookmark` has `commit`,
`abandonLanded` has `commits`. The one step for someone else's PR on the
stack is `retargetOther` (`number`, `author`, `branch`, `fromBase`,
`toBase`). A blocker has `message`, `wait`
(waiting alone can clear it) and `fetch` (a branch moved on GitHub since the
last fetch). A publish plan carries its `options` and may include `unstack`
and `linkStack` actions; `unstack` lists in `members` the open PRs that
leave the stack with it, bottom first, and `linkStack` lists in `above` the
PRs above the published head that go back into the stack. Under the setting that links
new stacks, `options.stack` reads `linkNew` for a publish that was given
neither flag. A ship plan's actions for PRs are `pushBranches`, `unstack`,
`retargetPr`, `confirmMerged` (`numbers`) and `retargetAbove` (`theirs` says
whether the PR is someone else's). A PR has `author` when GitHub names one,
and in a GitHub stack a `stack` object (`number`, `position`, `size`,
`base`). A stack has `githubStack` (`number`, `linked`) and `others`, the
PRs on it that are someone else's; a change someone else's branch holds has
`held`. Stack listing returns the repository
path, trunk, stacks, hidden local count, and any truncation warnings. `watch --json` emits newline-delimited
`{ "event": "autoLand", "state": ... }` records as its state changes.

Errors are written to stderr. With `--json`, errors have the shape
`{ "error": { "code": "...", "message": "..." } }`. No human progress text
is mixed into JSON stdout.

| Exit | Meaning |
| --- | --- |
| 0 | Successful command or successful dry run |
| 1 | Repository, authentication, network, persistence, or execution failure |
| 2 | Invalid arguments or ambiguous/missing head selection |
| 3 | A plan is blocked (`watch --dry-run` allows conditions it can wait through) |
| 130 | Auto-land stopped at the user's request |

`status` is an inspection command: a successful inspection returns 0 even when
its displayed landing plan is blocked. `land --dry-run` returns 3 for such a
plan, making it useful for scripting readiness checks.
