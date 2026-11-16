#!/usr/bin/env bash
# Sets the GitHub repository variables from Terraform outputs and creates the
# production environment (deploys from main only).
#
# These are variables, not secrets: none of them gives access on its own.
# Usage: infra/scripts/github-setup.sh
# Needs: gh (logged in), terraform (initialized), node.
set -euo pipefail

cd "$(dirname "$0")/../terraform"

terraform output -json github_variables \
  | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{for(const[k,v] of Object.entries(JSON.parse(s)))console.log(`${k}=${v}`)})' \
  | while IFS='=' read -r key value; do
      gh variable set "$key" --body "$value"
    done

url=$(terraform output -raw api_url 2> /dev/null || true)
if [ -n "$url" ] && [ "$url" != "null" ]; then
  gh variable set APP_URL --body "$url"
fi

# Production environment: only main can deploy.
gh api --method PUT "repos/{owner}/{repo}/environments/production" --input - > /dev/null <<'JSON'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
JSON
gh api --method POST "repos/{owner}/{repo}/environments/production/deployment-branch-policies" \
  -f name=main -f type=branch > /dev/null 2>&1 || true

echo "GitHub variables and the production environment are set."
