# End-to-end tests

One Playwright journey against the Plaid sandbox (design doc §14.3): setup
with a virtual passkey, connect, sync, transactions, budget, recurring,
check-in, reflection, the insight review, settings, export, and account
deletion. Every screen gets an axe check (WCAG 2.2 AA, light and dark) and a
375 px width check.

The journey needs an **empty database**. It deletes the account at the end, so
it can run again.

## Run locally

1. Start PostgreSQL (`docker compose up -d --wait`), build the web app
   (`pnpm --filter @savespace/web build`), and migrate an empty database
   (`worker migrate` with the owner URL).
2. Start the api with `SAVESPACE_WEB_DIR=apps/web/dist` and the worker
   (`worker run`), both with Plaid sandbox keys, `SAVESPACE_TOKEN_KEK`, and the
   runtime role (see `.env.example`).
3. Run, with the same Plaid sandbox keys in the environment:

   ```sh
   pnpm --filter @savespace/e2e exec playwright install chromium   # once
   pnpm --filter @savespace/e2e e2e
   ```

Settings: `E2E_BASE_URL` (default `http://localhost:8080`), `E2E_SETUP_CODE`
(default `local-setup-code`), and `E2E_CHANNEL=msedge` to use the installed
Edge instead of Playwright's Chromium.

## Run in GitHub Actions

Actions, **e2e**, **Run workflow**. It needs the repository secrets
`PLAID_SANDBOX_CLIENT_ID` and `PLAID_SANDBOX_SECRET` (sandbox keys only). On a
failure it uploads the Playwright report, traces, and the server logs.
