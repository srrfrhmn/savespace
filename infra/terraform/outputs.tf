output "api_url" {
  description = "The public URL of the web app and API. Passkeys work on this host name only."
  value       = local.public_origin
}

output "image_repository" {
  description = "Push images here."
  value       = "${var.region}-docker.pkg.dev/${var.project_id}/${google_artifact_registry_repository.images.repository_id}"
}

output "backup_bucket" {
  value = google_storage_bucket.backups.name
}

# Values for GitHub repository variables (infra/scripts/github-vars.sh).
output "github_variables" {
  value = {
    GCP_PROJECT_ID    = var.project_id
    GCP_REGION        = var.region
    GCP_WIF_PROVIDER  = google_iam_workload_identity_pool_provider.github.name
    GCP_DEPLOY_SA     = google_service_account.deployer.email
    GCP_BACKUP_SA     = google_service_account.backup.email
    GCP_BACKUP_BUCKET = google_storage_bucket.backups.name
  }
}
