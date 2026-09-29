# Jiji CLI

The standalone `jiji` executable and the desktop app share `jiji-core` and
`jiji-forge`. Stack discovery, jjpr branch matching, PR segmentation, status
classification, `trunk()` configuration, publishing, and landing use the same
Rust implementation. The CLI runs without a desktop window or webview.

## Install and run

From a checkout:

```sh
cargo install --path crates/jiji-cli --locked
```

For development:

```sh
bun run cli --help
# equivalent to:
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

`land` executes one round: merge the next PR, enable GitHub auto-merge, or join
the merge queue as appropriate, then reconcile PRs that have merged. Run it
again after a GitHub hand-off finishes, or use `watch` to supervise subsequent
rounds. Draft PRs are marked ready as part of landing. Required conditions and
the PR head checks are enforced by the shared engine.

`--method auto|squash|rebase|merge` chooses the landing method. The default is
automatic, using the methods allowed by the repository. `--bypass` explicitly
requests direct merges past checks, reviews, and merge queues, where GitHub
permits the account to do so. Merge conflicts and invalid bases still block.

`ship` sends local work directly to trunk without opening a PR. Execution
fetches first and derives a fresh plan. It also works with a non-GitHub git
remote; GitHub authentication is only used to report affected open PRs.

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
outcome. Stack listing returns the repository path, trunk, stacks, hidden local
count, and any truncation warnings. `watch --json` emits newline-delimited
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
