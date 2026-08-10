# CI

CI runs on GitHub Actions using Jalapeno Labs self-hosted runners, targeted with the labels `[self-hosted, fedora, earthly, docker]`. The runners persist the cargo registry, target dir, and yarn cache between runs, so workflows carry no cache save/restore steps.

## Workflow

`.github/workflows/ci.yml` runs on pushes and pull requests to `main` and `develop`:

- **Rust job**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **Frontend job**: `yarn install --immutable`, then typecheck, lint, test, and build across all workspaces.

## Version pinning

Every toolchain version is pinned and must stay in sync:

- Rust: `rust-toolchain.toml` and the `dtolnay/rust-toolchain` tag in the workflow.
- Node: pinned in the workflow's `setup-node` step.
- Yarn: `packageManager` in package.json and an explicit `corepack prepare` in the workflow.

## Earthly note

The runners carry the `earthly` label and Earthfiles remain welcome tooling, but api.earthly.dev is offline (the hosted Earthly service is sunsetted). Never rely on Earthly-hosted secrets or authentication; connection errors to api.earthly.dev at the start of runs are benign.
