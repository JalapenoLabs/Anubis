# Anubis

Anubis is an open-source SaaS framework: the developer experience of [Bullet Train](https://bullettrain.co), rebuilt on a Rust backend and a React SPA frontend.

Bullet Train proved that the same-in-every-SaaS plumbing (teams, roles, auth, a versioned public API, webhooks, billing) belongs in a framework, and that a great code generator turns domain modeling into the highest-leverage activity in application development. Anubis brings that experience to teams who want Rust and React instead of Ruby and ERB.

## The stack

- **Backend**: Rust on tokio, Axum, tower, Diesel (+ diesel-async), PostgreSQL, optional Redis for realtime and caching.
- **Frontend**: React SPA with Vite, TypeScript, HeroUI, TailwindCSS, Redux Toolkit, React Router, ky + SWR, react-hook-form + zod, i18next.
- **Contract**: OpenAPI 3.1 generated from the Rust handlers, driving a generated, fully typed TypeScript client.
- **Codegen**: `anubis scaffold` produces migrations, models, permissions, handlers, API docs, React pages, navigation, locale files, and tests from one command, using Bullet Train's living-templates philosophy.

Because both ends are statically typed, a scaffold either compiles end to end or tells you exactly what to fix. That is the promise Rails can never make.

## Status

Pre-alpha. The architecture is settled and documented in [docs/](docs/); implementation is underway, tracked through GitHub milestones M1 through M5.

## Documentation

- [Architecture](docs/architecture.md)
- [Tenancy, teams, and organizations](docs/tenancy.md)
- [Scaffolding](docs/scaffolding.md)
- [REST API](docs/api.md)
- [Background jobs](docs/jobs.md)

## Repository layout

- `anubis/`: the single Cargo package (framework library + `anubis` CLI binary)
- `frontend/`: the single npm package (`@jalapenolabs/anubis`)
- `starter/`: the template stamped by `anubis new`, and the CI host app for the scaffolding templates
- `docs/`: one document per decision category
- `bullet_train/`: Bullet Train reference material (docs, submodule, research report)

## Contributing

`main` is production (stable); `develop` is the working branch. Stakeholders push to `develop` directly; everyone else opens a pull request into `develop`.

## License

MIT. Anubis is a Jalapeno Labs project.
