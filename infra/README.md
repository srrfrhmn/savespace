# Infrastructure and operations

The MVP runs on Google Cloud and Neon at $0 per month (design doc §8.8). This file is the runbook.

| Part | Service | Free-tier limit that matters |
| --- | --- | --- |
| Web app and API | Cloud Run service `savespace-api` (scales to zero, max 1 instance) | 180,000 vCPU-s, 360,000 GiB-s, 2 M requests per month |
| Worker | Cloud Run job `savespace-worker` (`worker drain`), hourly from Cloud Scheduler | Same as above. 3 scheduler jobs |
| Migrations | Cloud Run job `savespace-migrate` (`worker migrate`), run by each deploy | Same as above |
| Database | Neon PostgreSQL 18, `aws-us-east-1`, 0.25 CU, scales to zero | 1 GB storage, 100 CU-hours per month. Neon never bills the free plan |
| Secrets | Secret Manager: `database-url`, `database-url-migrate`, `plaid-secret`, `token-kek` | 6 active versions, 10,000 accesses per month |
| Images | Artifact Registry `savespace` (keeps the 5 newest) | 0.5 GB |
| Backups | Cloud Storage `<project>-db-backups`, daily, age-encrypted, deleted after 35 days | 5 GB |
| CI/CD | GitHub Actions (public repository) | No limit |
| Logs | Cloud Logging (`SAVESPACE_LOG_FORMAT=gcp`) | 50 GiB per month |

Cost controls: max 1 Cloud Run instance, CPU only during requests, job timeouts, image cleanup, and a budget of 1 (in the billing account currency) with an alert at the first cent. A budget does not stop spend.

## Layout

```
infra/
├── terraform/      # All Google Cloud resources. One environment: prod.
├── scripts/        # bootstrap.sh, neon-setup.sh, set-secret.sh, github-setup.sh
└── backup/         # age public key for backups
```

Local development uses `compose.yaml`, not this folder.

## First-time setup

Do these steps once, in order. Commands use Bash and run from the repository root.

1. **Accounts.** Create a Google Cloud account with billing (a card is needed for identity; the free tier does not charge). Create a Neon account (no card).
2. **Log in.**
   ```sh
   gcloud auth login
   gcloud auth application-default login
   npx neonctl@9.5.0 auth
   ```
3. **Project and state bucket.**
   ```sh
   infra/scripts/bootstrap.sh savespace-prod-XXXX XXXXXX-XXXXXX-XXXXXX
   ```
4. **Foundation** (no Cloud Run yet). Copy `terraform.tfvars.example` to `terraform.tfvars` and fill it in.
   ```sh
   cd infra/terraform
   terraform init -backend-config="bucket=savespace-prod-XXXX-tfstate"
   terraform apply
   ```
5. **Secrets.** Neon setup creates the database and both URLs. If your Neon account has more than one organization, set `NEON_ORG_ID` first. Then add the Plaid secret and the token key.
   ```sh
   infra/scripts/neon-setup.sh
   printf '%s' "$PLAID_SECRET" | infra/scripts/set-secret.sh plaid-secret
   openssl rand -base64 32 | tr -d '\n' | infra/scripts/set-secret.sh token-kek
   openssl rand -hex 16 | tr -d '\n' | infra/scripts/set-secret.sh owner-setup-code
   ```
6. **First image.** Build and push from your machine:
   ```sh
   repo=$(terraform -chdir=infra/terraform output -raw image_repository)
   gcloud auth configure-docker "${repo%%/*}"
   docker build -t "$repo/app:$(git rev-parse HEAD)" . && docker push "$repo/app:$(git rev-parse HEAD)"
   ```
7. **Cloud Run.** Set `image` in `terraform.tfvars` to that image, then `terraform apply` again.
8. **Migrate.** `gcloud run jobs execute savespace-migrate --region us-east1 --wait`
9. **GitHub.** `infra/scripts/github-setup.sh` sets the repository variables and the `production` environment.
10. **Owner account.** Open the app at `terraform output -raw api_url`. Select **Set up SaveSpace**. Enter the setup code from `gcloud secrets versions access latest --secret owner-setup-code`, then create your passkey. Save the 10 recovery codes in a password manager.

After this, every push to `main` that passes CI deploys.

The app URL is `https://savespace-api-<project number>.us-east1.run.app`. Passkeys work on this host name only. The other `run.app` URL redirects to it.

## Deploy

`.github/workflows/deploy.yml` runs after `ci` passes on `main`:

1. Build the image `app:<git sha>` and push it.
2. Run `savespace-migrate` and wait. A failed migration stops the deploy.
3. Point `savespace-worker` at the new image.
4. Deploy `savespace-api`. Cloud Run sends traffic only after the `/health` startup probe passes.
5. Smoke test `/health`, `/ready`, `/`, and `/v1/auth/setup`.

Migrations run before the new code, so they must be backward compatible: expand and contract (design doc §9.3).

Only the newest commit on `main` deploys. If CI for an older commit finishes after a newer one, its deploy is skipped, so production never moves backward.

Manual deploy: Actions → deploy → Run workflow (on `main` only). Image tags are immutable: if the commit already has an image, the workflow deploys it without a new build.

## Roll back

```sh
gcloud run revisions list --service savespace-api --region us-east1
gcloud run services update-traffic savespace-api --region us-east1 --to-revisions REVISION=100
```

The next deploy sends traffic to the new revision again. Migrations do not roll back. Fix forward with a new migration.

## Operate

| Task | Command |
| --- | --- |
| API logs | `gcloud logging read 'resource.type="cloud_run_revision"' --limit 50` |
| Worker logs | `gcloud logging read 'resource.type="cloud_run_job" AND resource.labels.job_name="savespace-worker"' --limit 50` |
| Run the worker now | `gcloud run jobs execute savespace-worker --region us-east1` |
| Check the queue end to end | Insert `('system.ping', '{}')` into `app.jobs`, run the worker, check `status = 'succeeded'` |
| Pause the worker | `gcloud scheduler jobs pause savespace-worker-drain --location us-east1` |
| Database shell | `psql "$(gcloud secrets versions access latest --secret database-url-migrate)"` (needs local `psql`) |

## Bank connections (Plaid)

- The api and the worker read `PLAID_ENV`, `PLAID_CLIENT_ID`, `PLAID_SECRET`, and `SAVESPACE_TOKEN_KEK`. Without the Plaid values, bank connections are off and the app says so.
- Plaid sends webhooks to `<app url>/v1/webhooks/plaid`. The link token sets this URL, so nothing needs setup in the Plaid dashboard.
- After a new connection or a webhook, the api starts `savespace-worker` right away (`SAVESPACE_WORKER_JOB`, D15). At most one start per 20 seconds. The hourly run is the fallback.
- **Sandbox test:** on the Accounts page, connect **First Platypus Bank** (the plain entry, not OAuth) with `user_good` / `pass_good`. To test webhooks, call Plaid `/sandbox/item/fire_webhook` with `SYNC_UPDATES_AVAILABLE` for the item.
- **Check a sync:** worker logs show `sync finished` with counts. `SELECT status, error_code, last_synced_at FROM app.connections` (owner role) shows health.
- **A connection needs the person:** status `login_required` or `pending_expiration`. The Accounts page shows **Sign in to your bank** (Plaid update mode).
- **Web Push:** the VAPID key is derived from `token-kek` (D13), and the worker uses `SAVESPACE_PUBLIC_ORIGIN` as the VAPID contact. Rotating `token-kek` also changes the VAPID key: browsers must turn notifications on again.
- **`token-kek`:** do not rotate it yet. Rotation needs a re-wrap of every household key (`app.households.dek_wrapped`). Losing it makes all stored Plaid tokens unreadable. Then remove and connect the banks again.

### Go live: Plaid production (Trial plan)

The Trial plan is free: 10 production Items in total, Transactions included. Removing an Item does not give one back, and linking the same bank twice uses two.

1. The owner applies for the Trial plan in the Plaid dashboard and gets the production secret. The client ID stays the same.
2. On the live site, remove the sandbox connections (Accounts, **Remove**). Sandbox tokens do not work against production.
3. `printf '%s' "$PLAID_PRODUCTION_SECRET" | infra/scripts/set-secret.sh plaid-secret`
4. Set `plaid_env = "production"` in `terraform.tfvars`. `terraform plan` shows one in-place change each for the api service and the worker job (`PLAID_ENV`). Apply it after review. This also starts new instances, which read the new secret.
5. Connect each bank once. The API refuses the same institution twice (`already_connected`). A broken connection is fixed with **Sign in to your bank**, which does not use an Item.
6. Check: worker logs show `sync finished`; a webhook arrives within a day (`SELECT count(*) FROM app.webhook_events`).

OAuth banks (Chase, Bank of America, Wells Fargo, and others) open in a pop-up on desktop and mobile browsers, so no redirect URI is needed. The webhook URL comes from the link token, so the dashboard needs no webhook setting.

## Sign-in recovery

- **Lost a device, have another passkey:** sign in with the other passkey. In **Security**, remove the lost passkey.
- **No passkey, have a recovery code:** select **Use a recovery code** on the sign-in page. Then add a new passkey in **Security** and make new recovery codes.
- **No passkey and no recovery code:** use the database shell (owner role). Delete the owner's rows from `app.passkeys` and `app.sessions`, then insert the SHA-256 hash of a new recovery code (12 Crockford base32 characters, no dashes) into `app.recovery_codes`. Write an `app.audit_log` row with `action = 'operator.recovery'`.

## Rotate a secret

`set-secret.sh` adds a version and destroys the older ones. New instances read the new version. To make running instances use it now, deploy again (or run `gcloud run services update savespace-api --region us-east1 --update-labels rotated=$(date +%s)`).

- **App database password:** run `infra/scripts/neon-setup.sh` again.
- **Plaid secret:** rotate it in the Plaid dashboard, then `set-secret.sh plaid-secret`.
- **`token-kek`:** do not rotate it without re-encrypting the stored tokens (Phase 1 adds a key ID to each ciphertext).

## Backups and restore

`.github/workflows/backup.yml` runs daily at 09:17 UTC. Neon also keeps its own short restore window.

The age private key is at `~/.savespace/backup-age-key.txt` on the owner's machine. Keep a copy in a password manager. Without it, no backup can be decrypted.

Restore (owner credentials; the backup service account cannot read backups):

```sh
gcloud storage ls --recursive gs://<project>-db-backups/savespace/
gcloud storage cp gs://<project>-db-backups/savespace/YYYY/MM/<file>.dump.age .
age -d -i ~/.savespace/backup-age-key.txt <file>.dump.age > restore.dump
pg_restore --no-owner --exit-on-error --dbname "<target url>" restore.dump
rm restore.dump <file>.dump.age
```

The target needs the roles the dump grants to: `savespace_owner` and `savespace_runtime`. A Neon branch has them. On a plain PostgreSQL 18, create them first (`NOLOGIN`), and also `neon_superuser` and `cloud_admin`, which Neon's default privileges name.

Check a restore:

1. `SELECT count(*), bool_and(success), max(version) FROM _sqlx_migrations` matches the release.
2. Every `app` table with `household_id` has row level security on, except `outbox` (a system queue, like `jobs`).
3. As `savespace_runtime`: no `app.household_id` sees 0 users; the right one sees its rows; another one sees 0.
4. `worker migrate` brings it to the current schema.

Test a restore once per quarter. Log:

| Date | Backup | Target | Result |
| --- | --- | --- | --- |
| 2026-11-17 | `20261116T193433Z` (113 KB) | Local PostgreSQL 18 | Restored with no errors. 7 migrations, all applied. RLS checks pass. Migrated forward to 9. Decrypted files deleted. |

## Rules

- Every infrastructure change goes through a pull request. Run `terraform plan` and read it before `apply`.
- No secret values in Terraform, tfvars, workflows, or logs. Secrets go through `set-secret.sh`.
- GitHub Actions authenticates with OIDC. Do not create service account keys.
- Before you add a paid resource, write down its cost in the pull request.
