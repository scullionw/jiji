# Jiji

Jiji is a desktop workbench for [Jujutsu](https://github.com/jj-vcs/jj) repos.

It is still early, but the core local workflow is real: open a local `jj` repo, read the graph, review diffs, describe/new/edit/squash/abandon changes, move or rebase changes, manage bookmarks, and use the operation log to restore or revert mistakes.

What is not here yet: conflict resolution, publishing/review flows, multi-workspace management, and forge automation. Those surfaces may appear in the app shell, but they are not the reason to try Jiji today.

## Running

You need Rust, Bun, and the usual Tauri desktop prerequisites for your platform.

```bash
bun install
bun run tauri dev
```

Useful checks:

```bash
bun run check
bun run test
cargo test --workspace
```

See [CHANGELOG.md](CHANGELOG.md) for release notes.

## Status

Jiji is alpha software. It is meant for people who already know `jj` well enough to sanity-check what a GUI is doing before trusting it with important work.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Small, focused PRs are strongly preferred.

## Releasing

See [RELEASING.md](RELEASING.md) for the macOS GitHub Releases flow.

## License

FSL-1.1-MIT. Source is available for permitted uses; each version becomes MIT after two years.
