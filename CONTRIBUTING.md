# Contributing

devload is maintained by one person in their spare time. Small, focused pull requests get merged fastest.

## Bugs

Open an issue with the bug form. Include your devload and macOS versions, what you did, what you expected and what happened. A screenshot helps, but crop anything private (project names, paths).

## Features

devload deliberately does a few things: show which dev project uses memory, and get out of the way. Before you write a feature, open an issue to talk about it. "Show everything Activity Monitor shows" is out of scope.

## Code

- `make test` must pass. It runs the Rust tests, a C ABI smoke test and the Swift tests.
- If you change the popover or the icon, run `make snapshots` and attach the before/after PNGs to the pull request. They are drawn from sample data, so they're safe to share.
- If you change the C ABI (`core/src/ffi.rs`), regenerate the header with `make header` and commit it.
- After `make app`, `scripts/check-app.sh` checks the bundle the way CI and the release do (signature, icon, arm64, minimum macOS, a snapshot run).
- The logo lives in `docs/images/logo.svg`. If you change it, run `scripts/make-icon.sh` and commit the new `app/AppIcon.icns`.
- Keep the existing style. Comments say why, not what.
- Use `DEVLOAD_DATA_DIR` for development runs, so your real history isn't touched.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the pieces fit.

By contributing, you agree that your contributions are licensed under the [MIT license](LICENSE).
