# Cloud Run: the api service, the worker job, and the migration job.
#
# All three use the same image. CI changes the image on each deploy, so
# Terraform ignores it after the first apply.

locals {
  plain_env = {
    RUST_LOG        = "info"
    PLAID_ENV       = var.plaid_env
    PLAID_CLIENT_ID = var.plaid_client_id
    # The api: passkeys and redirects. The worker: the VAPID `sub` claim for
    # Web Push (design doc D13).
    SAVESPACE_PUBLIC_ORIGIN = local.public_origin
  }

  runtime_secret_env = {
    DATABASE_URL        = "database-url"
    PLAID_SECRET        = "plaid-secret"
    SAVESPACE_TOKEN_KEK = "token-kek"
  }

  # The deterministic Cloud Run URL. Passkeys are bound to this host name
  # (WebAuthn relying party ID), and the api redirects other host names here.
  public_origin = "https://savespace-api-${data.google_project.this.number}.${var.region}.run.app"

  # Only the api reads these.
  api_plain_env = {
    # The api starts this job right away for urgent work (design doc D15).
    SAVESPACE_WORKER_JOB = "projects/${var.project_id}/locations/${var.region}/jobs/savespace-worker"
  }
  api_secret_env = {
    SAVESPACE_OWNER_SETUP_CODE = "owner-setup-code"
  }
}

resource "google_cloud_run_v2_service" "api" {
  count               = local.deploy ? 1 : 0
  name                = "savespace-api"
  location            = var.region
  ingress             = "INGRESS_TRAFFIC_ALL"
  deletion_protection = true

  template {
    service_account                  = google_service_account.api.email
    timeout                          = "30s"
    max_instance_request_concurrency = 80

    # Scale to zero. One instance at most, so the cost has a hard limit.
    scaling {
      min_instance_count = 0
      max_instance_count = 1
    }

    containers {
      image = var.image

      ports {
        container_port = 8080
      }

      resources {
        limits = {
          cpu    = "1"
          memory = "512Mi"
        }
        # Request-based billing: CPU only while a request runs.
        cpu_idle          = true
        startup_cpu_boost = false
      }

      dynamic "env" {
        for_each = local.plain_env
        content {
          name  = env.key
          value = env.value
        }
      }

      dynamic "env" {
        for_each = local.api_plain_env
        content {
          name  = env.key
          value = env.value
        }
      }

      dynamic "env" {
        for_each = merge(local.runtime_secret_env, local.api_secret_env)
        content {
          name = env.key
          value_source {
            secret_key_ref {
              secret  = google_secret_manager_secret.this[env.value].secret_id
              version = "latest"
            }
          }
        }
      }

      startup_probe {
        http_get {
          path = "/health"
        }
        period_seconds    = 2
        timeout_seconds   = 1
        failure_threshold = 15
      }
    }
  }

  lifecycle {
    ignore_changes = [
      template[0].containers[0].image,
      client,
      client_version,
    ]
  }

  depends_on = [google_secret_manager_secret_iam_member.access]
}

# The web app is public. The app does its own authentication (passkeys).
resource "google_cloud_run_v2_service_iam_member" "public" {
  count    = local.deploy ? 1 : 0
  name     = google_cloud_run_v2_service.api[0].name
  location = var.region
  role     = "roles/run.invoker"
  member   = "allUsers"
}

resource "google_cloud_run_v2_job" "worker" {
  count               = local.deploy ? 1 : 0
  name                = "savespace-worker"
  location            = var.region
  deletion_protection = false

  template {
    task_count  = 1
    parallelism = 1

    template {
      service_account = google_service_account.worker.email
      max_retries     = 0
      timeout         = "600s"

      containers {
        image   = var.image
        command = ["/app/worker"]
        args    = ["drain"]

        resources {
          limits = {
            cpu    = "1"
            memory = "512Mi"
          }
        }

        dynamic "env" {
          for_each = local.plain_env
          content {
            name  = env.key
            value = env.value
          }
        }

        dynamic "env" {
          for_each = local.runtime_secret_env
          content {
            name = env.key
            value_source {
              secret_key_ref {
                secret  = google_secret_manager_secret.this[env.value].secret_id
                version = "latest"
              }
            }
          }
        }
      }
    }
  }

  lifecycle {
    ignore_changes = [
      template[0].template[0].containers[0].image,
      client,
      client_version,
    ]
  }

  depends_on = [google_secret_manager_secret_iam_member.access]
}

resource "google_cloud_run_v2_job" "migrate" {
  count               = local.deploy ? 1 : 0
  name                = "savespace-migrate"
  location            = var.region
  deletion_protection = false

  template {
    task_count  = 1
    parallelism = 1

    template {
      service_account = google_service_account.migrate.email
      max_retries     = 0
      timeout         = "300s"

      containers {
        image   = var.image
        command = ["/app/worker"]
        args    = ["migrate"]

        resources {
          limits = {
            cpu    = "1"
            memory = "512Mi"
          }
        }

        env {
          name  = "RUST_LOG"
          value = "info"
        }

        env {
          name = "DATABASE_URL"
          value_source {
            secret_key_ref {
              secret  = google_secret_manager_secret.this["database-url-migrate"].secret_id
              version = "latest"
            }
          }
        }
      }
    }
  }

  lifecycle {
    ignore_changes = [
      template[0].template[0].containers[0].image,
      client,
      client_version,
    ]
  }

  depends_on = [google_secret_manager_secret_iam_member.access]
}

# ---------------------------------------------------------------------------
# Scheduler: run the worker drain job on a schedule. Free tier: 3 jobs.

# The api runs the worker job after a new connection or a webhook (D15).
# Only this one job, not the migration job.
resource "google_cloud_run_v2_job_iam_member" "api_runs_worker" {
  count    = local.deploy ? 1 : 0
  name     = google_cloud_run_v2_job.worker[0].name
  location = var.region
  role     = "roles/run.invoker"
  member   = "serviceAccount:${google_service_account.api.email}"
}

resource "google_cloud_run_v2_job_iam_member" "scheduler_runs_worker" {
  count    = local.deploy ? 1 : 0
  name     = google_cloud_run_v2_job.worker[0].name
  location = var.region
  role     = "roles/run.invoker"
  member   = "serviceAccount:${google_service_account.scheduler.email}"
}

resource "google_cloud_scheduler_job" "worker" {
  count            = local.deploy ? 1 : 0
  name             = "savespace-worker-drain"
  region           = var.region
  description      = "Runs the savespace-worker Cloud Run job (worker drain)."
  schedule         = var.worker_schedule
  time_zone        = "Etc/UTC"
  attempt_deadline = "60s"

  retry_config {
    retry_count = 0
  }

  http_target {
    http_method = "POST"
    uri         = "https://run.googleapis.com/v2/projects/${var.project_id}/locations/${var.region}/jobs/${google_cloud_run_v2_job.worker[0].name}:run"

    oauth_token {
      service_account_email = google_service_account.scheduler.email
      scope                 = "https://www.googleapis.com/auth/cloud-platform"
    }
  }

  depends_on = [google_cloud_run_v2_job_iam_member.scheduler_runs_worker]
}
