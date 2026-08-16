# The production image: one binary that serves the API and the built SPA.
#
#   docker build -t anubis-starter .
#   docker run --rm -p 3000:3000 \
#     -e DATABASE_URL=postgres://user:password@host/app \
#     -e APP_URL=https://app.example.com \
#     -e ANUBIS_SECRET_KEY="$(anubis secret generate)" \
#     anubis-starter
#
# Three stages, so the image that ships carries neither toolchain: the Rust
# build, the frontend build, and a runtime that holds the binary, the bundle,
# and nothing else. Both build stages need a committed lockfile (`Cargo.lock`,
# `yarn.lock`); production builds resolve nothing.
#
# In the Anubis monorepo this file is the template rather than a buildable
# image: the starter compiles against the framework crate in the parent
# workspace, which lives outside this directory. `anubis new` stamps it into an
# application where the paths below are the application's own, and that is
# where it builds.

# ---- Stage 1: the server binary ---------------------------------------------
# Pinned to the same toolchain as rust-toolchain.toml, on the same Debian
# release as the runtime stage, so the glibc the binary links against is the
# glibc that runs it.
FROM rust:1.96.0-slim-trixie AS backend

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY backend/Cargo.toml backend/Cargo.toml
COPY backend/src backend/src
# Migrations and the two config files are compiled into the binary
# (`embed_migrations!` and `include_str!`), so they are build inputs rather
# than files the runtime image needs.
COPY backend/migrations backend/migrations
COPY config config

# Cache mounts rather than the usual dummy-crate trick: no fake source files to
# go stale, and the registry and target directory survive between builds, so a
# source-only change recompiles one crate instead of the whole graph. A runner
# with a cold cache pays the full build, which is the honest trade. The binary
# is copied out of the target directory because a cache mount leaves no layer
# behind.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p anubis-starter \
    && cp target/release/anubis-starter /usr/local/bin/anubis-starter

# ---- Stage 2: the frontend bundle -------------------------------------------
# Pinned to the Node and Yarn versions CI runs, so a bundle built here is the
# bundle the tests ran against.
FROM node:24.13.0-trixie-slim AS frontend

WORKDIR /src

# The framework's npm package installs from git until it is published. The slim
# image ships neither a git client nor a certificate store to verify github.com
# with, and this is the only stage that needs either: the server binary carries
# its own roots, and the runtime image below installs nothing.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

# Manifests first, so the install layer is reused until a dependency changes,
# and Yarn's global cache on a mount, so even a changed dependency re-fetches
# only itself. The cache earns its keep on the packages that install from git:
# Yarn has to clone and pack those rather than download a tarball.
COPY package.json yarn.lock .yarnrc.yml ./
COPY frontend/package.json frontend/package.json
RUN --mount=type=cache,target=/root/.yarn/berry/cache,sharing=locked \
    corepack enable \
    && corepack prepare yarn@4.17.0 --activate \
    && yarn install --immutable

# Then the sources the bundler reads. Tests, end-to-end specs, and lint
# configuration are deliberately absent: they are not inputs to a build.
COPY frontend/index.html frontend/index.html
COPY frontend/tsconfig.json frontend/tsconfig.json
COPY frontend/vite.config.ts frontend/vite.config.ts
COPY frontend/src frontend/src
RUN yarn workspace anubis-starter-frontend build

# ---- Stage 3: the runtime ---------------------------------------------------
# Debian slim rather than distroless. The binary links glibc and nothing else:
# rustls carries its own root certificates and diesel-async speaks the Postgres
# protocol in Rust, so there is no libssl and no libpq to install. The image
# lands around 122MB, of which 79MB is this base, 29MB the binary, 14MB the curl
# below, and the rest the frontend bundle. Distroless would save perhaps 50MB and
# take with it both the shell an incident wants and the only client a container
# healthcheck could run.
FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /app --shell /usr/sbin/nologin --uid 10001 app

WORKDIR /app
COPY --from=backend /usr/local/bin/anubis-starter /usr/local/bin/anubis-starter
COPY --from=frontend /src/frontend/dist /app/public

# The deployment shape, not the deployment's secrets: DATABASE_URL, APP_URL,
# and ANUBIS_SECRET_KEY are supplied at run time, and production refuses to boot
# without them. HOST is the one value a container must override, since the
# framework binds the loopback by default and nothing outside the container
# could reach that.
ENV ANUBIS_ENV=production \
    HOST=0.0.0.0 \
    PORT=3000 \
    SPA_DIR=/app/public

USER app
EXPOSE 3000

# Liveness, the same question an orchestrator asks: it touches no dependency, so
# a database blip drains traffic instead of restarting the container.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT}/healthz" || exit 1

# Exec form, so the server is PID 1 and receives SIGTERM itself: it stops
# accepting, drains what is in flight, and exits on its own terms.
ENTRYPOINT ["anubis-starter"]
