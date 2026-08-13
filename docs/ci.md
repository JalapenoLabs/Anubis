# CI

CI runs on GitHub Actions using Jalapeno Labs self-hosted runners, targeted with the labels `[self-hosted, rocky9, earthly, docker]`. The runners persist the cargo registry, target dir, and yarn cache between runs, so workflows carry no cache save/restore steps.

## Workflows

`.github/workflows/ci.yml` runs on pushes and pull requests to `main` and `develop`:

- **Rust job**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **Frontend job**: `yarn install --immutable`, then typecheck, lint, test, and build across all workspaces.

Two more workflows guard the branch flow rather than the code. Both are cheap string and history checks, and both belong in `main`'s required status checks:

- **`promotion-guard.yml`**: on pull requests targeting `main`, fails unless the source branch is `develop`.
- **`branch-ancestry.yml`**: on pushes to `main`, fails unless `develop` already contains the commit.

## Branch flow

`develop` must always contain `main`. Everything reaches `main` by promoting `develop`, so `main` is a strict subset of `develop` and the next promotion is always a clean diff.

Promote with a fast-forward, which satisfies the invariant by construction:

```sh
git fetch origin
git checkout main
git merge --ff-only origin/develop
git push origin main
```

A promotion merged through the GitHub UI instead adds a commit to `main` that `develop` does not have, so merge it back afterwards:

```sh
git checkout develop && git merge origin/main && git push origin develop
```

Squash-merging into `main` is the one thing to avoid. The squash is a new commit with content `develop` already has but no ancestry it shares, so the branches diverge while looking identical. Squash freely into `develop`, where nothing downstream tracks the feature branch.

## Version pinning

Every toolchain version is pinned and must stay in sync:

- Rust: `rust-toolchain.toml` and the `dtolnay/rust-toolchain` tag in the workflow.
- Node: pinned in the workflow's `setup-node` step.
- Yarn: `packageManager` in package.json and an explicit `corepack prepare` in the workflow.

## Earthly note

The runners carry the `earthly` label and Earthfiles remain welcome tooling, but api.earthly.dev is offline (the hosted Earthly service is sunsetted). Never rely on Earthly-hosted secrets or authentication; connection errors to api.earthly.dev at the start of runs are benign.
