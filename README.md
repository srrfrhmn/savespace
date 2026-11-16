# savespace

An anxiety conscious personal finance app. SaveSpace converts raw numbers into calm, true, contextual insights.

## Status

Phase 0 (foundations). See design doc §16 for the roadmap.

## Repository layout

```
savespace/
├── crates/
│   ├── core/                    # savespace-core: Money, currency, baselines, formatting (no I/O)
│   ├── api/                     # HTTP API (axum). Serves the web app. GET /health, /ready
│   ├── worker/                  # Job runner: run, drain, migrate
│   ├── domain-*/                # Domain libraries (Phase 1)
│   ├── aggregator-plaid/        # Plaid adapter (Phase 1)
│   ├── platform-db/             # Pool, job queue, migrations/
│   ├── platform-observability/  # tracing setup
│   └── xtask/                   # Repo automation: cargo xtask check-deps
├── bindings/
│   ├── wasm/                    # savespace-core for the web app (wasm-bindgen)
│   └── uniffi/                  # savespace-core for iOS and Android (UniFFI)
├── apps/
│   ├── web/                     # React + TypeScript (Vite)
│   ├── ios/                     # Phase 2 placeholder
│   └── android/                 # Phase 3 placeholder
├── clients/                     # Generated API clients (Phase 1)
├── infra/                       # Terraform (Google Cloud), ops scripts, runbook
├── evals/insights/              # LLM eval sets (Phase 1)
└── docs/                        # Design doc, diagrams, RFCs
```

## Quick start

```sh
pnpm wasm && pnpm install
docker compose up -d --wait                # PostgreSQL 18
sqlx migrate run --source crates/platform-db/migrations
cargo nextest run --workspace
cargo run -p api                          # http://127.0.0.1:8080/health
pnpm --filter @savespace/web dev          # web app
```
