#!/usr/bin/env bash
#
# Tool Artifact Registry — two applications, two ways in.
#
# Both applications are already in the catalogue. Neither has a deployment record when this
# starts, and they get one by opposite routes:
#
#   graph-publisher   a curator creates its deployment record by hand and mints it an API
#                     token. The application is handed that token and knows nothing else.
#   shacl-manager     no registry token exists for it at any point. A curator lists its OIDC
#                     client id on the software record; the deployment fetches a token from
#                     Keycloak, creates and thereafter maintains its own record, and advertises
#                     with the same token.
#
# Then they trade: the first publishes a graph and the shapes it claims to satisfy, the second
# validates it and publishes what that found, the first acts on the report and publishes a
# revision. The lineage that leaves behind runs through both deployments, which is the thing
# neither application could have recorded on its own.
#
# Which route you want depends on what you have. Tokens need no identity provider and are the
# only option when there isn't one; they are also a secret the registry has to store, hand over
# and rotate, once per deployment. Client credentials cost you a Keycloak, and in exchange the
# registry never holds a secret for the deployment at all.
#
#   ./demo/run-two-credentials-demo.sh          run it, and leave the registry up to browse
#   ./demo/run-two-credentials-demo.sh --stop   stop the registry this demo started
#   ./demo/run-two-credentials-demo.sh --clean  stop it, and delete this demo's data directory
#
# Requires the shipped Keycloak realm — there is no simulating the half of this demo that is
# about a real identity provider:
#   docker compose -f deploy/keycloak/compose.yaml up -d
#
# Environment:
#   KC_URL           Keycloak base URL   (default http://127.0.0.1:8090)
#   KC_REALM         realm               (default tar)
#   KC_CLIENT_ID     the confidential client with a service account, from realm-tar.json
#   KC_CLIENT_SECRET its secret
#   TAR_DEMO_PORT    pin the private registry's port instead of taking a free one
#
# This starts its **own** registry on a free port with its **own** data directory
# (demo/.run/two-credentials). It never writes to ./data or to port 8099, and it never
# reconfigures the Keycloak it reads tokens from.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT="$(cd "$HERE/.." && pwd)"
RUN="$HERE/.run/two-credentials"
APPS="$HERE/apps"

KC_URL="${KC_URL:-http://127.0.0.1:8090}"
REALM="${KC_REALM:-tar}"
CLIENT_ID="${KC_CLIENT_ID:-shacl-manager-demo}"
CLIENT_SECRET="${KC_CLIENT_SECRET:-shacl-manager-demo-secret}"
ISSUER="$KC_URL/realms/$REALM"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;34m▸ %s\033[0m\n' "$*"; }
note() { printf '  %s\n' "$*"; }
warn() { printf '  \033[33m! %s\033[0m\n' "$*"; }
die()  { printf '\033[31m%s\033[0m\n' "$*" >&2; exit 1; }

# ------------------------------------------------------------------ lifecycle

stop_registry() {
  if [ -f "$RUN/registry.pid" ]; then
    local pid; pid="$(cat "$RUN/registry.pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      note "stopped the demo registry (pid $pid)"
    fi
    rm -f "$RUN/registry.pid"
  fi
}

case "${1:-}" in
  --stop)  step "Stopping"; stop_registry; exit 0 ;;
  --clean) step "Stopping and cleaning"; stop_registry; rm -rf "$RUN"; note "removed $RUN"; exit 0 ;;
  "")      ;;
  *)       die "usage: $0 [--stop|--clean]" ;;
esac

# ------------------------------------------------------------------ preflight

for tool in curl jq python3; do
  command -v "$tool" >/dev/null || die "$tool is required"
done

# Checked before anything is started or written. Half of this demo is a deployment
# authenticating against a real identity provider; without one there is nothing to simulate
# that would still be worth watching, so it says so here rather than failing at step 8.
curl -sf "$ISSUER/.well-known/openid-configuration" >/dev/null 2>&1 || die \
"No Keycloak realm at $ISSUER.

  This demo needs a real one — one of its two applications authenticates with client
  credentials and never holds a registry token at all. Start the shipped realm with:

      docker compose -f deploy/keycloak/compose.yaml up -d

  and run this again. Set KC_URL / KC_REALM to point at a different one."

BIN="$PROJECT/target/release/tar"
[ -x "$BIN" ] || BIN="$PROJECT/target/debug/tar"
if [ ! -x "$BIN" ]; then
  step "Building the registry (cargo build --release)"
  ( cd "$PROJECT" && cargo build --release )
  BIN="$PROJECT/target/release/tar"
fi

bold "Two applications, two ways into the registry"
note "issuer   $ISSUER"

# ------------------------------------------------------------------ the operator's setup

step "1. Which audience does this issuer mint tokens for?"
KC_TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" -d grant_type=client_credentials \
  "$ISSUER/protocol/openid-connect/token" | jq -r '.access_token // empty')
[ -n "$KC_TOKEN" ] || die "Keycloak refused the client_credentials grant for $CLIENT_ID"

# Base64url, and JWT payloads carry no padding.
claims() {
  local p; p=$(cut -d. -f2 <<<"$1")
  printf '%s%s' "$p" "$(printf '=%.0s' $(seq $(( (4 - ${#p} % 4) % 4 ))))" \
    | tr '_-' '/+' | base64 -d 2>/dev/null
}
AUDIENCE=$(claims "$KC_TOKEN" | jq -r '.aud | if type=="array" then .[0] else . end // empty')
[ -n "$AUDIENCE" ] || die "the token carries no aud claim; the registry requires one"
note "aud: $(claims "$KC_TOKEN" | jq -r '.aud | if type=="array" then join(", ") else . end')"
note "A token says which service it may be presented to, and the registry refuses one addressed"
note "elsewhere. In production that name is the registry's own base IRI. The shipped development"
note "realm has a fixed audience mapper and this registry is about to come up on a free port, so"
note "the demo asks the issuer what it mints and tells the registry to answer to that name. Do"
note "not copy that part: configure the mapper, do not widen the check."

step "2. A private registry, told which issuer to trust"
mkdir -p "$RUN"
stop_registry
# Fresh every time, deliberately. This demo is a story with an order to it — a registry that
# already held half of it would read as though steps had been skipped. Its own directory, so
# there is nothing here anyone else could be using.
rm -rf "$RUN/data" "$RUN/apps"
PORT="${TAR_DEMO_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
export TAR_URL="http://127.0.0.1:$PORT"
export TAR_BASE_IRI="$TAR_URL"
export TAR_LISTEN="127.0.0.1:$PORT"
export TAR_DATA_DIR="$RUN/data"
export TAR_ROOT_TOKEN="demo-$(python3 -c 'import secrets;print(secrets.token_hex(16))')"
export TAR_OIDC_ISSUER="$ISSUER"
export TAR_OIDC_AUDIENCE="$AUDIENCE"
[ -d "$PROJECT/frontend/dist" ] && export TAR_STATIC_DIR="$PROJECT/frontend/dist"
# Left off on purpose. It would let *any* credential this registry accepts create deployment
# records, which is a different and much weaker statement than the one being demonstrated: that
# one named client may register deployments of one named application.
unset TAR_OIDC_AUTO_REGISTER_INSTANCES

( cd "$PROJECT" && exec "$BIN" serve ) >"$RUN/registry.log" 2>&1 &
echo $! > "$RUN/registry.pid"
printf '  starting'
for _ in $(seq 1 60); do
  curl -sf "$TAR_URL/healthz" >/dev/null 2>&1 && { printf ' ready\n'; break; }
  printf '.'; sleep 0.5
done
curl -sf "$TAR_URL/healthz" >/dev/null || { printf '\n'; die "it never became healthy — see $RUN/registry.log"; }
note "registry  $TAR_URL"
note "data dir  $TAR_DATA_DIR   (its own; ./data and port 8099 are untouched)"
note "This run starts from an empty registry: the directory above was deleted a moment ago and"
note "any registry a previous run left up was stopped. Run this as often as you like — it is the"
note "same story every time, and it is the only data directory this demo will ever write to."

api() { # method path token [body]
  local method="$1" path="$2" token="$3" body="${4:-}" out code
  out="$(mktemp)"
  if [ -n "$body" ]; then
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$TAR_URL$path" \
      -H "authorization: Bearer $token" -H 'content-type: application/json' -d "$body")
  else
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$TAR_URL$path" \
      -H "authorization: Bearer $token")
  fi
  if [ "${code:0:1}" != "2" ]; then
    warn "$method $path -> $code"
    jq -r '"    \(.title // "error"): \(.detail // .)", (.report // empty)' <"$out" >&2 || cat "$out" >&2
    rm -f "$out"; exit 1
  fi
  cat "$out"; rm -f "$out"
}
ROOT="$TAR_ROOT_TOKEN"

step "3. A curator settles the artifact types these two will trade in"
# A write may only name a type the registry holds; an invented IRI is refused with a 422. So
# look for the term first and mint one only where nothing answers — which is also what keeps
# this correct against a registry that was seeded with these already.
# The resolved IRI is the only thing on stdout; everything a reader sees goes to stderr, or
# `$(want_type …)` would capture the commentary along with the answer.
want_type() { # slug label definition media-type
  local iri
  iri=$(curl -s --get "$TAR_URL/api/v1/vocab/search" \
        --data-urlencode "branch=data" --data-urlencode "q=$2" \
        | jq -r --arg l "$2" 'first(.items[] | select(.label == $l) | .iri) // empty')
  if [ -z "$iri" ]; then
    iri=$(api POST /api/v1/types "$ROOT" "$(jq -n --arg s "$1" --arg l "$2" --arg d "$3" --arg m "$4" \
          '{slug:$s, label:$l, definition:$d, default_media_type:$m}')" | jq -r '.iri')
    printf '  minted   %-28s %s\n' "$2" "$iri" >&2
  else
    printf '  found    %-28s %s\n' "$2" "$iri" >&2
  fi
  printf '%s' "$iri"
}
T_GRAPH=$(want_type rdf-graph "RDF graph" \
  "An RDF graph in any serialisation." "text/turtle")
T_SHAPES=$(want_type shacl-shapes-graph "SHACL shapes graph" \
  "An RDF graph of SHACL shapes used to validate other graphs." "text/turtle")
T_REPORT=$(want_type shacl-validation-report "SHACL validation report" \
  "An RDF validation report produced by a SHACL processor." "text/turtle")
T_SUMMARY=$(want_type conformance-summary "Conformance summary" \
  "A human-readable summary of a validation run." "application/json")

step "4. Both applications are already in the catalogue"
SW_A=$(api POST /api/v1/software "$ROOT" "$(jq -n --arg g "$T_GRAPH" --arg s "$T_SHAPES" '{
  name: "graph-publisher (simulated)",
  tagline: "Publish a dataset graph and the shapes it claims to satisfy",
  description: "A SIMULATION written for this demo. It stands in for a service that exports RDF and advertises it, so that the API-token half of the credential story can be exercised end to end.",
  kinds: ["service"], maturity: "experimental",
  keywords: ["demo", "simulation", "rdf"],
  publisher: {name: "Maastricht University — Institute of Data Science", kind: "organization", identifier: "https://ror.org/02jz4aj89"},
  capability: {produces: [$g, $s]}
}')" | jq -r '.id')
note "graph-publisher   $SW_A"

# The one line that makes self-registration possible. It is not a credential and it is not a
# secret: it names an OIDC client that may create deployment records *of this software*, which
# is a narrower thing than "may write here" and a different thing from "is that deployment".
SW_B=$(api POST /api/v1/software "$ROOT" "$(jq -n --arg g "$T_GRAPH" --arg s "$T_SHAPES" \
  --arg r "$T_REPORT" --arg m "$T_SUMMARY" --arg c "$CLIENT_ID" '{
  name: "shacl-manager (simulated)",
  tagline: "Validate a graph against shapes and publish what it found",
  description: "A SIMULATION written for this demo. It stands in for a validation service that authenticates with its own identity provider; the registry issues it no credential of any kind.",
  kinds: ["service"], maturity: "experimental",
  keywords: ["demo", "simulation", "shacl"],
  publisher: {name: "Maastricht University — Institute of Data Science", kind: "organization", identifier: "https://ror.org/02jz4aj89"},
  registration_clients: [$c],
  capability: {consumes: [$g, $s], produces: [$r, $m]}
}')" | jq -r '.id')
note "shacl-manager     $SW_B"
note "                  registration_clients: $CLIENT_ID"

step "5. The curator hand-registers the first deployment, and mints it a token"
I_A=$(api POST /api/v1/instances "$ROOT" "$(jq -n --arg sw "$SW_A" '{
  label: "graph-publisher (demo deployment)",
  software: $sw,
  description: "A simulated deployment, started by demo/run-two-credentials-demo.sh on this machine.",
  operator: {name: "Maastricht University — Institute of Data Science", kind: "organization", identifier: "https://ror.org/02jz4aj89"},
  availability: "restricted", jurisdiction: "NL",
  allowed_scopes: ["advertise:produce", "advertise:consume"]
}')" | jq -r '.id')
TOKEN_A=$(api POST "/api/v1/instances/$I_A/tokens" "$ROOT" \
  '{"scopes":["advertise:produce","advertise:consume"],"label":"two-credentials demo"}' | jq -r '.token')
note "deployment $I_A"
note "token      ${TOKEN_A:0:12}…  shown once, stored as a hash, revocable at any time"
note "Two acts by a person, and a secret that now has to reach the application somehow. That is"
note "the cost of this route, and it is per deployment. What it buys is that no identity"
note "provider had to exist."

step "6. For the second deployment the curator does nothing further"
api GET "/api/v1/software/$SW_B" "$ROOT" | jq -r '"  registration_clients: \(.registration_clients | join(", "))"'
note "No record was created and no token was minted. There is nothing here to hand over, and"
note "nothing for the registry to rotate later — the deployment's secret belongs to Keycloak."

# What an operator provisions for each application: where the registry is, what it may call
# itself, the type IRIs its operator settled on, and one credential. Note what is not in
# either: the root token, or the other application's anything.
mkdir -p "$RUN/apps"
jq -n --arg reg "$TAR_URL" --arg tok "$TOKEN_A" --arg g "$T_GRAPH" --arg s "$T_SHAPES" \
      --arg r "$T_REPORT" --arg m "$T_SUMMARY" '{
  tag: "publisher", registry: $reg,
  credential: {kind: "registry-token", token: $tok},
  types: {graph: $g, shapes: $s, report: $r, summary: $m}
}' > "$RUN/apps/publisher.json"

jq -n --arg reg "$TAR_URL" --arg iss "$ISSUER" --arg cid "$CLIENT_ID" --arg sec "$CLIENT_SECRET" \
      --arg sw "$SW_B" --arg g "$T_GRAPH" --arg s "$T_SHAPES" --arg r "$T_REPORT" --arg m "$T_SUMMARY" '{
  tag: "manager", registry: $reg,
  credential: {kind: "oidc-client-credentials", issuer: $iss, client_id: $cid, client_secret: $sec},
  self_registration: {
    software: $sw,
    instance_key: "shacl-manager@demo-host",
    label: "shacl-manager (demo deployment)",
    description: "A simulated deployment that registered itself, started by demo/run-two-credentials-demo.sh on this machine.",
    availability: "restricted", jurisdiction: "NL"
  },
  types: {graph: $g, shapes: $s, report: $r, summary: $m}
}' > "$RUN/apps/manager.json"

# ------------------------------------------------------------------ the applications

app() { # config phase out
  python3 -u "$APPS/exchange_app.py" --config "$RUN/apps/$1.json" --phase "$2" --out "$RUN/apps/$3"
}

step "7. The first application publishes a graph and its shapes"
note "It presents the token it was given. The registry looks it up, finds the deployment it was"
note "minted for, and attributes the run to that — the payload never says which deployment it is."
app publisher export publisher-data

step "8. The second application registers itself, then validates what it found"
note "Its first call to the registry is the one that creates its record. Before that call the"
note "registry has never seen it; after it, it is a deployment like any other."
app manager validate manager-data

step "9. The first application acts on the report"
note "It looks the report up through the same public search anyone else would use. Neither"
note "application was told the other exists; they met over a type IRI."
app publisher revise publisher-data

# ------------------------------------------------------------------ what it left behind

step "10. The lineage now runs through both deployments"
GRAPH_ID=$(curl -s --get "$TAR_URL/api/v1/artifacts" --data-urlencode "conforms_to=$T_GRAPH" \
  | jq -r '[.items[]] | sort_by(.iri) | .[0].id')
python3 - "$TAR_URL" "$GRAPH_ID" <<'PY'
import json, sys, urllib.request

base, graph_id = sys.argv[1], sys.argv[2]


def get(path):
    with urllib.request.urlopen(base + path) as r:
        return json.load(r)


counts = get("/api/v1/registry")["counts"]
print(f"  {counts['software']} applications · {counts['instances']} deployments · "
      f"{counts['runs']} runs · {counts['artifacts']} artifacts")

# Who advertised each node, resolved per artifact: the run that generated it names the
# deployment, and that is the registry's own record of which credential arrived — not
# something either application claimed about itself.
#
# Listed flat rather than as a tree. `depth` counts hops through runs as well as through
# derivations, so two artifacts at different depths are not necessarily one below the other,
# and indenting them by it would draw a hierarchy the graph does not assert.
labels = {i["iri"]: i["label"] for i in get("/api/v1/instances")["items"]}
lineage = get(f"/api/v1/artifacts/{graph_id}/lineage?depth=4&direction=down")
root = lineage["nodes"][0]
print(f"\n  downstream of {root.get('title')}:")
for node in sorted((n for n in lineage["nodes"]
                    if n["entity_type"] == "artifact" and n["iri"] != root["iri"]),
                   key=lambda n: n.get("title") or ""):
    art = get("/api/v1/artifacts/" + node["iri"].rsplit("/", 1)[-1])
    run = art.get("generated_by_run") or {}
    who = labels.get(run.get("instance") or "", run.get("instance_label") or "?")
    print(f"    · {node.get('title'):<42} advertised by {who}")
PY
note ""
note "Three of the five artifacts came from one deployment and two from the other, and none of"
note "it was coordinated: each application said only what it had done, and the graph joined up."

step "11. The two credentials, side by side"
FRESH_KC=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" -d grant_type=client_credentials \
  "$ISSUER/protocol/openid-connect/token" | jq -r '.access_token')
for pair in "graph-publisher:$TOKEN_A" "shacl-manager:$FRESH_KC"; do
  printf '  %s\n' "${pair%%:*}"
  curl -s -H "authorization: Bearer ${pair#*:}" "$TAR_URL/api/v1/whoami" \
    | jq -r '"      credential:  \(.credential)\n      subject:     \(.subject)\n      issuer:      \(.issuer // "none — this secret was issued by the registry itself")\n      acting as:   \(.instance)\n      scopes:      \(.scopes | join(", "))"'
done
note ""
note "Same authority, arrived at two ways. The first line is the whole operational difference:"
note "one is a secret this registry issued, stored and can revoke; the other it has never held."
note "Rotating the first is a registry job, once per deployment. Rotating the second is a"
note "Keycloak job, and this registry has nothing to do."

step "12. What the second credential still cannot do"
REFUSED=$(curl -s -X PUT -H "authorization: Bearer $FRESH_KC" -H 'content-type: application/json' \
  -d "$(jq -n --arg sw "$SW_A" '{software: $sw, instance_key: "borrowed", label: "graph-publisher (impostor)"}')" \
  "$TAR_URL/api/v1/instances/self")
jq -r '"  \(.title // "?"): \(.detail // .)"' <<<"$REFUSED"
note "Self-registration is not a licence to register anything. The client id is listed by one"
note "application, so it may create deployments of that one — and being allowed to create a"
note "record is still not the same as being the thing the record describes."

printf '\n'
bold "Done. The registry is still up."
note "  $TAR_URL/instances          both deployments, and how each authenticates"
note "  $TAR_URL/artifacts          the five artifacts they traded"
note "  $TAR_URL/api/v1/artifacts/$GRAPH_ID/lineage?depth=4&direction=down"
note "  $TAR_URL/api/v1/capabilities?consumes=$T_GRAPH"
note ""
note "Stop it with:  ./demo/run-two-credentials-demo.sh --stop"
