# `magicblock-version`

This crate exposes build and git metadata used by operator-facing surfaces.
Field names and display formatting are compatibility-sensitive because
`magicblock-aperture`, `mbv-leader`, and `mbv-tui` consume them.

`bins/mbv-leader/src/main.rs` prints the resolved build version at startup.
`bins/mbv-tui/src/app.rs` starts with its own package metadata and enriches the
display with the remote leader's `getVersion` response.

The TUI is external; there is no embedded leader/TUI feature contract.

When changing version fields, inspect the Aperture `getVersion` response, the
leader startup banner, the TUI parsing/display path, and `.github/version.sh`.

Focused validation:

```bash
cargo check -p magicblock-version -p magicblock-aperture -p mbv-leader -p mbv-tui --locked
```
