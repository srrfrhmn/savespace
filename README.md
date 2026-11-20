# savespace

An anxiety conscious personal finance app. SaveSpace converts raw numbers into calm, true, contextual insights.

## Status

Phase 1 (web MVP) is live for one user, the owner, on real bank accounts in the US and Canada (Plaid Trial plan). It runs on Google Cloud Run and Neon at $0 per month. See design doc [§16](docs/design-doc.md#16-roadmap) for the roadmap and the Phase 1 exit criteria.

## Repository layout

```
savespace/
├── crates/
│   ├── core/                    # savespace-core: Money, currency, baselines, formatting (no I/O)
│   ├── api/                     # HTTP API (axum). Serves the web app. GET /health, /ready
│   ├── worker/                  # Job runner: run, drain, migrate
│   ├── domain-*/                # Domains: identity, sync, categorize, ledger, recurring, insights, notify
│   ├── aggregator-plaid/        # Plaid client and webhook checks
│   ├── platform-crypto/         # Envelope encryption for stored tokens
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
├── clients/                     # OpenAPI spec and the generated TypeScript client
├── e2e/                         # Playwright journey against the Plaid sandbox
├── infra/                       # Terraform (Google Cloud), ops scripts, runbook
├── evals/insights/              # LLM eval sets (planned; LLM text is off, D10)
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
