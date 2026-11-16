# Secret Manager. Terraform creates the secrets but never the values, so no
# secret is in Terraform state. Add values with infra/scripts/set-secret.sh.
#
# Free tier: 6 active secret versions and 10,000 access operations per month.
# Cloud Run reads each secret once when an instance starts. set-secret.sh
# destroys old versions, so this setup uses 5 active versions.

locals {
  secrets = {
    # Runtime role (savespace_runtime). Used by the api and the worker.
    "database-url" = [google_service_account.api, google_service_account.worker]
    # Migration role (owns the schema). Used by migrations and backups only.
    "database-url-migrate" = [google_service_account.migrate, google_service_account.backup]
    "plaid-secret"         = [google_service_account.api, google_service_account.worker]
    # 32 random bytes, base64. Key-encryption key for Plaid access tokens
    # (design doc §12.3). Replaces Cloud KMS in the MVP.
    "token-kek" = [google_service_account.api, google_service_account.worker]
    # One-time owner setup code (design doc D12). Useless once the owner
    # exists: setup closes after the first passkey.
    "owner-setup-code" = [google_service_account.api]
  }

  secret_access = merge([
    for secret, accounts in local.secrets : {
      for sa in accounts : "${secret}/${sa.account_id}" => { secret = secret, email = sa.email }
    }
  ]...)
}

resource "google_secret_manager_secret" "this" {
  for_each  = local.secrets
  secret_id = each.key

  replication {
    auto {}
  }

  depends_on = [google_project_service.this]
}

resource "google_secret_manager_secret_iam_member" "access" {
  for_each  = local.secret_access
  secret_id = google_secret_manager_secret.this[each.value.secret].id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${each.value.email}"
}
