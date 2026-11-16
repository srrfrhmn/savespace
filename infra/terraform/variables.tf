variable "project_id" {
  description = "The Google Cloud project ID."
  type        = string
}

variable "region" {
  description = "The region for all resources. us-east1, us-central1, and us-west1 are in the free tier for Cloud Run and Cloud Storage."
  type        = string
  default     = "us-east1"

  validation {
    condition     = contains(["us-east1", "us-central1", "us-west1"], var.region)
    error_message = "Use a free-tier region: us-east1, us-central1, or us-west1."
  }
}

variable "billing_account" {
  description = "The billing account ID (XXXXXX-XXXXXX-XXXXXX). Used only for the budget alert."
  type        = string
}

variable "budget_usd" {
  description = "Monthly budget in USD. Alerts go to billing admins at 1%, 50%, and 100%. A budget does not stop spend."
  type        = number
  default     = 1
}

variable "github_repository_id" {
  description = "The numeric GitHub repository ID. Only workflows on main in this repository can deploy. Find it with: gh api repos/OWNER/REPO --jq .id"
  type        = string
}

variable "image" {
  description = "The first container image for Cloud Run. Leave null on the first apply (no Cloud Run resources). After that, CI deploys new images and Terraform ignores this value."
  type        = string
  default     = null
}

variable "plaid_env" {
  description = "Plaid environment: sandbox or production."
  type        = string
  default     = "sandbox"

  validation {
    condition     = contains(["sandbox", "production"], var.plaid_env)
    error_message = "plaid_env must be sandbox or production."
  }
}

variable "plaid_client_id" {
  description = "The Plaid client ID. Not a secret. The Plaid secret is in Secret Manager."
  type        = string
}

variable "worker_schedule" {
  description = "Cron schedule (UTC) for the worker drain job. Each run wakes the database for about 5 minutes. Hourly uses about 15 of the 100 free Neon compute-hours each month."
  type        = string
  default     = "7 * * * *"
}
