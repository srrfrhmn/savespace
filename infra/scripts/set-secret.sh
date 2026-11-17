#!/usr/bin/env bash
# Adds a new version of a Secret Manager secret from stdin, then destroys the
# older versions (the free tier counts active versions).
#
# Usage: printf '%s' "$VALUE" | infra/scripts/set-secret.sh SECRET_NAME
# Never pass a secret as an argument: arguments show in the process list and
# in shell history.
set -euo pipefail

name=${1:?usage: printf %s VALUE | set-secret.sh SECRET_NAME}
project=${PROJECT:-$(gcloud config get-value project 2> /dev/null)}

if [ -t 0 ]; then
  echo "set-secret.sh: pipe the value on stdin" >&2
  exit 2
fi

# tr: gcloud on Windows (Git Bash) ends lines with CRLF. A stray CR made
# the new version look different from itself, so it was nearly destroyed.
new=$(gcloud secrets versions add "$name" --project "$project" --data-file=- --format='value(name)' | tr -d '\r')
new=${new##*/}
if [ -z "$new" ]; then
  echo "set-secret.sh: could not read the new version" >&2
  exit 1
fi

for v in $(gcloud secrets versions list "$name" --project "$project" \
  --filter='state!=DESTROYED' --format='value(name)' | tr -d '\r'); do
  v=${v##*/}
  if [ "$v" != "$new" ]; then
    gcloud secrets versions destroy "$v" --secret "$name" --project "$project" --quiet > /dev/null
  fi
done
echo "Set ${name} (version ${new}); older versions destroyed."
