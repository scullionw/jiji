# Contributing

Jiji is early alpha software. Small, focused changes are easiest to review and safest for people who trust the app with local `jj` repositories.

Before sending a change:

```bash
cargo test --workspace
```

A change to the app's interface should also be looked at: [crates/jiji-desktop/README.md](crates/jiji-desktop/README.md) describes its design rules and how to drive the app and take screenshots without a window on screen.
