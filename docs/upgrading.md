# Upgrading

Framework behavior lives in two versioned dependencies, the `anubis` crate and
the `@jalapenolabs/anubis` package. Upgrading an application is therefore
bumping two version numbers and re-running the generators. There is no starter
template to merge, and no upstream branch to reconcile.

The crate publishes as `anubis-framework` and an application renames it back to
`anubis` with Cargo's `package` key, so every line below writes `anubis` except
the two that name a package to crates.io, `cargo install` and `cargo update`.
[ci.md](ci.md#the-crate-name) has the reason.

That is the whole design. Bullet Train's most-cited long-term cost is the
upgrade: an application merges the upstream starter repository tag by tag,
resolves conflicts in files it has been editing for a year, and hand-diffs
every ejected view, because git reports no conflict on a file that was copied
rather than tracked. Anubis pays that cost once, at `anubis new`, by keeping
framework behavior out of the stamped tree entirely.

`anubis upgrade` is the command. It is described in full below.

## What a version number means

Anubis is pre-1.0, and the numbers say so.

| Change | What it may do |
|---|---|
| `0.x.0`, a minor | Break. The release notes name every break and every step it takes by hand |
| `0.x.y`, a patch | Fix behavior without breaking a compiling application |

Cargo and npm both treat a `0.x` minor as a major version, so `^0.2.0` never
resolves to `0.3.0` on either end. Moving across a minor is always a deliberate
edit to a manifest, which is exactly the edit `anubis upgrade` writes. An
application that never runs it never moves.

Every release ships notes. They are a GitHub Release cut from the tag, at
`https://github.com/JalapenoLabs/Anubis/releases/tag/v<version>`, and
`anubis upgrade` prints that URL for the version it moved to. A release that
breaks something says what broke, in what direction, and what to do about it.
Reading the notes is the manual step no tool replaces.

`1.0` turns this into ordinary semver: a major may break, a minor adds, a patch
fixes.

## What moves together

The crate and the package ship as one release and carry one version. An
application depends on both, and a version naming only half of a release is a
version nobody can resolve.

Three declarations in this repository state that version, and
`scripts/check-release-version.sh` cross-checks them:

| Where | What |
|---|---|
| `Cargo.toml`, `[workspace.package] version` | The crate's version |
| `Cargo.toml`, `[workspace.dependencies] anubis` | What the starter resolves the crate to |
| `frontend/package.json`, `version` | The package's version |

The release workflow runs that script before it publishes anything, so a
release that disagrees with itself never reaches a registry. The script also
checks that the workspace entry still renames the package `anubis/Cargo.toml`
declares: the crate publishes as `anubis-framework`, and an alias that drifts
leaves a starter that resolves nothing. See [ci.md](ci.md#releases) for the
release procedure and [the crate name](ci.md#the-crate-name) for the rename.

## Upgrading an application

```sh
git switch -c upgrade-anubis-0.3.0

cargo install anubis-framework --version 0.3.0   # the CLI that will regenerate
anubis upgrade                                   # or: anubis upgrade --to 0.3.0

cargo test --workspace
yarn test
```

Install the CLI first, and install the version you are moving to. `anubis
upgrade` regenerates the application's generated files with the framework
library compiled into the CLI binary, so a stale CLI would write last version's
output over this version's contract. `anubis --version` reports which one is on
`PATH`.

Review the diff before you commit it. The manifests, the two lockfiles, and
whichever generated files the release changed are all that should appear;
anything else is the release notes talking.

## What the command does

`anubis upgrade` runs from anywhere inside an application and works on the
application root, the directory holding `backend/`, `frontend/`, and
`config/roles.yml`.

| Flag | Effect |
|---|---|
| (none) | Moves to the latest version crates.io carries |
| `--to <version>` | Moves to exactly that version, and never reaches the registry |
| `--dry-run` | Prints everything the run would do, and writes nothing |

In order, one run:

1. Reads the `anubis` requirement from `backend/Cargo.toml` and the
   `@jalapenolabs/anubis` requirement from `frontend/package.json`.
2. Resolves the target version, from `--to` or from crates.io.
3. Rewrites both requirements, keeping each one's range operator: `^0.2.0`
   becomes `^0.3.0`, and a bare `0.2.0` stays bare. Everything else in both
   manifests is copied through byte for byte.
4. Runs `cargo update -p anubis-framework` and `yarn install`, which is what
   moves the two lockfiles. The package spec is the published crate name, not
   the `anubis` the manifest writes.
5. Re-runs every generator the application carries: the permissions module
   from `config/roles.yml`, the plan catalog from `config/billing.yml` when
   there is one, and the API client from the document the application's own
   binary exports. These are the three files CI checks for drift, so the run
   ends with them matching the framework that will compile them.
6. Prints the release notes URL.

A step that fails stops the run with its own output on the terminal. The
manifests are written before the steps and stay written, because the bump is
the part worth keeping: fix what the output named and run the remaining steps
by hand. A rerun rewrites only the manifests still behind, so it is safe.

`--dry-run` prints the same list without touching anything, which is the way to
read an upgrade before taking it.

### What it deliberately does not do

- **It does not restamp the files `anubis new` wrote.** The CI workflow, the
  Dockerfile, `compose.yaml`, `.env.example`, and every generated model are the
  application's from the moment they land. A release that changes one of those
  templates names the edit in its notes; the application applies it or does
  not. This is the trade that buys the absent merge.
- **It does not improve an ejected component.** `anubis eject` copies a field
  component into the application and stamps the version it came from, and from
  then on the copy is the application's. Delete it to go back to the package's.
  See [scaffolding.md](scaffolding.md).
- **It does not upgrade across a break for you.** Nothing here reads the
  release notes and edits your code.
- **It does not run in CI.** An upgrade is a reviewed change on a branch, not a
  scheduled one. An upgrade-PR workflow, and a `--check` mode that fails when
  an application is behind, are both plausible follow-ups and neither is built.

## Before the first release

Until the crate and the package are published, applications track the framework
from its git repository: `anubis = { package = "anubis-framework", git = ... }`
in `backend/Cargo.toml` and a git URL in `frontend/package.json`, which is what
`anubis new` stamps today. Publishing changes the source, never the key.

There is no version to bump in that arrangement, and `anubis upgrade` says so
rather than inventing one. Moving a git-tracked application to the framework's
latest commit is two commands:

```sh
cargo update -p anubis-framework
yarn up @jalapenolabs/anubis
```

Then re-run the generators, exactly as the drift checks in the application's
own CI workflow run them:

```sh
anubis roles generate-ts --file config/roles.yml --out frontend/src/roles.generated.ts
anubis billing generate-ts --file config/billing.yml --out frontend/src/plans.generated.ts
cargo run --quiet -p <app> -- openapi > openapi.json
anubis client generate-ts --from openapi.json --out frontend/src/api/v1.generated.ts
```

Run `anubis upgrade` once both dependencies name a published version. The
command verifies that the crate crates.io answers with is this framework, by
the repository the crate declares, and refuses to move an application onto a
crate that is not. It refuses the same way at the other end: an `anubis` entry
with no `package = "anubis-framework"` resolves the unrelated crate holding the
plain name, so the command names the line to write instead of upgrading it. See
[the crate name](ci.md#the-crate-name).
