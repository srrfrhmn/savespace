# SaveSpace threat model

STRIDE threat model for the Phase 1 web MVP (design doc §12.7). Update this file in the same pull request as any change to authentication, sessions, tenancy, secrets, or a new external input such as a webhook.

Scope: one owner (D8), the Cloud Run service `savespace-api`, the Cloud Run jobs, Neon PostgreSQL, Secret Manager, GitHub Actions, and the browser.

Status key: **Done** = built and tested. **Planned** = in a later milestone, with the milestone named.

## Assets

| Asset | Where | Why it matters |
| --- | --- | --- |
| Session tokens | Browser cookie. Server stores the SHA-256 hash only. | A token gives full access to the account. |
| Passkey public keys and counters | `app.passkeys` | A changed key would let an attacker sign in. |
| Recovery codes | Shown once. Server stores the SHA-256 hash only. | A code signs in once and allows step-up actions. |
| Owner setup code | Secret Manager `owner-setup-code` | Registers the first passkey while no owner exists. |
| Plaid access tokens (M2) | `app.connections`, encrypted with the household key | Read access to bank data. |
| Financial data (M2+) | Neon | Personal and sensitive. |
| Database URLs, `token-kek`, Plaid secret | Secret Manager | Full data access. |
| Deploy rights | GitHub Actions through Workload Identity Federation | Code runs in production. |

## Trust boundaries

1. Browser to the API (public internet, TLS from Cloud Run).
2. API and worker to Neon (TLS, `sslmode=verify-full`, runtime role).
3. API and worker to Plaid (M2: TLS, client secret).
4. Plaid webhooks to the API (M2: public input, JWT signature).
5. GitHub Actions to Google Cloud (OIDC, `main` branch only).

## Authentication and sessions (M1)

| STRIDE | Threat | Mitigation | Status |
| --- | --- | --- | --- |
| Spoofing | Phishing site collects a sign-in. | Passkeys are bound to the relying party ID (the Cloud Run host name). A phishing site cannot use them. No passwords. | Done |
| Spoofing | Attacker registers a passkey before the owner. | Setup needs the one-time setup code from Secret Manager, compared in constant time. Setup closes when an owner exists. An advisory lock makes two parallel setups safe. | Done |
| Spoofing | Recovery code guessing. | 60 bits per code. Single use, marked used in the same statement. Rate limit: 20 sign-in requests per minute per client, 100 in all. Failed attempts go to the audit log. | Done |
| Spoofing | Stolen session cookie. | `__Host-` cookie, `HttpOnly`, `Secure`, `SameSite=Strict`. 30 days idle, 90 days absolute. Sign-out deletes the server row. Step-up actions need a passkey check in the last 5 minutes. | Done |
| Spoofing | Cloned authenticator. | webauthn-rs checks the signature counter. The stored passkey is updated after each use. | Done |
| Tampering | Cross-site request forgery. | `SameSite=Strict` cookie. Unsafe methods need an `Origin` header equal to the public origin. JSON bodies need the JSON content type. | Done |
| Tampering | Replay of a WebAuthn response. | Each challenge is stored server-side, expires in 5 minutes, and is deleted when used. Challenges for signed-in actions are bound to the session. | Done |
| Tampering | Duplicate state changes from retries. | `Idempotency-Key` store per household (§11.3). A key reused for another request gets 409. | Done |
| Repudiation | Owner or attacker denies an action. | Append-only `app.audit_log` for setup, sign-in, sign-out, step-up, passkey, and recovery code events, with the request ID. The runtime role cannot update or delete it. | Done |
| Information disclosure | Session tokens or codes in the database or logs. | Only SHA-256 hashes are stored. Logs never contain tokens, codes, or request bodies. | Done |
| Information disclosure | Error messages reveal internals. | Errors use the §11.4 shape with stable codes. Database errors return a generic `api_error` and are logged server-side. | Done |
| Denial of service | Sign-in flooding. | In-memory rate limit (the MVP runs 1 instance at most). Cloud Run max instances = 1 caps cost. | Done |
| Elevation of privilege | A session without step-up changes security settings. | Adding or removing passkeys and new recovery codes need step-up. The last passkey cannot be removed. | Done |

## Tenancy and data access

| STRIDE | Threat | Mitigation | Status |
| --- | --- | --- | --- |
| Information disclosure | A bug reads another household's rows. | Row-level security on every tenant table, keyed on `app.household_id`, set per transaction by `platform_db::tenant_tx`. The runtime role is not the table owner, so RLS always applies. Tests run as the runtime role and try cross-household reads and writes. | Done |
| Elevation of privilege | Pre-sign-in lookups bypass RLS. | A few narrow `SECURITY DEFINER` functions, each with a fixed `search_path`. `EXECUTE` is granted to the runtime role only. The category seed runs from a trigger and is not callable by the runtime role. | Done |
| Tampering | The app changes the schema. | The runtime role has DML only. Migrations run as the owner role in a separate job. | Done |
| Information disclosure | Backups leak. | `pg_dump` encrypted with `age` before upload. The backup service account can create objects only. | Done |
| Information disclosure | Plaid tokens leak from the database. | Envelope encryption (`platform-crypto`): AES-256-GCM with a household data key, wrapped by `token-kek`. The associated data binds each ciphertext to its purpose and household. Decrypted keys stay in memory for 5 minutes at most. Deleting the household key makes the tokens unreadable. | Done |
| Tampering | A duplicate or late sync overwrites newer data. | The sync compares the stored cursor before it writes. Raw provider data is append-only (the runtime role cannot delete it). | Done |
| Elevation of privilege | A request uses another household's category, merchant, or rule ID. Foreign keys do not apply RLS. | `domain-ledger` and `domain-categorize` look up each ID inside the household's transaction (RLS applies) before they use it. Tests try another household's IDs in edits, splits, rules, and budget plans. | Done |
| Tampering | A person's edit is lost or changed by automation. | Edits are an overlay (`user_overrides`). Enrichment never writes to it. A pending-to-posted change moves the edit. | Done |
| Denial of service | A webhook flood starts many worker runs. | Webhooks must be signed. Worker starts are at most one per 20 seconds. The worker job runs one task at a time, and Cloud Run caps cost. | Done |

## External inputs

| STRIDE | Threat | Mitigation | Status |
| --- | --- | --- | --- |
| Spoofing | Forged Plaid webhook. | Verify the ES256 JWT in `Plaid-Verification` (key from `/webhook_verification_key/get`, cached, expired keys rejected) and the body SHA-256 (constant-time). Reject `iat` older than 5 minutes. Store the raw webhook and return 200. No session, no `Origin` check: the signature is the authentication. | Done |
| Tampering | SQL wildcards or injection through search. | Search text is a bound parameter. `%`, `_`, and `\` are escaped before `ILIKE`. Search has a 100-character limit. Notes and merchant names have length limits and reject control characters. | Done |
| Tampering | Script injection in the web app. | CSP: `default-src 'self'`, no inline scripts or styles, `frame-ancestors 'none'`. React escapes output. The only additions are Plaid Link's script URL, its frame origin, and the Plaid API origin for the current environment. Plaid's script also adds an inline style whose content changes on each load. The CSP blocks it, and Link works without it. | Done |
| Spoofing | A stolen session connects an attacker's bank or removes a connection. | Link tokens and connection removal need step-up (a passkey check in the last 5 minutes). | Done |
| Information disclosure | Host-name confusion between the two `run.app` URLs. | The API redirects every host name except the public origin. Passkeys work only on the public origin. | Done |

## Supply chain and deployment

| STRIDE | Threat | Mitigation | Status |
| --- | --- | --- | --- |
| Tampering | A compromised dependency. | `cargo deny` (advisories, licenses, sources), pinned lockfiles, Dependabot, actions pinned by SHA, base images pinned by digest. | Done |
| Elevation of privilege | A pull request deploys code. | Deploys run only for `main` after CI. The Workload Identity condition requires the repository ID and `refs/heads/main`. No service account keys. | Done |
| Tampering | An older CI run rolls production back. | The deploy gate deploys a commit only if it is still the newest on `main`. | Done |

## Accepted risks (Phase 1)

- **One region, no WAF.** Cloud Run's front end only. Acceptable for one user. The target infrastructure (§8.7) adds a WAF.
- **In-memory rate limits.** They reset when the instance restarts. Max 1 instance keeps them meaningful.
- **No external penetration test yet.** Required before a second person uses the app (§12.7, §8.8 trigger).
- **The setup code stays in Secret Manager after setup.** It cannot do anything once an owner exists. Destroy it after setup if you prefer.
