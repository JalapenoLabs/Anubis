#!/usr/bin/env bash
#
# The real-scaffold proof: every scaffolder in the family, run against the real
# starter application, with the generated code held to the bar hand-written
# code is held to. rustfmt on untouched output, clippy with warnings denied,
# the generated narratives against a real Postgres, and the frontend
# typecheck, lint, test, and production build over the generated TypeScript.
#
# CI runs this on every push and pull request, as the Scaffold job in
# .github/workflows/ci.yml. Run it yourself before touching a living template,
# with `yarn dev` stopped:
#
#   docker compose --env-file .env.example -f starter/compose.yaml up -d --wait
#   DATABASE_URL=postgres://anubis_starter:anubis-starter-dev-password@localhost:54321/anubis_starter_development \
#     bash scripts/ci-scaffold-proof.sh
#
# It writes generated models into starter/ and leaves them there, so the output
# is yours to read. Take the tree back over the three paths a scaffold writes
# into, rather than over starter/ as a whole, so nothing else you have in
# flight is in the blast radius:
#
#   paths='starter/backend starter/config starter/frontend/src'
#   git clean -fdn $paths                       # review what would go
#   git checkout -- $paths && git clean -fd $paths
#
# Read the dry run before the real one: the clean removes every untracked file
# under those paths, which is the generated domain plus anything else you had
# not committed yet.
#
# Windows developers run this through git-bash, so it stays POSIX: no GNU-only
# flags, no process substitution, and forward slashes throughout. Stop a
# running `anubis-starter` first: Windows locks a running executable, and cargo
# cannot relink one it cannot replace.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# Models left behind by an earlier run would stop the sequence part way
# through, since the scaffolder never overwrites a module. Other uncommitted
# changes are welcome: proving an edited living template is the reason to run
# this by hand. CI checks out clean, so this only ever fires locally. The list
# names the modules the sequence below generates, and moves when it does.
leftovers=""
for module in projects tickets goals tasks tags applied_tags stripe_webhooks; do
  if [ -e "starter/backend/src/$module" ]; then
    leftovers="$leftovers starter/backend/src/$module"
  fi
done
if [ -n "$leftovers" ]; then
  echo "error: an earlier proof left its generated models behind:$leftovers" >&2
  echo "       Take those paths back before running this again:" >&2
  echo "         paths='starter/backend starter/config starter/frontend/src'" >&2
  echo "         git checkout -- \$paths && git clean -fd \$paths" >&2
  exit 1
fi

# The generated narratives skip themselves when this is unset, and a proof that
# silently proves nothing is worse than no proof at all. Point it at a database
# nothing else is using: a development server sharing it drains the webhook
# deliveries the narratives assert on, through its own job worker, and they
# then read as failed what the test left pending.
if [ -z "${DATABASE_URL:-}" ]; then
  echo "error: DATABASE_URL is unset, and the generated narratives skip without it." >&2
  echo "       Start the development database and point this at it:" >&2
  echo "         docker compose --env-file .env.example -f starter/compose.yaml up -d --wait" >&2
  exit 1
fi

# Echoes a command, then runs it, so the log reads as the procedure itself.
run() {
  echo
  echo "+ $*"
  "$@"
}

# Runs one scaffolder inside the starter, which is the application the
# scaffolder finds by walking up from its working directory.
scaffold() {
  echo
  echo "+ anubis scaffold $*"
  (cd "$root/starter" && cargo run --quiet --bin anubis -- scaffold "$@")
}

echo "== scaffolding a domain into starter/ =="

# One domain, covering every scaffolder and every shape they generate: a
# team-owned model with extra fields, a field added afterwards, the field types
# the templates prove, all three ownership depths with each nested model
# attaching itself to its parent's page, a field added at the deepest one, a
# join and the has-many-through field that reads through it, both flavours of
# belongs_to on two ownership depths, a sign-in provider, and an incoming
# webhook receiver.
scaffold model Project Team name:text_field description:text_area
scaffold field Project priority:text_field
scaffold model Ticket Team title:text_field urgency:number_field open:boolean due_date:date_field
scaffold model Goal Project,Team name:text_field
scaffold model Task Goal,Project,Team name:text_field
scaffold field Task effort:number_field
scaffold model Tag Team name:text_field
scaffold join AppliedTag "project_id{class_name=Project}" "tag_id{class_name=Tag}"
scaffold field Project "tag_ids:super_select{class_name=Tag}"
# Bullet Train's signature assignment, on a team-owned model and on a nested
# one, then the same field pointed at an application model with `source` spelled
# out. The membership narratives assign a real person; the model-backed one
# proves the scoping a fresh team sees.
scaffold field Project "lead_id:super_select{class_name=TeamMembership}"
scaffold field Goal "owner_id:super_select{class_name=TeamMembership}"
scaffold field Ticket 'reviewer_id:super_select{"class_name=Tag,source=team.tags"}'
scaffold oauth google
scaffold webhook Stripe

echo
echo "== holding the generated code to the full bar =="

# Generated Rust is formatted as it is written, so the formatter must have
# nothing left to say about it.
run cargo fmt --all -- --check
run cargo clippy --workspace --all-targets -- -D warnings

# The generated narratives, against the Postgres in DATABASE_URL. Scoped to
# the application on purpose: the framework's own scaffolding test copies the
# starter and scaffolds `Project` into the copy, and by now this tree has one.
run cargo test -p anubis-starter

# The generated TypeScript: the route modules, forms, pages, urls, routes,
# navigation, locales, and the permissions module `scaffold model` recompiled
# from the roles it granted.
run corepack yarn install --immutable
run corepack yarn typecheck
run corepack yarn lint
run corepack yarn test
run corepack yarn build

echo
echo "== the generated code meets the bar =="
echo "starter/ still holds the generated domain. Take it back with:"
echo "  paths='starter/backend starter/config starter/frontend/src'"
echo "  git clean -fdn \$paths                       # review what would go"
echo "  git checkout -- \$paths && git clean -fd \$paths"
