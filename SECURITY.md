# Security

devload is maintained by a single developer. Only the latest release gets fixes.

## Reporting a vulnerability

Please don't open a public issue. Use GitHub's [private vulnerability reporting](https://github.com/gaspardmoindrot/devload/security/advisories/new) instead. I'll reply as soon as I can.

Relevant areas: anything that could leak a secret from process arguments (the scrubber in `core/src/scrub.rs`), the local Docker socket client, the quit action that signals processes, and the release build.
