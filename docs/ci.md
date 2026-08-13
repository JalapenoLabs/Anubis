# CI

CI runs on GitHub Actions using Jalapeno Labs self-hosted runners, targeted with the labels `[self-hosted, rocky9, earthly, docker]`. The runners persist the cargo registry, target dir, and yarn cache between runs, so workflows carry no cache save/restore steps.

## Workflow

`.github/workflows/ci.yml` runs on pushes and pull requests to `main` and `develop`:

- **Rust job**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the roles and client drift checks, then `cargo audit`.
- **Frontend job**: `yarn install --immutable`, then typecheck, lint, test, build, and `yarn npm audit`.

## Dependency audit

Both jobs end with a vulnerability audit: `cargo audit` against RustSec advisories, and `yarn npm audit --all --recursive` against npm's. Both run `continue-on-error: true`, so a fresh advisory published overnight reports on every push without blocking unrelated work from merging. Flip both to blocking once the reports are routinely empty and a new advisory is something the team wants to fix before merging.

`cargo-audit` is installed idempotently (`command -v cargo-audit || cargo install cargo-audit --locked`). The runners keep `~/.cargo/bin` between runs, so only the first run on a fresh runner pays the few minutes it takes to compile.

## Version pinning

Every toolchain version is pinned and must stay in sync:

- Rust: `rust-toolchain.toml` and the `dtolnay/rust-toolchain` tag in the workflow.
- Node: pinned in the workflow's `setup-node` step.
- Yarn: `packageManager` in package.json and an explicit `corepack prepare` in the workflow.

## Earthly note

The runners carry the `earthly` label and Earthfiles remain welcome tooling, but api.earthly.dev is offline (the hosted Earthly service is sunsetted). Never rely on Earthly-hosted secrets or authentication; connection errors to api.earthly.dev at the start of runs are benign.
