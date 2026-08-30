#!/usr/bin/env bash
#
# Tool Artifact Registry — two applications, coordinating through the registry.
#
# sulo-app (a simulated sulo-schema-builder) generates an OWL ontology and advertises it.
# onto-app (a simulated OntoExplorer) has a standing subscription that matches it, is told,
# fetches it, ingests it, and advertises what the ingest derived. Neither program knows the
# other exists. Everything goes through the public HTTP API — there is no privileged back door
# here that a tool of yours could not use.
#
#   ./demo/run-two-app-demo.sh          start a private registry, run both apps, leave it up
#   ./demo/run-two-app-demo.sh --stop   stop the registry and both apps
#   ./demo/run-two-app-demo.sh --clean  stop, and delete this demo's data directory
#
# Environment:
#   TAR_URL         run against a registry that is already running instead of starting one.
#                   Requires TAR_ROOT_TOKEN. Nothing is deleted in that mode.
#   TAR_ROOT_TOKEN  bootstrap admin token (generated when this script starts its own registry)
#
# By default this starts its **own** registry on a free port with its **own** data directory
# (demo/.run/data). It never touches port 8099 or ./data.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
RUN="$HERE/.run"
APPS="$HERE/apps"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
step()  { printf '\n\033[1;34m▸ %s\033[0m\n' "$*"; }
note()  { printf '  %s\n' "$*"; }
warn()  { printf '  \033[33m! %s\033[0m\n' "$*"; }
quiet() { printf '  \033[2m%s\033[0m\n' "$*"; }

# ------------------------------------------------------------------ lifecycle

stop_all() {
  local any=0
  for f in "$RUN"/*.pid; do
    [ -e "$f" ] || continue
    local pid name
    pid="$(cat "$f" 2>/dev/null || true)"
    name="$(basename "$f" .pid)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      note "stopped $name (pid $pid)"
      any=1
    fi
    rm -f "$f"
  done
  [ "$any" = 1 ] || note "nothing was running"
}

case "${1:-}" in
  --stop)  step "Stopping"; stop_all; exit 0 ;;
  --clean) step "Stopping and cleaning"; stop_all; rm -rf "$RUN"; note "removed $RUN"; exit 0 ;;
  "")      ;;
  *)       echo "usage: $0 [--stop|--clean]" >&2; exit 2 ;;
esac

command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }
command -v curl    >/dev/null || { echo "curl is required" >&2; exit 1; }

# ------------------------------------------------------------------ HTTP helper

# POST/PATCH JSON as a given bearer token; abort loudly on a non-2xx, because a demo that
# silently half-loads is worse than one that stops.
api() {
  local method="$1" path="$2" token="$3" body="${4:-}" out code
  out="$(mktemp)"
  if [ -n "$body" ]; then
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$TAR_URL$path" \
      -H "Authorization: Bearer $token" -H 'content-type: application/json' -d "$body")
  else
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$TAR_URL$path" \
      -H "Authorization: Bearer $token")
  fi
  if [ "${code:0:1}" != "2" ]; then
    warn "$method $path -> $code"
    python3 - "$out" <<'PY' >&2
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    print("   ", d.get("title", ""), "-", d.get("detail", ""))
    if d.get("report"):
        print("    --- SHACL validation report ---")
        print("    " + d["report"].replace("\n", "\n    "))
except Exception:
    print(open(sys.argv[1]).read()[:800])
PY
    rm -f "$out"; exit 1
  fi
  cat "$out"; rm -f "$out"
}

field() { python3 -c "import json,sys; print(json.load(sys.stdin)[sys.argv[1]])" "$1"; }

# ------------------------------------------------------------------ the registry

mkdir -p "$RUN"

if [ -n "${TAR_URL:-}" ]; then
  step "Using the registry already running at $TAR_URL"
  [ -n "${TAR_ROOT_TOKEN:-}" ] || { warn "TAR_ROOT_TOKEN must be set to register into an existing registry"; exit 1; }
  curl -sf "$TAR_URL/healthz" >/dev/null || { warn "no registry answering at $TAR_URL"; exit 1; }
  note "nothing in it will be deleted; this demo only adds records"
else
  BIN="$ROOT/target/release/tar"
  [ -x "$BIN" ] || BIN="$ROOT/target/debug/tar"
  if [ ! -x "$BIN" ]; then
    step "Building the registry (cargo build --release)"
    (cd "$ROOT" && cargo build --release)
    BIN="$ROOT/target/release/tar"
  fi

  PORT="${TAR_DEMO_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
  export TAR_URL="http://127.0.0.1:$PORT"
  export TAR_BASE_IRI="$TAR_URL"
  export TAR_LISTEN="127.0.0.1:$PORT"
  export TAR_DATA_DIR="$RUN/data"
  export TAR_ROOT_TOKEN="${TAR_ROOT_TOKEN:-$(python3 -c 'import secrets;print("demo-"+secrets.token_hex(16))')}"
  [ -d "$ROOT/frontend/dist" ] && export TAR_STATIC_DIR="$ROOT/frontend/dist"

  step "Starting a private registry on port $PORT"
  note "data dir  $TAR_DATA_DIR   (its own — port 8099 and ./data are untouched)"
  if [ -f "$RUN/registry.pid" ] && kill -0 "$(cat "$RUN/registry.pid")" 2>/dev/null; then
    warn "a demo registry is already running (pid $(cat "$RUN/registry.pid")); run --stop first"
    exit 1
  fi
  ( cd "$ROOT" && exec "$BIN" serve ) >"$RUN/registry.log" 2>&1 &
  echo $! > "$RUN/registry.pid"
  printf '  waiting for it'
  for _ in $(seq 1 60); do
    if curl -sf "$TAR_URL/healthz" >/dev/null 2>&1; then echo " ready"; break; fi
    printf '.'; sleep 0.5
  done
  curl -sf "$TAR_URL/healthz" >/dev/null || { echo; warn "it never became healthy — see $RUN/registry.log"; exit 1; }
fi

ROOT_TOKEN="$TAR_ROOT_TOKEN"

# ------------------------------------------------------------------ vocabulary

# EDAM has no term for "the subclass axioms a reasoner entailed" or "an index that lives in a
# database". D11 says an ArtifactType is any IRI, so the registry mints concepts for what EDAM
# does not name. Re-registering a slug updates it rather than duplicating it, so this is safe
# to run twice.
step "Registering the artifact types these two tools trade in"
mktype() { # slug label definition media-type
  api POST /api/v1/types "$ROOT_TOKEN" "$(python3 -c '
import json,sys
print(json.dumps({"slug":sys.argv[1],"label":sys.argv[2],"definition":sys.argv[3],"default_media_type":sys.argv[4]}))' \
    "$1" "$2" "$3" "$4")" | field iri
}
T_OWL=$(mktype owl-ontology "OWL ontology" "An OWL 2 ontology document." "text/turtle")
T_SHAPES=$(mktype shacl-shapes-graph "SHACL shapes graph" "An RDF graph of SHACL shapes used to validate other graphs." "text/turtle")
T_INFERRED=$(mktype inferred-subclass-axioms "Inferred subclass axioms" "Subclass relations entailed over an ontology and materialised as asserted triples." "text/turtle")
T_INDEX=$(mktype ontology-term-index "Ontology term index" "Ontology terms indexed for lookup inside a deployment's own database. Has no file form." "application/octet-stream")
T_METRICS=$(mktype ontology-ingest-metrics "Ontology ingest metrics" "Counts and quality signals computed while ingesting an ontology." "application/json")
note "5 types under $TAR_URL/type/…"

# ------------------------------------------------------------------ the two tools

step "Registering the two tools and their deployments"

SW_SULO=$(api POST /api/v1/software "$ROOT_TOKEN" "$(cat <<JSON
{
  "name": "sulo-schema-builder (simulated)",
  "tagline": "Design a schema, emit OWL and SHACL",
  "description": "A SIMULATION written for this demo. It is not the real sulo-schema-builder deployment and does not talk to it; it stands in for one so that the advertisement path can be exercised end to end. What it does faithfully is the part being demonstrated: hold a schema model, render it to a real OWL 2 ontology and a SHACL shapes graph, serve both, and advertise them with its own credential.",
  "kinds": ["service"],
  "maturity": "experimental",
  "keywords": ["demo", "simulation", "owl", "shacl", "ontology"],
  "publisher": {"name": "Maastricht University — Institute of Data Science", "kind": "organization", "identifier": "https://ror.org/02jz4aj89"},
  "capability": { "produces": ["$T_OWL", "$T_SHAPES"] }
}
JSON
)" | field id)
note "sulo-schema-builder (simulated)   $SW_SULO"

SW_ONTO=$(api POST /api/v1/software "$ROOT_TOKEN" "$(cat <<JSON
{
  "name": "ontoexplorer (simulated)",
  "tagline": "Ingest an ontology, derive what can be derived from it",
  "description": "A SIMULATION written for this demo. It is not the real OntoExplorer deployment and does not talk to it. It stands in for one to show the other half of the loop: a standing subscription, a pull-based delivery queue, a real fetch with checksum verification, and derived artifacts advertised back with their lineage.",
  "kinds": ["service"],
  "maturity": "experimental",
  "keywords": ["demo", "simulation", "ontology", "ingest", "subscription"],
  "publisher": {"name": "Maastricht University — Institute of Data Science", "kind": "organization", "identifier": "https://ror.org/02jz4aj89"},
  "capability": { "consumes": ["$T_OWL"], "produces": ["$T_INFERRED", "$T_INDEX", "$T_METRICS"] }
}
JSON
)" | field id)
note "ontoexplorer (simulated)          $SW_ONTO"

api POST "/api/v1/software/$SW_SULO/releases" "$ROOT_TOKEN" \
  '{"version":"0.0.1-demo","install_command":"python3 demo/apps/sulo_app.py --help"}' >/dev/null
api POST "/api/v1/software/$SW_ONTO/releases" "$ROOT_TOKEN" \
  '{"version":"0.0.1-demo","install_command":"python3 demo/apps/onto_app.py --help"}' >/dev/null

# No endpoint_url here: each app binds a free port at start-up, so the address is not known
# until the process is running. Each app records its own once it knows it — a deployment may
# maintain its own record. Note that this is an *outbound* address for fetching files, not an
# inbound one the registry could deliver to, which is why the subscriber still pulls.
mkinstance() { # label software-id
  api POST /api/v1/instances "$ROOT_TOKEN" "$(python3 -c '
import json,sys
print(json.dumps({"label":sys.argv[1],"software":sys.argv[2],
 "description":"A simulated deployment, started by demo/run-two-app-demo.sh on this machine.",
 "operator":{"name":"Maastricht University — Institute of Data Science","kind":"organization","identifier":"https://ror.org/02jz4aj89"},
 "availability":"restricted","jurisdiction":"NL",
 "allowed_scopes":["advertise:produce","advertise:consume"]}))' "$1" "$2")" | field id
}
I_SULO=$(mkinstance "sulo-app (simulated deployment)" "$SW_SULO")
I_ONTO=$(mkinstance "onto-app (simulated deployment)" "$SW_ONTO")

token_for() { api POST "/api/v1/instances/$1/tokens" "$ROOT_TOKEN" \
  '{"scopes":["advertise:produce","advertise:consume"],"label":"two-app demo"}' | field token; }
TOK_SULO=$(token_for "$I_SULO")
TOK_ONTO=$(token_for "$I_ONTO")
note "each deployment has its own token, scoped to advertise:produce and advertise:consume"
note "neither app ever sees the root token, or the other app's token"

# Each app's own configuration, the way an operator would provision one: where the registry
# is, which Instance this deployment is, its credential, and which type IRIs it works with.
write_config() { # path instance-id token
  python3 -c '
import json,sys
json.dump({"registry":sys.argv[2],"instance_id":sys.argv[3],
           "instance":sys.argv[2]+"/instance/"+sys.argv[3],"token":sys.argv[4],
           "types":{"owl":sys.argv[5],"shapes":sys.argv[6],"inferred":sys.argv[7],
                    "term_index":sys.argv[8],"metrics":sys.argv[9]}},
          open(sys.argv[1],"w"), indent=2)' \
    "$1" "$TAR_URL" "$2" "$3" "$T_OWL" "$T_SHAPES" "$T_INFERRED" "$T_INDEX" "$T_METRICS"
}
write_config "$RUN/sulo.json" "$I_SULO" "$TOK_SULO"
write_config "$RUN/onto.json" "$I_ONTO" "$TOK_ONTO"

# ------------------------------------------------------------------ the story

rm -f "$RUN/onto.ready" "$RUN/onto.done" "$RUN/sulo.ready" "$RUN/onto.log" "$RUN/sulo.log"

# The two apps log to files and this script relays them, rather than the apps writing to the
# terminal directly. Two reasons: the story stays in one stream in the order it happened, and
# the apps keep running after this script exits without holding its stdout open — so
# `run-two-app-demo.sh | tee somewhere` finishes instead of hanging on a live child.
declare -A OFFSET
relay() {
  local f off size
  for f in "$@"; do
    [ -f "$f" ] || continue
    off="${OFFSET[$f]:-0}"
    size="$(stat -c%s "$f")"
    if [ "$size" -gt "$off" ]; then
      tail -c "+$((off + 1))" "$f" | head -c "$((size - off))"
      OFFSET[$f]="$size"
    fi
  done
}

# Wait for a marker file, relaying whatever the apps say while we wait. Returns 1 on timeout.
await() { # marker, seconds, logs...
  local marker="$1" limit="$2" waited=0
  shift 2
  while [ ! -f "$marker" ]; do
    relay "$@"
    if [ "$waited" -ge "$((limit * 4))" ]; then relay "$@"; return 1; fi
    sleep 0.25
    waited=$((waited + 1))
  done
  relay "$@"
}

step "Starting onto-app — it subscribes first, then waits"
note "a subscription only ever sees what arrives after it exists, so the subscriber goes first"
python3 -u "$APPS/onto_app.py" \
  --config "$RUN/onto.json" --out "$RUN/onto-data" \
  --ready-file "$RUN/onto.ready" --done-file "$RUN/onto.done" --pid-file "$RUN/onto.pid" \
  --want 1 --timeout 120 --hold >"$RUN/onto.log" 2>&1 &

await "$RUN/onto.ready" 30 "$RUN/onto.log" \
  || { warn "onto-app never registered its subscription — see $RUN/onto.log"; stop_all; exit 1; }

step "Starting sulo-app — it builds an ontology and advertises it"
note "it has never heard of onto-app; it only tells the registry what it made"
python3 -u "$APPS/sulo_app.py" \
  --config "$RUN/sulo.json" --out "$RUN/sulo-data" \
  --ready-file "$RUN/sulo.ready" --pid-file "$RUN/sulo.pid" --hold >"$RUN/sulo.log" 2>&1 &

await "$RUN/sulo.ready" 30 "$RUN/sulo.log" "$RUN/onto.log" \
  || { warn "sulo-app never advertised — see $RUN/sulo.log"; stop_all; exit 1; }

step "The registry matched that advertisement against every standing subscription"
note "two artifacts went out; the filter asks for OWL ontologies, so exactly one delivery was queued"
note "the match happened on the advertise path, in memory, with no socket opened (subscriptions §3)"
echo

await "$RUN/onto.done" 120 "$RUN/onto.log" "$RUN/sulo.log" \
  || { warn "onto-app did not finish in time — see $RUN/onto.log"; stop_all; exit 1; }

# ------------------------------------------------------------------ what it looks like now

step "What is in the registry now"
ART_ONT=$(python3 -c "
import json,sys
print(json.load(open(sys.argv[1]))['artifacts'][0])" "$RUN/sulo.ready")
SUB=$(python3 -c "
import json,sys
print(json.load(open(sys.argv[1]))['subscription'])" "$RUN/onto.done")

python3 - "$TAR_URL" "$ART_ONT" "$SUB" "$I_ONTO" <<'PY'
import json, sys, urllib.request

base, ontology, sub, onto_instance = sys.argv[1:5]
def get(p):
    with urllib.request.urlopen(base + p) as r:
        return json.load(r)

c = get("/api/v1/registry")["counts"]
print(f"  {c['software']} software · {c['instances']} deployments · {c['runs']} runs "
      f"· {c['artifacts']} artifacts")

lin = get("/api/v1/artifacts/%s/lineage?depth=3&direction=down" % ontology.rsplit("/", 1)[-1])
derived = [n for n in lin["nodes"] if n["depth"] > 0 and n["entity_type"] == "artifact"]
print(f"  the ontology has {len(derived)} artifacts derived from it, all advertised by the "
      f"other deployment:")
for n in sorted(derived, key=lambda n: n.get("title") or ""):
    print(f"    · {n.get('title')}")

meta = get("/api/v1/artifacts?availability=metadata-only")
print(f"  {meta['total']} artifact(s) are metadata-only — described, findable, and provably "
      f"not retrievable")

print()
print("  Open:")
print(f"    {base}/artifacts/{ontology.rsplit('/', 1)[-1]}")
print( "        the ontology sulo-app made — real checksum, real size, real authorship")
print(f"    {base}/api/v1/artifacts/{ontology.rsplit('/', 1)[-1]}/lineage?depth=3&direction=down")
print( "        everything the other tool derived from it, as JSON")
print(f"    {base}/runs")
print( "        two runs: one build, one ingest that both used and generated")
print(f"    {base}/artifacts?availability=metadata-only")
print( "        the term index: no downloadURL, and no rel=\"item\" in the Signposting headers")
print(f"    {base}/instances/{onto_instance}")
print( "        the subscriber, and its subscription")
print()
print("  The subscription itself — only its owning deployment, a curator or an admin may read it:")
print("    TOK=$(python3 -c 'import json;print(json.load(open(\"demo/.run/onto.json\"))[\"token\"])')")
print(f"    curl -H \"Authorization: Bearer $TOK\" {base}/api/v1/subscriptions/{sub}")
print(f"    curl -H \"Authorization: Bearer $TOK\" '{base}/api/v1/subscriptions/{sub}/deliveries?cursor=0'")
PY

step "Still running"
note "registry  pid $(cat "$RUN/registry.pid" 2>/dev/null || echo external)   $TAR_URL"
note "sulo-app  pid $(cat "$RUN/sulo.pid")   serving the ontology it advertised"
note "onto-app  pid $(cat "$RUN/onto.pid")   serving the files it derived"
note ""
note "Everything the registry points at resolves while these are up."
note "Stop them with:  ./demo/run-two-app-demo.sh --stop"
