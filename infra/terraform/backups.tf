# Daily logical backups (design doc §8.8).
#
# The backup workflow runs pg_dump, encrypts the dump with age (public key in
# infra/backup/age-recipients.txt), and uploads it here. The backup service
# account can create objects but cannot read or delete them.
#
# Free tier: 5 GB of Standard storage in us-east1, us-central1, or us-west1.

resource "google_storage_bucket" "backups" {
  name                        = "${var.project_id}-db-backups"
  location                    = upper(var.region)
  storage_class               = "STANDARD"
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"
  force_destroy               = false

  soft_delete_policy {
    retention_duration_seconds = 604800 # 7 days
  }

  lifecycle_rule {
    condition {
      age = 35
    }
    action {
      type = "Delete"
    }
  }

  depends_on = [google_project_service.this]
}

resource "google_storage_bucket_iam_member" "backup_writer" {
  bucket = google_storage_bucket.backups.name
  role   = "roles/storage.objectCreator"
  member = "serviceAccount:${google_service_account.backup.email}"
}
