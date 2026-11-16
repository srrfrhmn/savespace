#!/usr/bin/env bash
# One-time setup: the Google Cloud project and the Terraform state bucket.
#
# Usage: infra/scripts/bootstrap.sh PROJECT_ID BILLING_ACCOUNT_ID
# Needs: gcloud, logged in (gcloud auth login; gcloud auth application-default login).
# Safe to run again.
set -euo pipefail

project=${1:?usage: bootstrap.sh PROJECT_ID BILLING_ACCOUNT_ID}
billing=${2:?usage: bootstrap.sh PROJECT_ID BILLING_ACCOUNT_ID}
region=${REGION:-us-east1}

if ! gcloud projects describe "$project" > /dev/null 2>&1; then
  gcloud projects create "$project" --name="SaveSpace"
fi
gcloud billing projects link "$project" --billing-account="$billing"
gcloud config set project "$project"

gcloud services enable --project "$project" \
  serviceusage.googleapis.com \
  cloudresourcemanager.googleapis.com \
  cloudbilling.googleapis.com \
  billingbudgets.googleapis.com \
  storage.googleapis.com

# Terraform state. Versioned, private, in a free-tier region.
bucket="gs://${project}-tfstate"
if ! gcloud storage buckets describe "$bucket" > /dev/null 2>&1; then
  gcloud storage buckets create "$bucket" --project "$project" --location "$region" \
    --uniform-bucket-level-access --public-access-prevention
fi
gcloud storage buckets update "$bucket" --versioning

gcloud auth application-default set-quota-project "$project"
echo "Bootstrap done. Next: terraform -chdir=infra/terraform init -backend-config=\"bucket=${project}-tfstate\""
