# Contributing

Jiji is early alpha software. Small, focused changes are easiest to review and safest for people who trust the app with local `jj` repositories.

Before sending a change, run the checks that match the area you touched:

```bash
bun run check
bun test
cargo test --workspace
```

For release-related changes, also run:

```bash
bun run release:preflight
```

Keep generated bindings in `src/lib/bindings` up to date when Rust types exported through `ts-rs` change:

```bash
bun run bindings
```
