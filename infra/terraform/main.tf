# SaveSpace MVP infrastructure on Google Cloud (design doc §8.8).
#
# Goal: production practices at $0 per month for one person.
# - Cloud Run scales to zero. Max one instance caps the cost.
# - PostgreSQL 18 is on Neon (free plan). It is not managed here.
# - No paid services: no Cloud KMS, no load balancer, no Cloud SQL.

data "google_project" "this" {}

locals {
  # Cloud Run resources need an image. The first apply creates everything else.
  deploy = var.image != null

  services = [
    "artifactregistry.googleapis.com",
    "billingbudgets.googleapis.com",
    "cloudresourcemanager.googleapis.com",
    "cloudscheduler.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "logging.googleapis.com",
    "run.googleapis.com",
    "secretmanager.googleapis.com",
    "storage.googleapis.com",
    "sts.googleapis.com",
  ]
}

resource "google_project_service" "this" {
  for_each           = toset(local.services)
  service            = each.value
  disable_on_destroy = false
}

# ---------------------------------------------------------------------------
# Container images. The free tier is 0.5 GB, so keep only recent images.

resource "google_artifact_registry_repository" "images" {
  location      = var.region
  repository_id = "savespace"
  format        = "DOCKER"
  description   = "SaveSpace container images. CI pushes one image per commit on main."

  docker_config {
    immutable_tags = true
  }

  cleanup_policy_dry_run = false

  cleanup_policies {
    id     = "keep-recent"
    action = "KEEP"
    most_recent_versions {
      keep_count = 5
    }
  }

  cleanup_policies {
    id     = "delete-old"
    action = "DELETE"
    condition {
      tag_state  = "ANY"
      older_than = "604800s" # 7 days
    }
  }

  depends_on = [google_project_service.this]
}

# ---------------------------------------------------------------------------
# Service accounts. One per workload. None has a key.

resource "google_service_account" "api" {
  account_id   = "savespace-api"
  display_name = "SaveSpace API (Cloud Run service)"
}

resource "google_service_account" "worker" {
  account_id   = "savespace-worker"
  display_name = "SaveSpace worker (Cloud Run job)"
}

resource "google_service_account" "migrate" {
  account_id   = "savespace-migrate"
  display_name = "SaveSpace migrations (Cloud Run job)"
}

resource "google_service_account" "scheduler" {
  account_id   = "savespace-scheduler"
  display_name = "Cloud Scheduler: starts the worker job"
}

resource "google_service_account" "deployer" {
  account_id   = "savespace-deployer"
  display_name = "GitHub Actions: deploy workflow"
}

resource "google_service_account" "backup" {
  account_id   = "savespace-backup"
  display_name = "GitHub Actions: backup workflow"
}
