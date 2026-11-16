#!/usr/bin/env bash
# Creates the Neon project (PostgreSQL 18, free plan) and the database roles,
# then stores both connection URLs in Secret Manager. Prints no secret.
#
# Roles:
#   savespace_owner  owns the schema. Migrations and backups only.
#                    Secret: database-url-migrate
#   savespace_app    login role, member of savespace_runtime (no DDL).
#                    Used by the api and the worker. Secret: database-url
#
# Usage: infra/scripts/neon-setup.sh
# Needs: node (npx), docker, gcloud. Log in to Neon once: npx neonctl@9.5.0 auth
# Safe to run again. A second run rotates the savespace_app password.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
neon() { npx --yes neonctl@9.5.0 "$@"; }
json() { node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const v=JSON.parse(s);console.log(eval(process.argv[1])??"")})' "$1"; }

# neonctl asks for an organization when it cannot choose one. Pass it.
# If your account has more than one organization, set NEON_ORG_ID.
if [ -z "${NEON_ORG_ID:-}" ]; then
  NEON_ORG_ID=$(neon orgs list --output json \
    | json '(()=>{const o=v.organizations??v;if(o.length!==1)throw new Error("set NEON_ORG_ID");return o[0].id})()')
fi
echo "Neon organization: ${NEON_ORG_ID}"

name=savespace
project_id=$(neon projects list --org-id "$NEON_ORG_ID" --output json | json "(v.projects??v).find(p=>p.name==='${name}')?.id")
if [ -z "$project_id" ]; then
  echo "Creating Neon project ${name} (PostgreSQL 18, aws-us-east-1)"
  project_id=$(neon projects create --org-id "$NEON_ORG_ID" --name "$name" --region-id aws-us-east-1 --pg-version 18 \
    --database savespace --role savespace_owner --cu 0.25 --no-secrets --output json \
    | json 'v.project.id')
fi
echo "Neon project: ${project_id}"

# Direct (not pooled) endpoint. Full certificate check.
raw=$(neon connection-string --project-id "$project_id" --role-name savespace_owner --database-name savespace)
owner_url="${raw%%\?*}?sslmode=verify-full"
host=${owner_url#*@}
host=${host%%/*}

app_password=$(openssl rand -hex 32)

# psql in a throwaway container. The image has no CA bundle, so install one.
# Secrets go in as environment variables, never as arguments.
MSYS_NO_PATHCONV=1 docker run --rm -i \
  -e PGURL="$owner_url" -e APP_PASSWORD="$app_password" \
  -e PGSSLROOTCERT=/etc/ssl/certs/ca-certificates.crt \
  postgres:18 sh -c '
    apt-get update -qq > /dev/null && apt-get install -y -qq ca-certificates > /dev/null &&
    psql "$PGURL" -v ON_ERROR_STOP=1 -q' <<'SQL'
\getenv app_password APP_PASSWORD
-- Same statement as migration 20261115000000_runtime_role.sql. Safe to repeat.
DO $$ BEGIN CREATE ROLE savespace_runtime NOLOGIN; EXCEPTION WHEN duplicate_object THEN NULL; END $$;
SELECT format('CREATE ROLE savespace_app LOGIN IN ROLE savespace_runtime PASSWORD %L', :'app_password')
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'savespace_app') \gexec
SELECT format('ALTER ROLE savespace_app WITH LOGIN PASSWORD %L', :'app_password') \gexec
GRANT savespace_runtime TO savespace_app;
SQL

app_url="postgres://savespace_app:${app_password}@${host}/savespace?sslmode=verify-full"

printf '%s' "$owner_url" | "$here/set-secret.sh" database-url-migrate
printf '%s' "$app_url" | "$here/set-secret.sh" database-url
echo "Neon setup done."
