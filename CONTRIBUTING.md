# Contributing to SaveSpace

Read the [design document](docs/design-doc.md) first. Sections 8 (architecture) and 14 (engineering practices) apply to every change.

## Setup

Requirements:

| Tool | Version | Install |
| --- | --- | --- |
| Rust | from `rust-toolchain.toml` | [rustup](https://rustup.rs). The toolchain and the `wasm32` target install on first use. |
| Node.js | 24 or later | [nodejs.org](https://nodejs.org) |
| pnpm | from `package.json` (`packageManager`) | `npm i -g pnpm` |
| wasm-pack | latest | `cargo install wasm-pack --locked` |
| cargo-nextest | latest | `cargo install cargo-nextest --locked` |
| cargo-deny | latest | `cargo install cargo-deny --locked` |
| Docker | Docker Desktop (Windows, macOS) or Docker Engine | [docker.com](https://www.docker.com) |
| sqlx-cli | latest | `cargo install sqlx-cli --no-default-features --features postgres,rustls --locked` |

Then:

```sh
pnpm wasm          # build savespace-core to WASM (bindings/wasm/pkg)
pnpm install
cp .env.example .env
docker compose up -d --wait   # PostgreSQL 18 on 127.0.0.1:5432
sqlx migrate run --source crates/platform-db/migrations
```

Run `pnpm wasm` again after you change `crates/core` or `bindings/wasm`.

## Commands

| Task | Command |
| --- | --- |
| Rust tests | `cargo nextest run --workspace` |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Rust format | `cargo fmt --all` |
| Dependency rule (§8.3) | `cargo xtask check-deps` |
| Licenses and advisories | `cargo deny check` |
| Run the API | `cargo run -p api` (listens on `127.0.0.1:8080`, `GET /healthz`) |
| Run the worker | `cargo run -p worker` |
| Web dev server | `pnpm --filter @savespace/web dev` |
| Web lint and format | `pnpm lint` / `pnpm format` |
| Web typecheck, test, build | `pnpm typecheck` / `pnpm test` / `pnpm build` |
| Rebuild diagrams | `pnpm diagrams` |
| Start / stop PostgreSQL | `docker compose up -d --wait` / `docker compose down` |
| Apply migrations | `sqlx migrate run --source crates/platform-db/migrations` |
| Database shell | `docker compose exec postgres psql -U savespace` |

CI runs all of these on every pull request (`.github/workflows/ci.yml`). CI and `compose.yaml` use the same `postgres:18` image.

## Rules

- **Money.** Use `savespace_core::Money`. Never use floating-point numbers for money. Add property tests for new money math.
- **One source of truth.** Put calculations that a client shows in `crates/core`. Do not repeat them in TypeScript, Swift, or Kotlin.
- **Golden files.** If you change formatting, update `crates/core/tests/golden/format.json`. The Rust test and the WASM test both read it.
- **Crate layers.** `api`/`worker` → `domain-*` → `platform-*` → `savespace-core`. A domain crate does not depend on another domain crate. `cargo xtask check-deps` enforces this.
- **Migrations.** Forward-only. Expand and contract (§9.3).
- **Design review.** Write an RFC (`docs/rfcs/0000-template.md`) for a new table, external dependency, insight rule, or data flow (§14.4).
- **Behavioral review.** Changes to insights, notifications, or copy follow design doc §5 and Appendix B. The pull request template has a checklist.

## Writing

Documentation follows the ASD-STE100-inspired rules in design doc [Appendix A](docs/design-doc.md#appendix-a-writing-conventions): short sentences, active voice, one term for one meaning.

## Branches and commits

- Branch from `main`. Keep branches short-lived.
- One logical change per pull request. CI must pass before merge.
