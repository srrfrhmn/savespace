# syntax=docker/dockerfile:1
#
# SaveSpace production image (design doc §8.7).
#
# One image holds both binaries and the web app:
#   /app/api      HTTP API, also serves /app/web (default command)
#   /app/worker   job runner: `worker drain`, `worker migrate`
#
# Base images are pinned by digest. Dependabot updates them.
# Build: docker build -t savespace .

FROM rust:1.99-trixie@sha256:8a4e90c7946b74626488b1446f712c0fe0ed1a596cbd3d871684bc9c5433ae07 AS chef
WORKDIR /src
# The toolchain and the wasm32 target come from rust-toolchain.toml.
COPY rust-toolchain.toml .
RUN rustup show active-toolchain || rustup toolchain install
RUN cargo install cargo-chef --version 0.1.78 --locked \
 && cargo install wasm-pack --version 0.15.1 --locked

# Dependency recipe. This layer changes only when Cargo.toml or Cargo.lock change.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked -p api -p worker \
 && wasm-pack build bindings/wasm --target web --release \
      --out-dir pkg --out-name savespace_core --scope savespace

FROM node:24-trixie-slim@sha256:af02d57b57719b9657a45199a79264a9437940cd55b3c7b3fc99615654f124a0 AS web
WORKDIR /src
RUN npm install -g pnpm@12.16.1
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/web/package.json apps/web/
COPY docs/diagrams/tools/package.json docs/diagrams/tools/
COPY --from=builder /src/bindings/wasm/pkg bindings/wasm/pkg
RUN pnpm install --frozen-lockfile --filter "@savespace/web..."
COPY crates/core/tests/golden crates/core/tests/golden
COPY apps/web apps/web
RUN pnpm --filter @savespace/web build

# Runtime: no shell, no package manager, non-root user.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:cb87acc574abe719b6e5c547384defa2f7b1f24af9605d3f618cf852495d8be2
COPY --from=builder /src/target/release/api /src/target/release/worker /app/
COPY --from=web /src/apps/web/dist /app/web
ENV SAVESPACE_API_ADDR=0.0.0.0:8080 \
    SAVESPACE_WEB_DIR=/app/web \
    SAVESPACE_LOG_FORMAT=gcp \
    RUST_LOG=info
USER nonroot
EXPOSE 8080
CMD ["/app/api"]
