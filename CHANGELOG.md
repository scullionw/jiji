# Changelog

## Unreleased

Landing a stack is now checked, and GitHub's own stacked pull requests are
supported.

- Fixed: landing could drop a change after a reorder. GitHub closes a
  pull request as merged the moment its branch is inside its base branch,
  and a reordered stack pushed with the old bases in place does that to the
  pull request that moved down. Publish could lose that race, and Land then
  took "merged" at its word, deleted the bookmark and abandoned the change,
  so trunk ended up without it. Publish now points every pull request whose
  base changes at trunk before it pushes and at its new base afterwards.
- Fixed: the same could happen with no base changing. When a chain's
  bookmarks each moved up a commit, publishing only the lower one pushed it
  onto the commit the upper pull request still had as its head, and GitHub
  closed that pull request as merged into the lower branch. Publish now
  refuses such a partial publish and names the bookmark to publish through;
  publishing the whole chain points the upper pull request at trunk for the
  push and back afterwards. The same is done for bookmarks that moved down,
  for a head that a merge brings in as a second parent (an immutable one
  included), for a base branch that has no pull request of its own, and for
  pull requests GitHub's batched answer left out (it holds a hundred at
  most; Publish now asks for the ones based on each branch of the stack
  before it plans anything). Publish stops when a base branch that reads
  in sync here has moved on GitHub since the last fetch, and when a GitHub
  stack would have to be dissolved while that list does not show all of it.
- Land no longer takes GitHub's word that a merge landed. Before it rebases,
  pushes, deletes or abandons anything on the strength of a merge, it checks
  that the commit GitHub names for that merge is part of trunk here and holds
  the segment's changes as they are here. If not, the run stops at that step
  and says why: the pull request merged into another branch, trunk lacks
  named files, or the fetch did not bring the commit. The plan says the same
  before anything runs when the repository already knows. A new step,
  "Check that trunk holds …", shows in every plan that follows a merge.
- Fixed: a bookmark that reused the name of a branch whose pull request had
  merged earlier was treated as landed, and Land deleted the new work. A
  pull request's merge now vouches only for the commits it was merged from;
  other work under the same name has to be on trunk's tip to count, so a
  patch that landed once and was reverted is not taken as landed again.
- The check measures a segment from the commit below it in the stack. A
  segment that merges a side branch in is checked for that branch's content.
  Commits that are themselves in trunk's history are judged at the commit
  where they entered it, for everything that commit brought in (from each
  place the branch left trunk, when there is more than one): a merge that
  takes a branch in and keeps none of its changes, or only some (GitHub
  marks that pull request merged too), is not a landing, and the bookmark
  is left alone.
- Land compares landing a stack one pull request at a time with landing its
  tip in one go before it merges the first pull request, and stops when the
  two would end differently, naming the files. They can when trunk has made
  a change that a lower pull request makes too and a higher one changes
  again: on GitHub a flag that trunk had turned on ended up off after the
  stack landed, where its tip would have left it on, with every merge
  passing its check. Rebasing the stack onto trunk and publishing makes the
  pull requests show what lands, and Land goes ahead. The same is asked
  after a landing, before what is left of the stack is rebased onto trunk:
  a Land that picks a stack up after its bottom pull request merged used to
  rebase the rest unasked.
- Fixed: after a landing, the rest of the stack was rebased onto the local
  trunk bookmark, or onto `trunk()` when that named a local commit (Jiji's
  default `trunk()` takes the latest of the remote's branch and the local
  one). Commits on the local trunk that nobody had pushed then went into
  the next pull request's branch. Landings are now checked against, and the
  stack rebased onto, the plan's trunk branch as the remote has it after
  the fetch. `trunk()` is not asked again during a run, so a fetch that
  changes what it names (another of `main`, `master` and `trunk` moving
  ahead) does not change where a stack lands.
- Fixed: that rebase put each remaining change on trunk alone. A change
  that merged a side branch in lost the branch, and its pull request lost
  the branch's files. Only the parent that landed is replaced now.
- Land does not merge, enqueue or arm auto-merge for a change that carries
  a jj conflict. Such a commit cannot be pushed by jj, but one pushed some
  other way reads to GitHub as ordinary files.
- Fixed: cleanup after a landing deleted the bookmark wherever it pointed
  and abandoned the changes whatever they had become, so an edit made while
  Land ran, or a commit pushed to the branch after the merge, went with
  them. Cleanup now deletes a bookmark only while it points at the commit
  that was checked and abandons exactly the commits that were checked.
  Anything else is left in place, and the step says so. A change started on
  a landed one while Land ran is moved onto trunk before its parent is
  abandoned, so its files keep what landed; if one turns up after that
  look, nothing is abandoned, and the step names it and says what is left
  to do by hand (the landed bookmark is deleted by then).
- Land refuses to merge a pull request whose branch on GitHub is not the
  commit the bookmark has here, when something pushed to it since the last
  fetch. Auto-land fetches once in that case and lands what is there if the
  bookmark followed; the round then starts by saying that a branch had
  moved. If the fetch shows the branch was rewritten or cut back, the job
  ends and says so, and likewise when a branch still to land is shorter
  than the job's last look left it, whoever fetched, also across a restart,
  or when a branch has left the stack without its pull request having
  merged into trunk.
- At the end of a round Land hands the next pull request to GitHub's
  auto-merge only while its bookmark still holds the changes the plan
  listed. A branch replaced on GitHub while the pull request below it
  merged comes in with the run's own fetch; auto-merge is not armed on it.
- Fixed: after a rebase merge, the landed changes stayed behind as divergent
  copies. They are swept now.
- Fixed: a pull request landed as a merge commit and fetched, but not
  cleaned up (the run stopped after its merge, or the merge was made on
  github.com), left a stack that could not move: Land called its bookmark
  immutable history and told the pull request above to be republished, and
  an auto-land job gave up after its retries. Land now checks that
  bookmark, points the pull request above it at trunk, and removes it. It
  does that only for a branch no other open pull request is based on: a
  long-lived branch merged into trunk looks the same, and is left alone.
- Fixed: after a merge, the pull request two segments up could be pointed
  at trunk and armed for auto-merge when the segment between had no pull
  request, which would have shown and landed that segment's commits. Only
  the segment directly above is handed on. Auto-merge and the merge queue
  are also refused for a pull request that no longer points at trunk.
- After a merge, the next pull request is retargeted to trunk before the
  stack is rebased and pushed, so it spends less time pointing at a branch
  that has landed.
- Auto-land counts a pull request as landed only once that check has passed.
- GitHub stacks. `jiji publish --stack`, or "Link as a GitHub stack" on the
  Update card, links a stack's pull requests as one of GitHub's own stacked
  pull requests. Nothing is linked unasked. Once a stack is linked, by Jiji
  or by anyone on github.com, Jiji keeps it in step without being asked: new
  pull requests are added on top, and a reorder dissolves the stack and links
  it again in the new order (GitHub cannot reorder a stack or retarget a pull
  request inside one). `jiji publish --no-stack` dissolves it.
- Landing a GitHub stack goes through the merge endpoint GitHub requires for
  one, which the previous release did not know: its Land failed on a linked
  stack with "GitHub rejected the token". A round lands every pull request
  that is ready from the bottom up in one request, all or none, waits for
  GitHub to rebase the rest, and then checks and cleans up as usual.
  GitHub has no auto-merge for stacked pull requests, so a pull request
  still waiting on checks or a review is reported as waiting; auto-land
  sits through it. Bypass applies to the bottom pull request only, which is
  GitHub's rule.
- GitHub rebases its own copy of every branch left above a merge in a
  stack. Land does not merge below a bookmark of the same GitHub stack that
  differs from its branch on GitHub (it says to publish first), because the
  change would otherwise exist twice afterwards. When GitHub cannot rebase
  a branch because it conflicts, Land rebases it here; when GitHub has not
  got to it yet, Land stops and asks to be run again instead of rebasing
  across GitHub's own. (GitHub reads as conflicting for a moment before a
  rebase that succeeds, so the answer counts once it has stood ten seconds.)
- Land refuses a pull request whose GitHub stack lands on another branch
  than trunk, and reports a pull request that GitHub put in the merge queue
  as queued, not as merged.
- A publish that has to dissolve and link a GitHub stack again keeps the
  pull requests above the published head in it when they still sit on it,
  and names any member the new stack cannot hold before anything runs. A
  publish that stops in the middle says what it left: a pull request still
  pointed at trunk, a stack dissolved and not linked again.
- `jiji stacks`, `jiji status` and a stack's header in Publish say when a
  stack is a GitHub stack.
- Ship finishes the pull requests of what it ships. Pushed to trunk as it
  stood, a stack had its bottom pull request marked merged and the ones
  above it left open, because GitHub marks a pull request merged only when
  its head is inside its own base, and afterwards refuses to point those at
  trunk. Ship now points each of them at trunk before the push, pushes a
  branch first when the commit that ships is not what GitHub has for it,
  and after the push waits for GitHub to mark them merged and says what it
  found. The plan names each pull request and what happens to it, in the
  app and in `jiji ship --dry-run` and `--json`. When the push is refused,
  every pull request is pointed back at the base it had, and the failed
  step says so; a step that fails before the push is undone with the ones
  before it. A push that reports an error may still have arrived, so Ship
  asks GitHub where trunk is before it puts anything back, and puts
  nothing back when it cannot ask. A GitHub stack is dissolved first when
  all of it ships (GitHub cannot retarget inside one, and leaves a shipped
  stack's upper pull requests open), and linked again if the push is
  refused; a stack that would ship only in part is refused. A pull request
  that sits on the shipped work without being part of it is pointed at
  trunk and stays open.
- Ship does not go by a list of pull requests it cannot trust. It stops
  when GitHub cannot be read (only a missing token still ships, and the
  plan then says that it knows of no pull requests and what GitHub does
  with them anyway), and when GitHub answered only its 100 most recently
  updated open pull requests while the work has branches on GitHub.
- Fixed: shipping only the bottom of a GitHub stack left the pull request
  above it based on the shipped branch, where GitHub does not move it, and
  the next Land deleted that branch, which made GitHub close the pull
  request. Ship now refuses a GitHub stack it would ship in part, and Land
  keeps a landed branch for as long as any open pull request is based on
  it, your own included; for one in a GitHub stack the step says to
  dissolve the stack, which lets the pull request be pointed at trunk.
- A setting, "Link new stacks as GitHub stacks", in the app's Settings and
  followed by `jiji publish` as well. It is off unless turned on. With it
  on, a publish that opens a pull request for a chain that is not a GitHub
  stack links the chain without being asked; a plain chain that is only
  updated stays plain. `--stack`, `--no-stack` and the checkbox on the
  Update card still decide for one publish, either way. Where the
  repository has no GitHub stacks the publish goes through and says the
  pull requests stay a plain chain.

Someone else's pull request on your stack (a teammate's, or your own from
another clone: its branch is not a bookmark here):

- Fixed: once a fetch had brought their branch, the stack under it was
  gone. jj calls everything an untracked remote bookmark contains
  immutable, and Jiji took immutable for landed: `jiji stacks` did not list
  the stack, `jiji status` and Publish showed no changes, and Land answered
  that the bookmark "points at immutable history — everything under it is
  already part of trunk". A change that is immutable only because someone
  else's branch builds on it is still drawn, grouped and counted as yours.
  One that is immutable for a reason of its own (a tag, or what your own
  `immutable_heads()` names) stays so, whoever's branch also rests on it.
- Fixed: Land could stop half-way through a landing. Its own fetch brings
  their branch, and the rebase of what was left then failed with "Change …
  is immutable", after the merge and the retarget, with nothing pushed and
  nothing cleaned up. That rebase now goes through, and their commits are
  not rewritten by it.
- Fixed: Land closed their pull request. GitHub closes every open pull
  request based on a branch when the branch is deleted, whether or not the
  branch's own pull request merged, and Land deleted a landed branch
  without looking. Land now points their pull request at trunk before the
  branch is deleted, and asks GitHub which pull requests are based on a
  branch at the moment it would delete it: while there is one, the branch
  and its changes are left in place and the step names it.
- Publish, Land and Ship never push to their branch. After a squash or a
  rebase landing their pull request shows your landed changes as its own
  (its branch still carries the commits that were squashed or rebased)
  until its author rebases the branch. Land's plan and its retarget step
  say so by number, author and branch before and when it happens. A
  merge-commit landing replaces nothing under it, and it shows what it
  showed before. Land leaves their pull request as it is when it was
  closed, or when its author has pointed it at another branch since the
  plan was made, and never merges it.
- Land stops before it merges anything when someone else's GitHub stack
  starts on a branch the run would land. GitHub lets no one else change a
  base inside a stack, so their pull request could not be pointed at
  trunk, and the landed branch could not be deleted without GitHub
  closing it. The plan names their pull requests and the stack.
- Fixed: Land stopped half-way when its own fetch brought their branch
  and that branch was built on versions of your changes that you had
  rewritten since (a reorder or an amend, published before fetching). The
  rebase of what was left answered "exists more than once here now …
  abandon the copy you do not want", after the merge and the retargets,
  and the other copy is theirs to keep. It now goes on with your commit,
  and their branch keeps the one it was built on.
- Fixed: in a repository that deletes head branches on merge, every
  landing ended with a failed "Clean up landed bookmark" ("moved on the
  remote since the last fetch") and left the landed change behind. GitHub
  deletes the branch a moment after the merge, later than Land's fetch, and
  the push of the deletion was refused. Land now fetches once more when
  that push is refused, and with the branch gone the step is done.
- Fixed: Publish treated the part of a stack under their branch as trunk.
  A new segment on top was opened against trunk with the whole stack inside
  it, and an update planned to point your own next pull request at trunk.
- Fixed: after you rewrote a change they had built on, a fetch left that
  change divergent (their branch keeps the old commit), and Publish refused
  it with "resolve the divergence first", which nothing here can do. Such
  a copy no longer blocks. It goes when its author rebases their branch.
- Publish says, by number and author, when its push replaces commits under
  their pull request, when it dissolves a GitHub stack their pull request
  is in (the step lists the pull requests that leave the stack), and
  refuses a push that would put their pull request's head inside its own
  base. A pull request from a fork is theirs whatever its branch is
  called: it is never matched to a bookmark here by name.
- Ship leaves their pull request open, pointed at trunk and showing only
  its own changes, and refuses when it could not: when the work has to be
  rebased onto trunk first, when the branch their pull request rests on
  was rewritten here since it was pushed, when a change under it was
  rewritten after their branch was built on it, and when their pull
  request's head is among the commits that ship, on the chain or brought
  in by a merge (GitHub would mark it merged).
- `jiji stacks`, `jiji status` and the ledger in Publish show their pull
  request with the stack it rests on, by number and author, and no longer
  list your stack a second time as theirs. `jiji status` and the change
  header's "immutable" tag say why changes of yours read as immutable
  while their branch builds on them.

Taking over someone else's pull request, for when someone opens one to
help and wants you to push it over the line:

- "Take over", on their pull request's row in Publish, on the card of
  their stack and in the command palette, and `jiji take-over NUMBER` (or
  a branch name) in the command. Their branch becomes a bookmark here, at
  what GitHub has for it, and their pull request a segment of your stack
  like any other: listed in the landing order with whose it was, rebased
  with the stack, pushed by Publish and merged by Land. The changes of
  yours under it are yours to rewrite again. The plan is shown first and
  says what it means; nothing is pushed until your next Publish. On
  GitHub the pull request stays its author's, with their title and
  description, and their commits keep their author.
- "Give back", on the segment and in the palette, and `jiji give-back
  NUMBER`, is the reverse: no bookmark here any more, the branch left as
  it is. It is refused while changes to the branch were not published or
  work of your own sits on it.
- From the take-over on, Jiji pushes to a branch someone else made. Each
  push goes through only if the branch is where the last fetch saw it, so
  a commit its author pushes afterwards is never overwritten. A plan that
  pushes the branch names the pull request and its author as taken over; a
  push that is refused because they pushed again says so, and what to do.
- A branch that was built on an earlier copy of your changes (you amended
  them since, or landed what was under them) is brought onto the copy you
  have now when it is taken over, here only; the result says so and the
  next Publish pushes it.
- A pull request from a fork cannot be taken over, and the plan says why:
  its branch is in another repository, and pushing there takes the fork as
  a remote of its own, which Jiji does not set up. Nor can one that rests
  on another pull request of someone else's, until that one is taken over.
- `jiji publish --stack`, and "Link as a GitHub stack" in Publish, now take
  someone else's pull requests that sit on top of the chain into the stack,
  naming each by number and author. In a stack GitHub rebases their branch
  itself after each merge below it, so their pull request goes on showing
  only its own changes. Where Land's plan says that their pull request
  will show your landed commits, it now names the ways to avoid that:
  take it over, or, for one on top of the chain, link the stack first.

Known limits:

- From the moment commits under someone else's pull request are replaced
  (a publish of a rewrite, a squash or rebase landing, the rebase of what
  is left of your stack after any landing) until its author rebases its
  branch, their pull request shows the replaced commits as its own.
  Publish and Land say so by number and author and leave their branch
  alone.
- GitHub cannot delete a branch only if no pull request is based on it.
  Land asks which are just before it deletes a landed branch; one opened
  on that branch in the instant between is closed with it.
- While a landed branch is kept because an open pull request is still
  based on it, each later Land of that stack starts by reconciling it
  again and merges nothing further until the pull request has moved.
- Work of your own built on top of their commits is not part of this:
  their commits are a base below your bookmark, and the pull request you
  publish for that work targets trunk. Changes of your own under their
  commits then read as that base too, and are not listed as a stack.
- A change counts as yours under their branch only where jj makes it
  immutable through `untracked_remote_bookmarks()` called plainly, as the
  default `immutable_heads()` does. Where their branches cannot be told
  from your own reasons, nothing is taken apart and the stack under their
  branch reads as landed history, as it did before and as it does for jj:
  with an `immutable_heads()` that names remote branches another way
  (`remote_bookmarks()`), with one that calls `untracked_remote_bookmarks`
  with an argument, and with a configuration that defines a helper of that
  name itself.
- The branch of a pull request from a fork is never here. Once you have
  published a rewrite under such a pull request (Publish says so then),
  Ship cannot tell that their branch is still built on the replaced
  commits, and goes ahead; their pull request shows those commits before
  the ship and after it, until its author rebases.
- GitHub dissolves a stack as a whole. A pull request that someone adds to
  a GitHub stack in the moment between the plan and the dissolve leaves it
  with the rest, unnamed.
- Ship refuses work under someone else's pull request when it would
  replace the commits their branch is built on; Land goes ahead and says
  what their pull request shows afterwards. With more than 100 open pull
  requests in the repository, GitHub's list is cut short and Ship refuses
  work that has branches on GitHub; Land takes it.
- When Ship's push fails and it puts the pull requests back, a base that
  someone changed by hand while the ship ran is overwritten with the base
  the plan saw. And a push that arrived although it reported an error, on
  a trunk someone pushed to again before Ship looked, reads as not
  arrived: the pull requests are pointed back, and the ones above the
  bottom stay open with nothing left to merge.
- When several pull requests of a GitHub stack land in one request, GitHub
  writes the squash commits of all but the top one itself, from each pull
  request's title and commits. Only the top one carries Jiji's message.
- Some paths for GitHub stacks are written to GitHub's documentation and
  have not been run against GitHub: landing onto a branch protected by a
  merge queue (with the "queued, not merged" answer and a queued pull
  request that cannot leave its stack), Bypass (which needs repository rules
  to bypass), and a merge request that finds another one already pending.
- In a GitHub stack, GitHub's own lease covers only the pull request the
  merge is asked for. Jiji re-reads the ones below just before the request;
  a push to one of them in that instant lands with it, and the check after
  the merge then says that it landed from another commit than the one here.
  Likewise, a stack dissolved on github.com in the instant before the
  request sends the pull request asked for into the branch below it; the
  check stops the run, and the content lands with the pull request below.
- When a branch got commits on GitHub ("Update branch") and was merged
  there, Land needs two runs to clean up: the first leaves the bookmark its
  fetch moved, and says so. An auto-land job that finds both already done
  ends after the first.
- The comparison of one pull request at a time with the tip is made here,
  with jj's merge. Before a first merge it is against trunk as the last
  fetch left it; a change that reached trunk on GitHub after that fetch is
  seen only when the rebase after the merge asks again. It is not made for
  the rebase method. A stack with a merge in it, one of more than 500
  commits, and one that sits on something trunk neither contains nor holds
  are not judged and land as before. In a GitHub stack, GitHub rebases
  what is left after a merge itself, before Land can ask.
- After a pull request of a stack has landed, that comparison is about
  what is left, measured from the landed one. A change trunk had made by
  itself, that the landed pull request also made and one further up
  changes again, is then seen as one change, and the later version lands.
  The comparison before the first merge catches it only when trunk's
  change had been fetched by then. With trunk moved since a stack was
  started: fetch, rebase and publish before landing the first of it.
- Publish does not detect a base branch that has moved here and has also
  moved on GitHub since the last fetch: the push of that base is refused,
  and the pull request on it is not parked for it. Nor one that has no
  pull request of its own and has moved on GitHub since the last fetch:
  a branch pushed back onto a commit that base has gained is closed by
  GitHub as merged into it. Fetch before publishing a branch onto an older
  commit.
- `jiji land NAME` on a long-lived branch that was merged into trunk, has
  not moved since, and has no open pull request based on it removes the
  branch like any landed one. GitHub refuses the delete for a protected
  branch.
- A pull request that renames a file trunk has edited since cannot be
  merged by GitHub as it stands (GitHub reports a conflict where git itself
  would follow the rename), so it has to be rebased first, like any
  conflict.

## 0.5.1 - 2026-11-26

- Jiji is now built for Apple Silicon only, which halves the download
  (about 18 MB, down from 36). The universal build carried an Intel half
  that an Apple Silicon Mac never runs.
- Intel Macs are not offered this update. 0.5.0 is the last build that
  runs on them, and it stays on the Releases page.

## 0.5.0 - 2026-11-26

Jiji is now a native app. The interface was rewritten from a web view to
GPUI, the toolkit behind the Zed editor, so the graph and the diffs are
drawn on the GPU by the same Rust program that reads your repository with
jj-lib. It is the same workbench, with the same layout, shortcuts and
`jiji` command line. Your open tabs, preferences, saved views, window
placement and license carry over the first time it opens.

- A refreshed look. Change ids in the graph take their workstream's lane
  color. The change header leads with a larger title, then the author,
  where the change sits in its stack ("4 of 4 in Op log filter") and a
  five-block diffstat. Added and removed runs carry a bar down the left
  edge of the diff.
- The title bar has one command field (⌘K) in place of separate buttons,
  with trunk's state and Fetch beside it. History search stays on ⌘⇧F,
  themes and the license moved into Settings, and a tab's close button
  shows on hover.
- A comparison against a pinned change or a range is read-only. File, hunk
  and line actions are offered on a change's own diff, where they cannot
  land on the wrong change.
- Updates are checked twice: after the download, and again at "Restart to
  update" on the exact bytes that get installed. If the swap fails, the
  app you had is put back. An app in a folder you cannot write to asks for
  an administrator password instead of failing.
- Jiji keeps a log at `~/Library/Logs/com.lauf.jiji/Jiji.log`. If it quits
  unexpectedly, the next launch restores your tabs, says so, and offers the
  log.
- Requires macOS 12 or later, and is tested on the current macOS release
  and the one before it. 0.4.1 ran on older systems: a Mac on macOS 11 or
  earlier cannot open this release and should decline the update. 0.4.1
  stays on the Releases page.
- Building from source is `cargo build`. Node, Bun and the web toolchain
  are gone, and the repository is one Cargo workspace.

Known gaps in this release:

- An error while drawing a view quits the app, where 0.4.1 showed the
  error inside that view. Your tabs come back on the next launch; a
  description you were typing at that moment does not.
- Keyboard focus for buttons and VoiceOver labels were rebuilt for the new
  toolkit and have had less use than the rest. If something that worked in
  0.4.1 does not work here, please open an issue.

## 0.4.1 - 2026-10-06

A cleaner Publish section, in the same flat style as the workbench.

- A stack's header now matches a change's two bars: the title with a
  Workbench button, then the stack's state, its pull requests and
  changes, the branch it lands into, and whether it is your current
  stack.
- The landing order reads as one list: each row shows its PR number,
  title, branch, checks and review, with "Next to land" or "Merged"
  beside it, and the heading counts how many have merged.
- Land and Update are a segmented control at the top of the action
  column, beside a labelled More menu for shipping straight to trunk.
- Stack list rows lead with the title, then the status and PR count.
  Auto-land jobs are flat rows with a status dot that pulses while the
  job runs.
- Counts read correctly in the singular ("1 step", "1 change").

## 0.4.0 - 2026-09-28

Everything since 0.1.0 in one release: a new look built around the diff,
the `jiji` command line, stacked-PR publishing with landing and auto-land,
conflict resolution, evolution and annotate views, repository tabs, and
jj 0.45 underneath.

- A new look, built around the diff. The window is one full-bleed surface
  with hairline joins: repository tabs share a slim title bar with the
  traffic lights, sections move to a 44px icon rail (⌘B hides it), and the
  change header shrinks to two bars — title and actions, then ids, author,
  bookmarks and the diff controls — so code starts about 130px higher, and
  in a 1440px-wide window the code column is roughly 60% wider. Diffs run
  edge to edge with a pinned header per file and full-contrast context
  lines; graph rows are denser, with flat selection and quiet ref labels.
  The interface uses the platform's own fonts, popovers and menus are
  solid surfaces, glows, gradients and blur are gone, and section, view
  and palette switches are instant. The ten themes were redrawn on the
  same four surface steps, and the welcome screen is a plain start page
  with your recent repositories.
- Built on jj 0.45 (jj-lib 0.45.1, up from 0.39), so Jiji reads and
  writes repositories the way current `jj` does. Fetching follows jj
  0.44's tags: they arrive as `<name>@<remote>`, tracked by local tags,
  and `remotes.<name>.fetch-tags` narrows them. New remote bookmarks
  follow `remotes.<name>.auto-track-bookmarks` now that
  `git.auto-local-bookmark` is gone, and untracked remote tags count as
  immutable, like in the CLI.
- Open a plain Git repository and Jiji offers to set jj up in it, the way
  `jj git init --colocate` does: a `.jj` folder next to `.git`, your
  branches imported as bookmarks, and Git working alongside. Nothing
  changes unless you confirm; opening a folder inside the repository
  offers its root.
- Updates no longer restart Jiji on their own. A new version downloads in
  the background and the status bar offers "Restart to update"; Check for
  Updates… still asks first. A window left open looks again every few
  hours.
- Fixed hiding or unhiding right after another hide occasionally failing
  with "another hide/unhide command owns the archive lock".

- `jiji ready` and `jiji draft` change a stack's review readiness without
  `gh`: mark its draft PRs ready for review, or convert its open PRs back
  to drafts. Nothing is pushed and bases and descriptions are untouched.
  Name an intermediate bookmark to change only the PRs up to it. Both
  support `--dry-run` and `--json` like the other stack commands.
  `jiji publish --draft` opens new PRs as drafts, so reviewers and code
  owners are not asked until the stack is ready.
- History search (⌘⇧F) reopens on the last query, selected, so repeating
  a search is one keystroke and a new one is just typing.
- Tabs for repositories that share a name (`~/work/api` and `~/oss/api`,
  or workspaces in same-named folders) show the enclosing folder that
  tells them apart, in the tab strip and the palette's tab rows.
- Fixed the change header's open panels closing whenever the repository
  refreshed. Saving a file in your editor while describing, splitting, or
  rebasing the working copy used to shut the panel mid-edit (and drop the
  split selection); the header's More actions menu and a failed merge
  tool's error no longer vanish on a refresh either.
- Fixed Jiji rewriting trunk on its own. Opening a repository fetches,
  and while the fetch was still downloading, the repo watcher could
  import the half-updated refs as a second, concurrent operation: a
  merged PR's deleted branch made its commits look abandoned, and jj's
  reconcile of the two operations rebased every trunk commit built on
  them, leaving local `main` diverged from `main@origin`. Jiji's own
  writes to a repository (refreshes, fetches, pushes, every action) now
  take turns; reads still run alongside. If a repo shows "reconcile
  divergent operations" right after a fetch and `main` diverged, restore
  to the fetch's operation (`jj op restore <fetch op>`).
- Fixed the workbench staying blank ("Idle — open a repository") when a
  remote bookmark sits on a commit rewritten since, e.g. after amending a
  pushed branch and switching to the Team view. The hidden commit now
  goes by its commit id, like jj addresses it, instead of colliding with
  the change's visible commit; it also opens its own diff rather than the
  visible commit's. A view that fails to render now says so and offers
  Try again, instead of freezing.
- Refreshes on big repositories are several times faster: whether trunk
  contains each bookmark is answered in one pass instead of one history
  walk per bookmark, and emptiness of merge commits is remembered instead
  of re-merging parent trees every time. On a 105k-commit repo with 1,500
  remote branches a refresh went from about 870ms to 160ms.
- An unsaved description is no longer lost when the editor closes without
  saving (Esc, Cancel, selecting another change, switching tabs). Reopening
  Describe on that change restores the draft, with a Discard link; the
  pencil wears a dot while a draft waits. A draft is dropped once saved,
  and never offered over a description changed elsewhere.
- Move through long histories from the keyboard: after clicking into the
  graph, Page Up/Down moves the selection a screenful and Home/End jump
  to either end (elsewhere they still scroll the diff). `@` jumps back to
  the working copy from anywhere, also as "Go to working copy" in the
  command palette.
- Drop folders from Finder onto the window to open them as repository
  tabs, like dropping them on the Dock icon. A folder inside a repository
  opens that repository.
- Opening a folder inside a repository opens the repository, the way `jj`
  works from any subdirectory. Picking `src/` in the open dialog used to
  refuse with "Not a JJ repository".
- Pushing a bookmark or deleting one from a remote no longer freezes the
  window while the remote responds.
- Recent repositories can be removed from the welcome screen, so a moved
  or deleted repo no longer lingers in the list.
- Added ⌘⇧T to reopen closed repository tabs, newest first, also under
  File ▸ Reopen Closed Tab and in the command palette.
- Status-bar errors show their full text on hover, including jj's hint
  lines, and can be dismissed instead of holding the bar until the next
  refresh.
- Fixed switching tabs while a repository was still opening: the
  repository's saved graph lens could land on the other tab's repository
  and show its history under the wrong tab. A tab closed mid-open now
  also releases the repository it was loading.
- Added `jiji hide`, `unhide`, and `hidden` to put local changes or stacks
  aside and recover them by change ID. Hidden work is removed from ordinary
  jj/Git history, with protected object packs preserving recovery across Git
  garbage collection. Commands support dry runs and JSON output.
- Added the first-class `jiji` CLI: stack discovery/status, publish, one-round
  landing, supervised auto-land with saved resume, direct shipping, PR review,
  and shared GitHub authentication. Commands support repository selection,
  readable output, JSON, dry runs, and landing method/bypass options.
- Moved Publish stack discovery and status classification into shared Rust so
  the CLI and desktop recognize the same jjpr stacks. Existing app-only CLI
  launchers can be upgraded from the command palette; `jiji .` still opens Jiji.
- Rebuilt Publish around selectable stacks with linked PRs in landing
  order, checks and review status, and adjacent landing controls. Stacks
  created with jjpr appear automatically; local scratch work is collapsed
  and the PR browser has its own tab. Publishing reads complete stacks
  regardless of the workbench's graph filter.
- Fixed trunk detection to honor the repository's `trunk()` alias. Repos
  using `develop` no longer plan publishing or landing against `main`.
- Refined the workbench with clearer change titles, readable secondary
  text in every palette, a history search shortcut, and a compact menu for
  less-used change actions. Pane resizing preserves room for the diff and
  supports arrow keys, Home/End, and double-click to reset.
- Reduced initial JavaScript by about 59% by loading syntax languages and
  secondary sections on demand. Large history graphs render only nearby
  rows, and diff scrolling shares one viewport measurement per frame with
  binary searches over cached row offsets.
- Isolated diff caches by repository, preserved proper LRU ordering, and
  prevented stale diff content from supplying file actions during selection
  changes or refreshes. Failed loads now show an error and a Retry action.
- The graph stays readable however wide history gets. Three changes to
  how the workbench lays out the tree: a rail now joins the rail already
  waiting for the same parent as soon as it can (jj's own `├─╯`), so
  forty stacks rebased onto trunk draw two columns instead of forty and
  teammates' branches on one base converge the same way, while trunk's
  spine never bends into another line; history is walked one branch at a
  time down to its base before the next head (git's topo-order, jj's
  grouped log) instead of interleaving parallel lines by date; and the
  gutter has a lane budget measured off the pane — the leftmost lanes
  keep full spacing only as far as every row's text keeps its room, the
  lanes past that compress into a narrow band of hairlines with small
  markers, and anything beyond the band shares its last slot. A row's
  text no longer gives way to the tree: a deep `All history` lens with
  twenty open branches, which used to push every subject off the pane
  behind a horizontal scrollbar, now shows the working copy and every
  commit's id and description at the same left edge. Widening the pane
  spends the room on lanes; narrowing it protects the text. Rewrites and
  pane drags morph the rails to their new positions as before.
- Evolution grew into a full surface: a change's version history now
  takes over the diff pane as a scrubbable timeline instead of a cramped
  header dropdown. Every version the change was rewritten through sits
  on the strip oldest to newest, each step wearing its rewrite's weight
  (+/− lines, dashed when only the description or position moved); the
  selected step's diff renders through the real diff pipeline — syntax
  highlighting, unified or split layout, virtualized cards. Built for
  the `jj edit` workflow: the pane opens on the newest step, so "what
  did I just change" is one click (or the palette's "Show evolution")
  away. ←/→ scrub through time, ⇧-click or ⇧←/⇧→ widen the span to read
  several rewrites as one diff, and description-only rewrites show the
  description's own before/after. Restore moved in too: any earlier
  version restores from the timeline's caption, confirm-first, undoable
  from the op log. Esc returns to the diff.
- Save a revset as a named view: the lens picker's revset input grew a
  "Save as view…" affordance — name what you typed and it becomes a
  preset row in the picker's new Saved group, per repo, surviving
  restarts. A saved view applies with one click like the built-in
  lenses (the lens trigger wears the view's name while it is active),
  and hover offers rename and delete. A saved revset that stops
  resolving (a deleted bookmark, a typo) refuses inline with jj's own
  diagnostic, exactly like a typed one, leaving the graph as it was.
- Git tags show in the graph: a commit a colocated repo tags (a release,
  an rc) now wears a quiet tag chip beside its bookmarks, on whatever row
  the tag's commit is drawn. Read-only markers — Jiji offers no tag
  management, and jj itself treats tagged commits as immutable.
- Annotate any file — `jj file annotate` made visual: right-click a file
  in the diff pane or its file tree and pick "Annotate File" to see, for
  every line of the file as that change's tree carries it, the change
  that introduced the line (its id, author, summary, and age, grouped
  into runs with the whole file's text alongside). Clicking a line's
  origin jumps the graph to that change — revealing it under a transient
  revset when the current lens does not draw it — and hovering any line
  tints everything else that came from the same change. Read-only;
  esc returns to the diff. Binary, oversized, and conflicted files
  refuse with a plain answer instead of pretending markers have blame.
- Resolve divergence without the CLI: the change header's divergence
  callout now offers, per copy, Compare — the two copies' trees against
  each other in the diff pane, through the ordinary pinned comparison —
  and Abandon, with a confirm stating what goes (bookmarks on the copy,
  descendants rebasing) and that the change stops being divergent with
  one copy left. After abandoning, the selection follows the copy that
  was kept. Undoable from the op log like any mutation.
- See how a change evolved: the change header's new Evolution button (and
  the palette's "Show evolution") opens the change's version history —
  `jj evolog` made visual. Every version the change was rewritten through
  lists newest first, each wearing the operation that recorded it and its
  time; a "diff" disclosure on each transition unfolds exactly what that
  rewrite changed, and description-only rewrites say so. Any earlier
  version can be restored: the change's files and description return to
  that version as one new operation (the change keeps its place in the
  graph — parents and author stay), descendants rebase, and the op log
  undoes it like any mutation. Versions from before the op log's horizon
  still list from commit metadata, just without their operation.
- Conflicts render in the diff pane: a conflicted file no longer shows
  jj's raw marker text diffed against the parent — it renders its merge
  structure, read-only. Resolved runs read as context (long ones elide
  like unchanged lines), and each conflict region opens with a
  "Conflict N of M" band followed by every side as its own diff from the
  base, labeled "Side #1 · ours" / "Side #2 · theirs" to match the take
  verbs (plus jj's conflict labels — "rebase destination", a commit's
  description — when the tree carries them). Conflicted files a change
  merely inherits (the parent-relative diff never mentions them) now
  appear in its diff and file list too, so opening a conflicted change
  always shows what is unresolved. Conflicts with more than two sides
  render every side; binary and oversized conflicts keep the old
  marker-text rendering.
- Resolve conflicts without leaving the app: every conflicted file's row
  in the Conflicts view grew "Take ours" / "Take theirs" buttons that keep
  one side of the conflict wholesale (the CLI's built-in `:ours`/`:theirs`
  merge tools — ours is side #1, usually the rebase destination's version;
  theirs side #2, usually the change's own). Instant, no merge-tool window,
  and available even when no external merge tool is configured; the op log
  undoes it like any mutation. Conflicts with more than two sides refuse
  with a pointer to the CLI, matching `jj resolve`.
- Combined diff of a stack selection: ⇧-click a second change in the same
  workstream and the selection extends to the contiguous run between the
  two, every row highlighted, with the diff pane showing the run as one
  combined diff — everything the selected changes add together, measured
  from the run's root parent. The header states the range
  ("3 changes as one diff · root → head · vs base"), the range's base row
  wears the graph's "diff base" chip, and Esc (or the header's ×)
  collapses back to the anchored single change before clearing the
  selection. First slice is contiguous-stack-only; ⇧-clicking outside the
  anchor's lane falls back to a plain select.
- Split single lines: the split panel's hunks now open one level further,
  to a checkbox per changed line. Checking a subset of a hunk's lines
  carves just those line changes into the new (or destination) change,
  with the file and hunk checkboxes showing an indeterminate "–" while
  partially picked. The picked line indices ride the same
  freshness verification as hunk coordinates — a change that moved
  underneath the panel refuses instead of rewriting unseen content.
  Alongside it, "Discard This Line" in the diff context menu reverts just
  the changed line under the cursor.
- Discard a single hunk: right-click a hunk in a mutable change's diff
  (the working copy included) and "Discard This Hunk" reverts just that
  hunk, leaving the file's other hunks in place. The hunk's rendered
  coordinates are re-verified against a fresh diff before anything is
  rewritten — if the change moved underneath the menu, the discard
  refuses with "refresh and try again" instead of touching content you
  never saw. The op log undoes it like any mutation.
- Revert a change (`jj revert`): "Revert Change" in a change's context
  menu and the command palette creates a new change on top of the working
  copy that backs out the target's diff, described like the CLI
  (`Revert "…"` / `This reverts commit …`). Works on immutable changes —
  backing out landed work is the point — and the op log undoes it.
- Fixed: undoing an operation that only added a commit (duplicate, and
  now revert) from the op log answered "already undone" and recorded
  nothing; the added change now disappears as expected.
- File tree: the diff pane grew a directory tree of the changed files,
  docked left of the diff. Single-child directories compact into one row
  (`src/lib/components`), collapsing a directory folds its subtree into a
  file count and aggregate +/− stats, and the row for the file under the
  reading line stays highlighted as the diff scrolls. Click a file to
  jump to it, drag the divider to resize, and toggle the tree from the
  diff header or the command palette; it steps aside on narrow panes and
  single-file diffs.
- Tabs: open several repositories at once, one tab each. ⌘T opens a tab,
  ⌘W closes it, ⌃Tab / ⌘⇧] / ⌘⇧[ cycle, and the command palette offers
  switch rows for the other open tabs. Opening a repo that is already
  open (from the `jiji` CLI, recents, or the picker) switches to its
  existing tab instead of duplicating it; tabs are restored on the next
  launch. Background tabs stay live — each open repo keeps its own
  watcher — and a running auto-land job now survives switching tabs
  (closing its tab interrupts it resumably, like before).

## 0.1.0 - 2026-06-15

Initial alpha release for macOS.

Jiji is a local desktop workbench for Jujutsu repositories. This first release focuses on core repo navigation and history-editing workflows:

- Open a local `jj` repository and inspect its working graph.
- Review change diffs.
- Describe, create, edit, squash, abandon, move, and rebase changes.
- Create, move, rename, and delete bookmarks.
- Use the operation log to restore or revert mistakes.
- Receive future app updates through Tauri's signed updater.

Still intentionally out of scope for this alpha: conflict resolution, publishing/review flows, multi-workspace management, and forge automation.
