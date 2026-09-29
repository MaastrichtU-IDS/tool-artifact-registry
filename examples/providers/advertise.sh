#!/bin/sh
# Advertise one run's artifacts to the registry, as the deployment that ran it.
#
#   ./advertise.sh produced payload.json      # or: consumed
#   ./advertise.sh announce  record.json      # PUT /api/v1/instances/self
#
# Needs curl and nothing else, so it runs in a bare CI image.
#
# Configuration, from the environment:
#   TAR_URL             the registry, e.g. https://registry.example.org
#   TAR_TOKEN_URL       your identity provider's token endpoint
#   TAR_CLIENT_ID       this deployment's client id
#   TAR_CLIENT_SECRET   its secret
#   TAR_TOKEN           instead of the three above: a registry API token
set -eu

what=${1:?"usage: $0 produced|consumed|announce FILE"}
file=${2:?"usage: $0 produced|consumed|announce FILE"}

if [ -z "${TAR_TOKEN:-}" ]; then
  # Client credentials: a short-lived token from your own identity provider. The registry
  # stores no secret for this deployment; rotating it is your identity provider's job.
  TAR_TOKEN=$(curl -fsS -u "$TAR_CLIENT_ID:$TAR_CLIENT_SECRET" -d grant_type=client_credentials \
    "$TAR_TOKEN_URL" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
  [ -n "$TAR_TOKEN" ] || { echo "no access token from $TAR_TOKEN_URL" >&2; exit 1; }
fi

case "$what" in
  produced | consumed) method=POST; path=/api/v1/advertise/$what ;;
  announce) method=PUT; path=/api/v1/instances/self ;;
  *) echo "unknown action: $what" >&2; exit 2 ;;
esac

# --fail-with-body prints the registry's problem+json on an error, which says what to fix.
# 429 and 5xx are retried, honouring the Retry-After the registry sends; a 4xx is not.
curl -sS --fail-with-body --retry 3 \
  -X "$method" "${TAR_URL%/}$path" \
  -H "Authorization: Bearer $TAR_TOKEN" -H 'Content-Type: application/json' \
  --data-binary "@$file"
echo
