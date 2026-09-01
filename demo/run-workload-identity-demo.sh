#!/usr/bin/env bash
#
# Tool Artifact Registry — a deployment that authenticates as itself.
#
# Every other example here hands a program a registry API token. This one does not: the
# deployment holds a credential for its *own* identity provider, exchanges it for a short-lived
# token, and the registry works out which deployment that is. No registry secret is ever issued,
# stored or rotated — which is the point of workload identity, and the reason the registry does
# not want to be in the business of minting long-lived keys.
#
# It shows four things, in this order, because the last two are what make the first two mean
# anything:
#
#   1. the deployment fetches a token from Keycloak with `client_credentials`
#   2. the registry identifies it, and grants exactly the scopes the deployment record allows
#   3. it advertises a run — with no registry token anywhere in the process
#   4. a *person's* token, from the same realm, cannot do the same thing
#
#   ./demo/run-workload-identity-demo.sh
#
# Requires a registry and the shipped Keycloak realm. Start them with:
#   docker compose -f deploy/keycloak/compose.yaml up -d
#   TAR_BASE_IRI=http://127.0.0.1:8099 TAR_ROOT_TOKEN=… ./target/release/tar serve
#
# Environment:
#   TAR_URL          registry base URL          (default http://127.0.0.1:8099)
#   TAR_ROOT_TOKEN   bootstrap admin token — used *once*, to bind a deployment to a client id.
#                    That is a curator's act, not the deployment's; after it, the deployment
#                    never sees a registry credential again.
#   KC_URL           Keycloak base URL          (default http://127.0.0.1:8090)

set -euo pipefail

TAR_URL="${TAR_URL:-http://127.0.0.1:8099}"
KC_URL="${KC_URL:-http://127.0.0.1:8090}"
REALM="${KC_REALM:-tar}"
# Ships in deploy/keycloak/realm-tar.json: a confidential client with a service account, and
# the audience mapper the registry's `aud` check requires.
CLIENT_ID="${KC_CLIENT_ID:-shacl-manager-demo}"
CLIENT_SECRET="${KC_CLIENT_SECRET:-shacl-manager-demo-secret}"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1m%s\033[0m\n' "$*"; }
note() { printf '  %s\n' "$*"; }
die()  { printf '\033[31m%s\033[0m\n' "$*" >&2; exit 1; }

command -v jq >/dev/null || die "jq is required"

curl -sf "$TAR_URL/healthz" >/dev/null || die "no registry at $TAR_URL — start one, or set TAR_URL"
curl -sf "$KC_URL/realms/$REALM/.well-known/openid-configuration" >/dev/null \
  || die "no Keycloak realm at $KC_URL/realms/$REALM — docker compose -f deploy/keycloak/compose.yaml up -d"

ROOT="${TAR_ROOT_TOKEN:-}"
[ -n "$ROOT" ] || die "TAR_ROOT_TOKEN is required once, to bind a deployment to the client id"

bold "A deployment that authenticates as itself"
note "registry $TAR_URL"
note "issuer   $KC_URL/realms/$REALM"

# ---------------------------------------------------------------- the curator's one act

step "1. A curator binds a deployment to an identity provider client"
INSTANCE=$(curl -s "$TAR_URL/api/v1/instances" | jq -r '[.items[] | select(.tombstoned != true)][0].id')
[ "$INSTANCE" != "null" ] || die "the registry holds no deployment to bind; register one first"

BOUND=$(curl -s -X PATCH -H "authorization: Bearer $ROOT" -H 'content-type: application/json' \
  -d "$(jq -n --arg c "$CLIENT_ID" --arg i "$KC_URL/realms/$REALM" \
        '{oidc_client_id:$c, oidc_issuer:$i, allowed_scopes:["advertise:produce","advertise:consume"]}')" \
  "$TAR_URL/api/v1/instances/$INSTANCE")
note "$(jq -r '"\(.label)  ←  \(.oidc_client_id)"' <<<"$BOUND")"
note "scopes: $(jq -r '.allowed_scopes | join(", ")' <<<"$BOUND")"
note "This is the whole of the registry-side setup, and the only step that uses a registry"
note "credential. Nothing was minted; the deployment was told which client id speaks for it."

# ------------------------------------------------------------------- the deployment's job

step "2. The deployment fetches a token from its own identity provider"
TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" -d grant_type=client_credentials \
  "$KC_URL/realms/$REALM/protocol/openid-connect/token" | jq -r '.access_token // empty')
[ -n "$TOKEN" ] || die "Keycloak refused the client_credentials grant for $CLIENT_ID"

# Decode the payload for display. Base64url, and JWT payloads carry no padding.
payload() {
  local p; p=$(cut -d. -f2 <<<"$1")
  printf '%s%s' "$p" "$(printf '=%.0s' $(seq $(( (4 - ${#p} % 4) % 4 ))))" \
    | tr '_-' '/+' | base64 -d 2>/dev/null
}
CLAIMS=$(payload "$TOKEN")
note "azp (the client that asked): $(jq -r '.azp' <<<"$CLAIMS")"
note "aud (who may accept it):     $(jq -r '.aud | if type=="array" then join(", ") else . end' <<<"$CLAIMS")"
note "sid:                         $(jq -r '.sid // "absent — a machine token, not a browser login"' <<<"$CLAIMS")"
note "exp in:                      $(( $(jq -r '.exp' <<<"$CLAIMS") - $(date +%s) ))s"
note "Short-lived, and nothing in it is a registry secret."

step "3. The registry works out which deployment that is"
curl -s -H "authorization: Bearer $TOKEN" "$TAR_URL/api/v1/whoami" \
  | jq -r '"  credential: \(.credential)\n  acting as:  \(.instance // "nothing — this credential is bound to no deployment")\n  scopes:     \(.scopes | join(", "))"'
note "The scopes came from the deployment record, not from the token: an identity provider"
note "says who you are, and the registry decides what that lets you do."

step "4. It advertises a run — with no registry token in sight"
TYPE=$(curl -s "$TAR_URL/api/v1/vocab/search?branch=data&q=validation%20report" | jq -r '.items[0].iri // empty')
[ -n "$TYPE" ] || TYPE=$(curl -s "$TAR_URL/api/v1/vocab/search?branch=data&q=report" | jq -r '.items[0].iri')
KEY="workload-identity-demo/$(date +%s)"
OUT=$(curl -s -X POST -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d "$(jq -n --arg k "$KEY" --arg t "$TYPE" \
        '{run:{external_key:$k, status:"success"},
          artifacts:[{title:"Validation report from a workload-authenticated run",
                      conforms_to:$t,
                      keywords:["SHACL"]}]}')" \
  "$TAR_URL/api/v1/advertise/produced")
if [ "$(jq -r '.artifacts // empty' <<<"$OUT")" = "" ]; then
  die "advertise refused: $(jq -c '{status, detail}' <<<"$OUT")"
fi
note "run:      $(jq -r '.run' <<<"$OUT")"
note "artifact: $(jq -r '.artifacts[0]' <<<"$OUT")"
note "The registry recorded the deployment as the agent because the *credential* said so —"
note "the payload never names it, and could not lie about it if it tried."

# ------------------------------------------------------------------------- the negative

step "5. A person's token from the same realm cannot do this"
HUMAN=$(curl -s -X POST "$KC_URL/realms/$REALM/protocol/openid-connect/token" \
  -d client_id=tar-ui -d grant_type=password \
  -d username="${KC_USER:-curator}" -d password="${KC_PASSWORD:-curator-password}" \
  | jq -r '.access_token // empty')
if [ -z "$HUMAN" ]; then
  note "skipped — could not sign in as ${KC_USER:-curator}"
else
  REFUSED=$(curl -s -X POST -H "authorization: Bearer $HUMAN" -H 'content-type: application/json' \
    -d '{"run":{"status":"success"},"artifacts":[{"title":"Should not exist"}]}' \
    "$TAR_URL/api/v1/advertise/produced")
  note "$(jq -r '"\(.status): \(.detail)"' <<<"$REFUSED")"
  note "Same realm, same registry, a curator with more authority than the deployment — and"
  note "still refused, because advertising is something a deployment does about itself. Being"
  note "trusted is not the same as being the thing that ran."
fi

printf '\n'
bold "Done."
note "No registry API token was issued to the deployment at any point."
note "Rotating its credential is a Keycloak matter; the registry holds nothing to rotate."
