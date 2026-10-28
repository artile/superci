# SuperCI fork (artile/superci)

This checkout is our fork of [supercorp-ai/superci](https://github.com/supercorp-ai/superci). It runs CI for `my-remotely/web-app` on Modal.

Read [docs/tao/operations.md](docs/tao/operations.md) before you change, build or deploy anything. It describes the branch model, the deploy to remo-dev, the CI setup and the facts we learned.

Rules:

- `main` mirrors upstream. Do not commit on it.
- `tao` is the integration branch that runs on remo-dev. Change it only by merging PR branches or by commits that stay ours (docs under `docs/tao/`, this file).
- Each upstream change gets its own branch from upstream `main` (or stacked on an open PR branch) and its own upstream PR. Keep "Allow edits from maintainers" on.
- Work in a separate git worktree for each branch. Remove the worktree after the branch is pushed and merged into `tao`.
- Never print secrets. `~/.superci` holds sign-in keys.
