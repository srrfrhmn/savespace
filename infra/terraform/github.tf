# GitHub Actions authenticates with OIDC (Workload Identity Federation).
# There are no service account keys.
#
# Only workflows that run on main in this repository get a token. The workflow
# name selects the service account:
#   "deploy" -> savespace-deployer
#   "backup" -> savespace-backup

resource "google_iam_workload_identity_pool" "github" {
  workload_identity_pool_id = "github"
  display_name              = "GitHub Actions"

  depends_on = [google_project_service.this]
}

resource "google_iam_workload_identity_pool_provider" "github" {
  workload_identity_pool_id          = google_iam_workload_identity_pool.github.workload_identity_pool_id
  workload_identity_pool_provider_id = "savespace"
  display_name                       = "savespace repository"

  attribute_mapping = {
    "google.subject"          = "assertion.sub"
    "attribute.repository_id" = "assertion.repository_id"
    "attribute.ref"           = "assertion.ref"
    "attribute.workflow"      = "assertion.workflow"
  }

  # Repository ID, not name: a renamed or re-created repository cannot match.
  attribute_condition = "assertion.repository_id == '${var.github_repository_id}' && assertion.ref == 'refs/heads/main'"

  oidc {
    issuer_uri = "https://token.actions.githubusercontent.com"
  }
}

locals {
  wif_workflow = "principalSet://iam.googleapis.com/${google_iam_workload_identity_pool.github.name}/attribute.workflow"
}

resource "google_service_account_iam_member" "deploy_wif" {
  service_account_id = google_service_account.deployer.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "${local.wif_workflow}/deploy"
}

resource "google_service_account_iam_member" "backup_wif" {
  service_account_id = google_service_account.backup.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "${local.wif_workflow}/backup"
}

# Deployer: push images, update the service and jobs, run the migration job.
resource "google_artifact_registry_repository_iam_member" "deployer_push" {
  location   = google_artifact_registry_repository.images.location
  repository = google_artifact_registry_repository.images.name
  role       = "roles/artifactregistry.writer"
  member     = "serviceAccount:${google_service_account.deployer.email}"
}

resource "google_project_iam_member" "deployer_run" {
  project = var.project_id
  role    = "roles/run.developer"
  member  = "serviceAccount:${google_service_account.deployer.email}"
}

# Deploying a revision that runs as a service account needs actAs on it.
resource "google_service_account_iam_member" "deployer_act_as" {
  for_each = {
    api     = google_service_account.api.name
    worker  = google_service_account.worker.name
    migrate = google_service_account.migrate.name
  }
  service_account_id = each.value
  role               = "roles/iam.serviceAccountUser"
  member             = "serviceAccount:${google_service_account.deployer.email}"
}
