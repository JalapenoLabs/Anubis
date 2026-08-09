# Anubis

Anubis is an exciting, revoluationary project that's meant to be a framework that's extremely comparable to the (Ruby Bullet Train framework)[https://bullettrain.co].

However, it's core stack differs dramatically from Bullet Train. Here we use a rust-first ecosystem, with a full Rust backend.
Additionally, we use a React.js client side rendered SPA with HeroUI and TailwindCSS.
We don't use the same Ruby and SSR stack that Bullet Train uses, but offer as much as we can of everything else.

Additionally, we're tied closely to Jalapeno Labs and use:
- (Arsox Satellites)[https://github.com/JalapenoLabs/arsox-satellites] (for remote LLM job management)
- (UI Kit)[https://github.com/JalapenoLabs/uikit] (for re-usable UI components)
- (CLI)[https://github.com/JalapenoLabs/cli] (for utils like eslint standardization)
- (Brand)[https://github.com/JalapenoLabs/brand] (for Jalapeno Labs branding + styles + themes)
- (Github Actions)[https://github.com/JalapenoLabs/github-actions] (Shared, re-usable github actions workflow items and flows)

For full details, reference the @./README.md
We have a @./docs folder, for all documentation.
We use Github Issues for tracking tech debt and future work, and use milestones as "epics" for grouping a collection of tasks together.
We have @./bullet_train which has documentation of bullet train, in which we fully model and we submodule the main branch of their repository at bullet_train/bullet_train for reference.
We have a full research report about what bullet train is, in @./bullet_train/bullet-train-deep-research.md
That report covers the framework's identity and history, current MIT/open-source licensing (including the fully open-sourced former Pro tier), modular gem architecture, Super Scaffolding code generator, team/membership multi-tenancy model, roles/permissions via CanCanCan, front-end stack (Tailwind + Hotwire + ERB), auto-generated REST API, developer workflow, deployment options, community health, real-world adopters, comparisons with Jumpstart Pro and vanilla Rails, demo resources, a categorized feature list, staged recommendations for a senior frontend engineer, and caveats around stale pricing sources and front-end lock-in.

# **Source-of-truth policy.**

The USER is the primary source of truth. This `CLAUDE.md` is the SECOND source of truth. The codebase is **not** a source of truth (it can drift). Whenever a design decision is made or changed, update this file in the same change. Keep it routinely up to date.

# Style

No em dashes anywhere in user-facing text (docs, UI, commits, PRs).

# Git

@./README.md

This repo is open-source, public.
The main branch is production (stable), the develop branch is the working branch (unstable) that PRs push into.
If you're a Stakeholder (such as `navarrotech`) then you may push commits straight to develop.
Else you will be required to make a pull request for all other commits.

Stakeholders are responsible for promoting develop to main.
