terraform {
  required_version = ">= 1.14"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 8.11"
    }
  }

  # State is in a versioned GCS bucket that infra/scripts/bootstrap.sh creates.
  # Pass the bucket at init: terraform init -backend-config="bucket=<project>-tfstate"
  backend "gcs" {
    prefix = "prod"
  }
}

provider "google" {
  project = var.project_id
  region  = var.region

  # The billing budget API needs a quota project when you use user credentials.
  user_project_override = true
  billing_project       = var.project_id

  default_labels = {
    app        = "savespace"
    env        = "prod"
    managed-by = "terraform"
  }
}
