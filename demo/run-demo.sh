#!/usr/bin/env bash
#
# Tool Artifact Registry — end-to-end demo.
#
# Registers four real IDS tools, then tells one story with them: OntoExplorer ingests the
# pizza ontology from a URL, shacl-rust validates it against shapes that sulo-schema-builder
# produced, and RDFCraft maps a CSV into the same graph. Every write goes through the public
# HTTP API — there is no privileged back door here that a tool of yours could not use.
#
#   ./demo/run-demo.sh                 start the stack, then load the story
#   ./demo/run-demo.sh --no-stack      load the story into a registry already running
#   ./demo/run-demo.sh --down          stop and remove the stack and its volumes
#
# Environment:
#   TAR_URL         registry base URL              (default http://localhost:8080)
#   TAR_ROOT_TOKEN  bootstrap admin token          (default matches compose.demo.yaml)
#   ASSETS_URL      object store base URL          (default http://localhost:9000)
#   NO_ASSETS=1     skip the object store; reference images on raw.githubusercontent instead

set -euo pipefail

TAR_URL="${TAR_URL:-http://localhost:8080}"
TAR_ROOT_TOKEN="${TAR_ROOT_TOKEN:-demo-root-token-change-me-please}"
ASSETS_URL="${ASSETS_URL:-http://localhost:9000}"
ASSETS_BUCKET="${ASSETS_BUCKET:-tar-demo-assets}"
MINIO_USER="${MINIO_USER:-minioadmin}"
MINIO_PASSWORD="${MINIO_PASSWORD:-minioadmin}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;34m▸ %s\033[0m\n' "$*"; }
note() { printf '  %s\n' "$*"; }
warn() { printf '  \033[33m! %s\033[0m\n' "$*"; }

# ---------------------------------------------------------------- HTTP helpers

# POST/PATCH JSON as a given bearer token; abort loudly on a non-2xx, because a demo that
# silently half-loads is worse than one that stops.
api() {
  local method="$1" path="$2" token="$3" body="${4:-}"
  local out code
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
    # A 422 carries a SHACL report; show it, it is the most useful error this API produces.
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
    rm -f "$out"
    exit 1
  fi
  cat "$out"
  rm -f "$out"
}

jqr() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

# --------------------------------------------------------------- stack control

stack_up() {
  step "Starting the demo stack (registry + asset store)"
  note "First run builds the registry image, which takes a few minutes."
  docker compose -f "$HERE/compose.demo.yaml" up -d --build
  printf '  waiting for the registry'
  for _ in $(seq 1 120); do
    if curl -sf "$TAR_URL/healthz" >/dev/null 2>&1; then echo " ready"; return; fi
    printf '.'; sleep 2
  done
  echo; warn "the registry never became healthy; see: docker compose -f $HERE/compose.demo.yaml logs"
  exit 1
}

stack_down() {
  step "Stopping the demo stack"
  docker compose -f "$HERE/compose.demo.yaml" down -v
  exit 0
}

# ------------------------------------------------------------------- assets

# Fetch the images and READMEs that these repositories actually ship, and put them in the
# demo's own object store. This bucket holds *web assets* only. Files that a tool ingests or
# emits are a different thing entirely — those become Artifacts further down.
publish_assets() {
  step "Publishing tool images and READMEs to the asset store"
  mkdir -p "$WORK/assets"

  fetch() { # url, filename
    if curl -sfL --max-time 30 -o "$WORK/assets/$2" "$1"; then
      note "fetched $2 ($(du -h "$WORK/assets/$2" | cut -f1))"
    else
      warn "could not fetch $2 from $1"
    fi
  }

  local raw=https://raw.githubusercontent.com
  fetch "$raw/MaastrichtU-IDS/sulo-schema-builder/main/docs/images/builder.png" sulo-schema-builder.png
  fetch "$raw/MaastrichtU-IDS/sulo-schema-builder/main/docs/images/owl-export.png" sulo-schema-builder-owl.png
  fetch "$raw/MaastrichtU-IDS/RDFCraft/main/imgs/1.png" rdfcraft.png

  # Ask GitHub where the README is rather than guessing the filename: shacl-rust spells it
  # `Readme.md`, and a hard-coded `README.md` silently 404s.
  fetch_readme() { # owner/repo, output name
    local url
    url=$(curl -sfL --max-time 20 -H "Accept: application/vnd.github+json" \
            "https://api.github.com/repos/$1/readme" 2>/dev/null \
          | python3 -c "import json,sys; print(json.load(sys.stdin).get('download_url') or '')" 2>/dev/null)
    if [ -n "$url" ]; then fetch "$url" "$2"; else warn "no public README for $1"; fi
  }
  fetch_readme ensaremirerol/shacl-rust shacl-rust-README.md
  fetch_readme MaastrichtU-IDS/sulo-schema-builder sulo-schema-builder-README.md
  fetch_readme MaastrichtU-IDS/RDFCraft rdfcraft-README.md

  # OntoExplorer is a private repository, so there is no anonymous URL for its README. Use the
  # GitHub CLI when the person running this has access, and carry on without it when not.
  if command -v gh >/dev/null 2>&1 && gh api repos/MaastrichtU-IDS/ontoexplorer >/dev/null 2>&1; then
    gh api repos/MaastrichtU-IDS/ontoexplorer/readme -H "Accept: application/vnd.github.raw" \
      > "$WORK/assets/ontoexplorer-README.md" 2>/dev/null && note "fetched ontoexplorer-README.md (via gh)"
  else
    warn "ontoexplorer is private and gh is unavailable — skipping its README"
  fi

  # `mc` runs in a container so the demo needs nothing installed locally.
  docker run --rm --network host -v "$WORK/assets:/assets:ro" --entrypoint sh minio/mc -c "
    mc alias set demo '$ASSETS_URL' '$MINIO_USER' '$MINIO_PASSWORD' >/dev/null &&
    mc mb -p demo/$ASSETS_BUCKET >/dev/null &&
    mc anonymous set download demo/$ASSETS_BUCKET >/dev/null &&
    mc cp --recursive /assets/ demo/$ASSETS_BUCKET/ >/dev/null &&
    echo ok" >/dev/null 2>&1 \
    && note "uploaded to $ASSETS_URL/$ASSETS_BUCKET (anonymous read)" \
    || { warn "asset upload failed — falling back to raw.githubusercontent URLs"; NO_ASSETS=1; }
}

# Where a given asset lives, once the choice of store is settled.
asset() { # filename, github-fallback-url
  if [ "${NO_ASSETS:-0}" = "1" ]; then echo "$2"; else echo "$ASSETS_URL/$ASSETS_BUCKET/$1"; fi
}

# ------------------------------------------------------------- the four tools

register_software() {
  step "Registering the tools"
  local raw=https://raw.githubusercontent.com

  # shacl-rust — public, MIT, and the engine this registry validates its own writes with.
  SW_SHACL=$(api POST /api/v1/software "$TAR_ROOT_TOKEN" "$(cat <<JSON
{
  "name": "shacl-rust",
  "tagline": "SHACL validator written in Rust",
  "description": "A Rust implementation of the SHACL specification: SHACL Core plus SHACL-SPARQL, built on oxigraph. This registry uses it to validate every write against shapes/tar-shapes.ttl before committing.",
  "homepage": "http://ensaremirerol.github.io/shacl-rust/",
  "code_repository": "https://github.com/ensaremirerol/shacl-rust",
  "documentation": "http://ensaremirerol.github.io/shacl-rust/",
  "license": "https://spdx.org/licenses/MIT",
  "kind": "library",
  "maturity": "active",
  "topics": ["http://data.europa.eu/8mn/euroscivoc/1f6c74df-a512-462e-99aa-8dcbaa98972a", "http://data.europa.eu/8mn/euroscivoc/981a4eb6-f63a-4360-953d-efe0ec861672"],
  "keywords": ["shacl", "validation", "rdf", "rust"],
  "publisher": {"name": "Maastricht University — Institute of Data Science", "kind": "organization", "identifier": "https://ror.org/02jz4aj89"},
  "capability": {
    "consumes": ["$T_RDF_GRAPH", "$T_SHAPES"],
    "produces": ["$T_REPORT"]
  }
}
JSON
)" | jqr "['id']")
  note "shacl-rust            $SW_SHACL"

  # sulo-schema-builder — public, ships two screenshots, and declares no licence, which the
  # registry records as a FAIR warning rather than inventing one.
  SW_SULO=$(api POST /api/v1/software "$TAR_ROOT_TOKEN" "$(cat <<JSON
{
  "name": "sulo-schema-builder",
  "tagline": "SULO-compliant ontology schema builder",
  "description": "A web application that bridges domain schema design and formal OWL ontology engineering. Define classes and properties, align them to the Simplified Upper-Level Ontology (SULO), and generate RDF/Turtle, OWL DL, SHACL shapes and a Mermaid UML diagram from one model.",
  "code_repository": "https://github.com/MaastrichtU-IDS/sulo-schema-builder",
  "documentation": "https://github.com/MaastrichtU-IDS/sulo-schema-builder#readme",
  "image": "$(asset sulo-schema-builder.png "$raw/MaastrichtU-IDS/sulo-schema-builder/main/docs/images/builder.png")",
  "screenshots": ["$(asset sulo-schema-builder-owl.png "$raw/MaastrichtU-IDS/sulo-schema-builder/main/docs/images/owl-export.png")"],
  "kind": "service",
  "maturity": "active",
  "topics": ["http://data.europa.eu/8mn/euroscivoc/123e5118-1586-4a45-b4da-34583bd74940", "http://data.europa.eu/8mn/euroscivoc/981a4eb6-f63a-4360-953d-efe0ec861672"],
  "keywords": ["sulo", "owl", "shacl", "ontology", "schema"],
  "publisher": {"name": "Maastricht University — Institute of Data Science", "kind": "organization", "identifier": "https://ror.org/02jz4aj89"},
  "capability": {
    "consumes": ["$T_SCHEMA_MODEL", "$T_SULO"],
    "produces": ["$T_RDF_GRAPH", "$T_OWL", "$T_SHAPES", "$T_MERMAID"]
  }
}
JSON
)" | jqr "['id']")
  note "sulo-schema-builder   $SW_SULO"

  # ontoexplorer — private repository. The record says so instead of carrying a URL that 404s.
  SW_ONTO=$(api POST /api/v1/software "$TAR_ROOT_TOKEN" "$(cat <<JSON
{
  "name": "ontoexplorer",
  "tagline": "FAIR ontology repository and explorer",
  "description": "Ingest, browse, query and reason over ontologies with full provenance tracking. Ingests by IRI, URL or file upload across nine serialisations; reasons with ELK over OWL-EL; serves semantic search over pgvector embeddings; exposes SPARQL 1.1 and an OLS4-compatible read API. Raw ontology files are kept in its own object store — those appear here as Artifacts, not as assets of this record.",
  "kind": "service",
  "maturity": "active",
  "topics": ["http://data.europa.eu/8mn/euroscivoc/123e5118-1586-4a45-b4da-34583bd74940", "http://data.europa.eu/8mn/euroscivoc/981a4eb6-f63a-4360-953d-efe0ec861672"],
  "keywords": ["ontology", "owl", "fair", "sparql", "reasoning", "semantic-search"],
  "publisher": {"name": "Maastricht University — Institute of Data Science", "kind": "organization", "identifier": "https://ror.org/02jz4aj89"},
  "capability": {
    "consumes": ["$T_OWL", "$T_RDF_GRAPH"],
    "produces": ["$T_INFERRED", "$T_EMBEDDING_INDEX", "$T_VOID", "$T_DCAT"]
  }
}
JSON
)" | jqr "['id']")
  note "ontoexplorer          $SW_ONTO  (private repository — no repo link recorded)"

  SW_CRAFT=$(api POST /api/v1/software "$TAR_ROOT_TOKEN" "$(cat <<JSON
{
  "name": "RDFCraft",
  "tagline": "Map CSV and JSON to RDF through a GUI",
  "description": "RDFCraft maps csv/json data to RDF with an easy to use GUI. Build the mapping visually, emit RML, and generate the graph without hand-writing mapping rules.",
  "homepage": "https://maastrichtu-ids.github.io/RDFCraft/",
  "code_repository": "https://github.com/MaastrichtU-IDS/RDFCraft",
  "documentation": "https://maastrichtu-ids.github.io/RDFCraft/",
  "image": "$(asset rdfcraft.png "$raw/MaastrichtU-IDS/RDFCraft/main/imgs/1.png")",
  "license": "https://spdx.org/licenses/MIT",
  "kind": "service",
  "maturity": "active",
  "topics": ["http://data.europa.eu/8mn/euroscivoc/1f6c74df-a512-462e-99aa-8dcbaa98972a", "http://data.europa.eu/8mn/euroscivoc/981a4eb6-f63a-4360-953d-efe0ec861672"],
  "keywords": ["rml", "mapping", "csv", "json", "rdf"],
  "publisher": {"name": "Maastricht University — Institute of Data Science", "kind": "organization", "identifier": "https://ror.org/02jz4aj89"},
  "capability": {
    "consumes": ["$T_TABULAR", "$T_RML"],
    "produces": ["$T_RML", "$T_RDF_GRAPH"]
  }
}
JSON
)" | jqr "['id']")
  note "RDFCraft              $SW_CRAFT"

  # Releases give the "Use it" block something to show and let an Instance be marked outdated.
  api POST "/api/v1/software/$SW_SHACL/releases" "$TAR_ROOT_TOKEN" \
    '{"version":"0.2.11","date_published":"2026-08-20T00:00:00Z","install_command":"cargo add shacl-rust","changelog":"https://github.com/ensaremirerol/shacl-rust/releases"}' >/dev/null
  api POST "/api/v1/software/$SW_CRAFT/releases" "$TAR_ROOT_TOKEN" \
    '{"version":"1.4.0","date_published":"2026-07-02T00:00:00Z","container_image":"ghcr.io/maastrichtu-ids/rdfcraft:1.4.0","install_command":"docker pull ghcr.io/maastrichtu-ids/rdfcraft:1.4.0"}' >/dev/null
  REL_ONTO=$(api POST "/api/v1/software/$SW_ONTO/releases" "$TAR_ROOT_TOKEN" \
    '{"version":"0.9.0","date_published":"2026-08-22T00:00:00Z","container_image":"ghcr.io/maastrichtu-ids/ontoexplorer:0.9.0","install_command":"docker compose up -d"}' | jqr "['id']")
  api POST "/api/v1/software/$SW_SULO/releases" "$TAR_ROOT_TOKEN" \
    '{"version":"0.4.0","date_published":"2026-06-11T00:00:00Z","install_command":"npm ci && npm run build"}' >/dev/null
  note "releases registered"
}

# --------------------------------------------------------------- artifact types

# EDAM has no term for a SHACL shapes graph, an RML mapping or a pgvector embedding index.
# D11 says ArtifactType is any IRI, so the registry mints concepts for what EDAM does not name
# and uses EDAM where it genuinely fits.
register_types() {
  step "Registering artifact types"
  mktype() { # slug, label, definition, media type
    api POST /api/v1/types "$TAR_ROOT_TOKEN" "$(python3 -c '
import json,sys
print(json.dumps({"slug":sys.argv[1],"label":sys.argv[2],"definition":sys.argv[3],"default_media_type":sys.argv[4]}))' \
      "$1" "$2" "$3" "$4")" | jqr "['iri']"
  }
  T_RDF_GRAPH=$(mktype rdf-graph "RDF graph" "An RDF graph in any serialisation." "text/turtle")
  T_OWL=$(mktype owl-ontology "OWL ontology" "An OWL 2 ontology document." "application/rdf+xml")
  T_SHAPES=$(mktype shacl-shapes-graph "SHACL shapes graph" "An RDF graph of SHACL shapes used to validate other graphs." "text/turtle")
  T_REPORT=$(mktype shacl-validation-report "SHACL validation report" "A sh:ValidationReport produced by a SHACL processor." "text/turtle")
  T_SCHEMA_MODEL=$(mktype schema-model "Schema model" "A structural description of a dataset schema, before it is expressed as OWL." "application/json")
  T_SULO=$(mktype sulo-ontology "SULO ontology" "The Simplified Upper-Level Ontology, or a module of it." "text/turtle")
  T_MERMAID=$(mktype mermaid-uml "Mermaid UML diagram" "A class diagram in Mermaid syntax." "text/vnd.mermaid")
  T_INFERRED=$(mktype inferred-hierarchy "Inferred class hierarchy" "Subsumptions entailed by an OWL reasoner over an ontology, as asserted triples." "text/turtle")
  T_EMBEDDING_INDEX=$(mktype embedding-index "Term embedding index" "Vector embeddings of ontology terms, for semantic search. Lives inside a database, not as a file." "application/octet-stream")
  T_VOID=$(mktype void-statistics "VoID statistics" "Structural statistics describing an RDF dataset." "application/json")
  T_DCAT=$(mktype dcat-record "DCAT metadata record" "A DCAT description of a dataset, queryable over SPARQL." "text/turtle")
  T_RML=$(mktype rml-mapping "RML mapping" "Declarative rules mapping non-RDF sources into RDF." "text/turtle")
  T_TABULAR=$(mktype tabular-data "Tabular data" "CSV or JSON records awaiting mapping to RDF." "text/csv")
  note "13 types registered under $TAR_URL/type/…"
}

# ---------------------------------------------------------------- deployments

# Each deployment is bound to an OIDC client id. In this demo no identity provider is running,
# so each also gets a registry token — the fallback that keeps a single container usable. In
# production the client id is the credential and no token is minted at all.
register_instances() {
  step "Registering deployments and their credentials"
  mkinstance() { # label, software id, endpoint, oidc client
    api POST /api/v1/instances "$TAR_ROOT_TOKEN" "$(python3 -c '
import json,sys
b={"label":sys.argv[1],"software":sys.argv[2],"oidc_client_id":sys.argv[4],
   "operator":{"name":"Maastricht University — Institute of Data Science","kind":"organization"},
   "availability":"restricted","jurisdiction":"NL",
   "allowed_scopes":["advertise:produce","advertise:consume"]}
if sys.argv[3]: b["endpoint_url"]=sys.argv[3]; b["endpoint_description"]=sys.argv[3].rstrip("/")+"/openapi.json"
print(json.dumps(b))' "$1" "$2" "$3" "$4")" | jqr "['id']"
  }
  token_for() { api POST "/api/v1/instances/$1/tokens" "$TAR_ROOT_TOKEN" \
      '{"scopes":["advertise:produce","advertise:consume"],"label":"demo"}' | jqr "['token']"; }

  I_ONTO=$(mkinstance "onto.ids.unimaas.nl" "$SW_ONTO" "https://onto.ids.unimaas.nl" "ontoexplorer-ids3")
  I_SHACL=$(mkinstance "shacl-rust CI (GitHub Actions)" "$SW_SHACL" "" "repo:ensaremirerol/shacl-rust:ref:refs/heads/main")
  I_SULO=$(mkinstance "sulo.ids.unimaas.nl" "$SW_SULO" "https://sulo.ids.unimaas.nl" "sulo-schema-builder-ids3")
  I_CRAFT=$(mkinstance "rdfcraft.ids.unimaas.nl" "$SW_CRAFT" "https://rdfcraft.ids.unimaas.nl" "rdfcraft-ids3")

  TOK_ONTO=$(token_for "$I_ONTO"); TOK_SHACL=$(token_for "$I_SHACL")
  TOK_SULO=$(token_for "$I_SULO"); TOK_CRAFT=$(token_for "$I_CRAFT")

  note "onto.ids.unimaas.nl        oidc client ontoexplorer-ids3"
  note "sulo.ids.unimaas.nl        oidc client sulo-schema-builder-ids3"
  note "rdfcraft.ids.unimaas.nl    oidc client rdfcraft-ids3"
  note "shacl-rust CI              github actions subject, no endpoint (batch job)"
}

# ================================================================================
#  The story: OntoExplorer ingests the pizza ontology from a URL
# ================================================================================
#
# This is where the question "how does an uploaded or ingested resource show up as an
# artifact?" gets answered. Three cases appear below, and they are modelled differently on
# purpose. See demo/README.md for the reasoning.

PIZZA_URL="https://raw.githubusercontent.com/owlcs/pizza-ontology/master/pizza.owl"

ingest_story() {
  step "OntoExplorer ingests the pizza ontology from a URL"

  # Compute the real checksum of the real file, so the demo's provenance is true rather than
  # decorative. This is also what proves the stored copy and the upstream file are the same.
  local sha size
  if curl -sfL --max-time 60 -o "$WORK/pizza.owl" "$PIZZA_URL"; then
    sha=$(sha256sum "$WORK/pizza.owl" | cut -d' ' -f1)
    size=$(stat -c%s "$WORK/pizza.owl")
    note "fetched pizza.owl — $size bytes, sha256 ${sha:0:16}…"
  else
    warn "could not fetch the pizza ontology; using placeholder size and checksum"
    sha="0000000000000000000000000000000000000000000000000000000000000000"; size=241414
  fi

  # ---- CASE 1: ingest by URL -------------------------------------------------
  #
  # OntoExplorer fetched bytes that already existed at a public URL and kept a copy in its own
  # object store. The bytes did not change, so this is ONE artifact with TWO distributions —
  # the upstream URL and the s3:// object — not two artifacts. The shared checksum is the
  # evidence that they are interchangeable. DCAT models exactly this: one dcat:Dataset,
  # several dcat:Distribution, each a different way to obtain the same thing.
  local ingest
  ingest=$(api POST /api/v1/advertise/produced "$TOK_ONTO" "$(cat <<JSON
{
  "run": {
    "external_key": "ontoexplorer/ingest/pizza-2026-08-30",
    "label": "ingest pizza-ontology by URL",
    "started_at": "2026-08-30T09:14:02Z",
    "ended_at": "2026-08-30T09:14:37Z",
    "status": "success"
  },
  "artifacts": [{
    "title": "Pizza ontology (pizza.owl)",
    "description": "The classic Protégé pizza tutorial ontology, ingested from its public source. The registry records where the bytes came from and where OntoExplorer keeps them; it stores neither.",
    "conforms_to": "$T_OWL",
    "license": "https://spdx.org/licenses/CC-BY-4.0",
    "keywords": ["pizza", "owl", "tutorial", "ontology"],
    "issued": "2026-08-30T09:14:37Z",
    "external_key": "ontoexplorer/ontology/pizza",
    "distributions": [
      {
        "title": "Upstream source",
        "access_url": "$PIZZA_URL",
        "download_url": "$PIZZA_URL",
        "media_type": "application/rdf+xml",
        "byte_size": $size,
        "checksum": {"algorithm": "sha256", "value": "$sha"},
        "access_protocol": "https",
        "auth_method": "none",
        "availability": "public"
      },
      {
        "title": "OntoExplorer object store",
        "access_url": "https://onto.ids.unimaas.nl/ontologies/pizza",
        "download_url": "s3://ontoexplorer-raw/ontologies/pizza/pizza.owl",
        "media_type": "application/rdf+xml",
        "byte_size": $size,
        "checksum": {"algorithm": "sha256", "value": "$sha"},
        "access_protocol": "s3",
        "auth_method": "apikey",
        "availability": "restricted",
        "access_request_url": "https://onto.ids.unimaas.nl/access"
      }
    ]
  }]
}
JSON
)")
  RUN_INGEST=$(echo "$ingest" | jqr "['run']")
  ART_PIZZA=$(echo "$ingest" | jqr "['artifacts'][0]")
  note "one artifact, two distributions (https upstream + s3 copy, same checksum)"

  # ---- CASE 2: what the ingest derived --------------------------------------
  #
  # These are genuinely new things — a reasoner ran, an index was built. They are separate
  # artifacts linked back by prov:wasDerivedFrom, so the lineage says what produced what.
  api POST /api/v1/advertise/produced "$TOK_ONTO" "$(cat <<JSON
{
  "run": { "external_key": "ontoexplorer/ingest/pizza-2026-08-30" },
  "artifacts": [
    {
      "title": "Pizza ontology — inferred class hierarchy (ELK, OWL-EL)",
      "description": "Subsumptions entailed over pizza.owl by the ELK reasoner, materialised as asserted triples.",
      "conforms_to": "$T_INFERRED",
      "license": "https://spdx.org/licenses/CC-BY-4.0",
      "was_derived_from": ["$ART_PIZZA"],
      "distributions": [{
        "access_url": "https://onto.ids.unimaas.nl/ontologies/pizza/inferred",
        "download_url": "https://onto.ids.unimaas.nl/ontologies/pizza/inferred.ttl",
        "media_type": "text/turtle", "access_protocol": "https",
        "auth_method": "none", "availability": "public"
      }]
    },
    {
      "title": "Pizza ontology — term embedding index",
      "description": "nomic-embed-text-v1.5 embeddings over labels, definitions and synonyms, for semantic search.",
      "conforms_to": "$T_EMBEDDING_INDEX",
      "was_derived_from": ["$ART_PIZZA"],
      "distributions": [{
        "title": "pgvector table",
        "availability": "metadata-only",
        "access_request_url": "https://onto.ids.unimaas.nl/access",
        "media_type": "application/octet-stream"
      }]
    },
    {
      "title": "Pizza ontology — VoID statistics",
      "conforms_to": "$T_VOID",
      "license": "https://spdx.org/licenses/CC0-1.0",
      "was_derived_from": ["$ART_PIZZA"],
      "distributions": [{
        "access_url": "https://onto.ids.unimaas.nl/ontologies/pizza/void",
        "download_url": "https://onto.ids.unimaas.nl/ontologies/pizza/void.json",
        "media_type": "application/json", "access_protocol": "https",
        "auth_method": "none", "availability": "public"
      }]
    },
    {
      "title": "Pizza ontology — DCAT metadata record",
      "description": "Reachable by SPARQL rather than by downloading a file: the distribution names the service, not a byte stream.",
      "conforms_to": "$T_DCAT",
      "was_derived_from": ["$ART_PIZZA"],
      "distributions": [{
        "access_url": "https://onto.ids.unimaas.nl/sparql",
        "access_service": "https://onto.ids.unimaas.nl/sparql",
        "media_type": "text/turtle", "access_protocol": "sparql",
        "auth_method": "none", "availability": "public"
      }]
    }
  ]
}
JSON
)" >/dev/null
  note "four derived artifacts, each linked back by prov:wasDerivedFrom"
  note "  · inferred hierarchy   public https"
  note "  · embedding index      metadata-only — it lives in pgvector, there is no file"
  note "  · VoID statistics      public https"
  note "  · DCAT record          access_protocol sparql, via dcat:accessService"

  # ---- CASE 3: a file upload -------------------------------------------------
  #
  # Someone uploaded a file from their laptop. There is no upstream URL to point at, so the
  # object store IS the only distribution. The artifact records who put it there and under
  # what upload id, and that is the whole provenance available.
  api POST /api/v1/advertise/produced "$TOK_ONTO" "$(cat <<JSON
{
  "run": {
    "external_key": "ontoexplorer/upload/2026-08-30-emenu",
    "label": "upload restaurant-menu.ttl",
    "started_at": "2026-08-30T11:02:10Z",
    "ended_at": "2026-08-30T11:02:12Z",
    "status": "success"
  },
  "artifacts": [{
    "title": "Restaurant menu vocabulary (uploaded)",
    "description": "Uploaded through the OntoExplorer web form. No public source exists for these bytes — the object store is the only place they live, so it is the only distribution.",
    "conforms_to": "$T_RDF_GRAPH",
    "keywords": ["menu", "upload"],
    "external_key": "ontoexplorer/upload/2026-08-30-emenu",
    "distributions": [{
      "title": "OntoExplorer object store",
      "access_url": "https://onto.ids.unimaas.nl/ontologies/restaurant-menu",
      "download_url": "s3://ontoexplorer-raw/uploads/2026-08-30/restaurant-menu.ttl",
      "media_type": "text/turtle",
      "byte_size": 18422,
      "checksum": {"algorithm": "sha256", "value": "b7f1d0c9a2e4438f5c6d7e8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c"},
      "access_protocol": "s3",
      "auth_method": "apikey",
      "availability": "restricted",
      "access_request_url": "https://onto.ids.unimaas.nl/access"
    }]
  }]
}
JSON
)" >/dev/null
  note "upload: one s3 distribution and no upstream — the store is the only source"
}

# ================================================================================
#  The chain across the four tools
# ================================================================================

cross_tool_chain() {
  step "sulo-schema-builder emits shapes, shacl-rust validates the ontology with them"

  local out
  out=$(api POST /api/v1/advertise/produced "$TOK_SULO" "$(cat <<JSON
{
  "run": {
    "external_key": "sulo-builder/build/pizza-shapes-7",
    "label": "generate SULO-aligned artefacts for the pizza domain",
    "started_at": "2026-08-30T10:05:00Z", "ended_at": "2026-08-30T10:05:26Z", "status": "success"
  },
  "artifacts": [
    {
      "title": "Pizza domain — SHACL shapes (SULO-aligned)",
      "conforms_to": "$T_SHAPES",
      "license": "https://spdx.org/licenses/CC-BY-4.0",
      "distributions": [{
        "access_url": "https://sulo.ids.unimaas.nl/models/pizza/shapes",
        "download_url": "https://sulo.ids.unimaas.nl/models/pizza/shapes.ttl",
        "media_type": "text/turtle", "access_protocol": "https",
        "auth_method": "none", "availability": "public"
      }]
    },
    {
      "title": "Pizza domain — class diagram",
      "conforms_to": "$T_MERMAID",
      "distributions": [{
        "access_url": "https://sulo.ids.unimaas.nl/models/pizza/diagram",
        "media_type": "text/vnd.mermaid", "access_protocol": "https",
        "auth_method": "none", "availability": "public"
      }]
    }
  ]
}
JSON
)")
  ART_SHAPES=$(echo "$out" | jqr "['artifacts'][0]")
  note "shapes + diagram produced by sulo-schema-builder"

  # shacl-rust consumes two artifacts that other deployments produced. Advertising the inputs
  # is what turns four separate tools into one traceable chain.
  api POST /api/v1/advertise/consumed "$TOK_SHACL" "$(cat <<JSON
{
  "run": {
    "external_key": "gh-actions/shacl-rust/8891/attempt-1",
    "label": "validate pizza.owl against the SULO-aligned shapes",
    "started_at": "2026-08-30T10:31:00Z", "status": "running"
  },
  "artifacts": [{ "iri": "$ART_PIZZA" }, { "iri": "$ART_SHAPES" }]
}
JSON
)" >/dev/null

  api POST /api/v1/advertise/produced "$TOK_SHACL" "$(cat <<JSON
{
  "run": {
    "external_key": "gh-actions/shacl-rust/8891/attempt-1",
    "ended_at": "2026-08-30T10:31:04Z", "status": "success"
  },
  "artifacts": [{
    "title": "Validation report — pizza.owl against SULO-aligned shapes",
    "description": "sh:ValidationReport. 3 violations, 11 warnings.",
    "conforms_to": "$T_REPORT",
    "license": "https://spdx.org/licenses/CC-BY-4.0",
    "was_derived_from": ["$ART_PIZZA", "$ART_SHAPES"],
    "distributions": [{
      "access_url": "https://github.com/ensaremirerol/shacl-rust/actions/runs/8891",
      "download_url": "https://github.com/ensaremirerol/shacl-rust/actions/runs/8891/artifacts/report.ttl",
      "media_type": "text/turtle", "byte_size": 8140,
      "access_protocol": "https", "auth_method": "none", "availability": "public"
    }]
  }]
}
JSON
)" >/dev/null
  note "validation report derived from both inputs, by a CI job with no endpoint"

  step "RDFCraft maps a menu CSV into the same graph"
  out=$(api POST /api/v1/advertise/produced "$TOK_CRAFT" "$(cat <<JSON
{
  "run": {
    "external_key": "rdfcraft/project/pizza-menu/run-12",
    "label": "map menu.csv to RDF with the pizza vocabulary",
    "started_at": "2026-08-30T12:20:00Z", "ended_at": "2026-08-30T12:20:09Z", "status": "success"
  },
  "artifacts": [
    {
      "title": "Pizza menu — RML mapping",
      "conforms_to": "$T_RML",
      "license": "https://spdx.org/licenses/MIT",
      "distributions": [{
        "access_url": "https://rdfcraft.ids.unimaas.nl/projects/pizza-menu/mapping",
        "download_url": "https://rdfcraft.ids.unimaas.nl/projects/pizza-menu/mapping.ttl",
        "media_type": "text/turtle", "access_protocol": "https",
        "auth_method": "none", "availability": "public"
      }]
    },
    {
      "title": "Pizza menu — mapped RDF",
      "description": "Menu rows expressed with terms from the pizza ontology, ready for OntoExplorer to ingest.",
      "conforms_to": "$T_RDF_GRAPH",
      "was_derived_from": ["$ART_PIZZA"],
      "distributions": [{
        "access_url": "https://rdfcraft.ids.unimaas.nl/projects/pizza-menu/output",
        "download_url": "https://rdfcraft.ids.unimaas.nl/projects/pizza-menu/output.ttl",
        "media_type": "text/turtle", "byte_size": 44120,
        "access_protocol": "https", "auth_method": "none", "availability": "public"
      }]
    }
  ]
}
JSON
)")
  ART_MAPPED=$(echo "$out" | jqr "['artifacts'][1]")

  # And the loop closes: OntoExplorer ingests what RDFCraft produced. The consumed artifact is
  # another deployment's output, referenced by IRI — no copying, no coordination.
  api POST /api/v1/advertise/consumed "$TOK_ONTO" "$(cat <<JSON
{
  "run": {
    "external_key": "ontoexplorer/ingest/pizza-menu-2026-08-30",
    "label": "ingest RDFCraft output", "started_at": "2026-08-30T12:41:00Z",
    "ended_at": "2026-08-30T12:41:20Z", "status": "success"
  },
  "artifacts": [{ "iri": "$ART_MAPPED" }]
}
JSON
)" >/dev/null
  note "OntoExplorer ingests RDFCraft's output — the chain closes"

  # A failed run, because a demo where everything succeeds teaches the wrong lesson.
  api POST /api/v1/advertise/produced "$TOK_SHACL" "$(cat <<JSON
{
  "run": {
    "external_key": "gh-actions/shacl-rust/8902/attempt-1",
    "label": "validate menu graph — malformed shapes",
    "started_at": "2026-08-30T13:02:00Z", "ended_at": "2026-08-30T13:02:01Z", "status": "failed"
  },
  "artifacts": []
}
JSON
)" >/dev/null
  note "one failed run recorded, with no outputs"
}

# ---------------------------------------------------------------------- report

summary() {
  step "Loaded"
  python3 - "$TAR_URL" "${ART_PIZZA##*/}" <<'PY'
import json, sys, urllib.request
base, pizza_id = sys.argv[1], sys.argv[2]
def get(p):
    with urllib.request.urlopen(base + p) as r: return json.load(r)
c = get("/api/v1/registry")["counts"]
print(f"  {c['software']} software · {c['releases']} releases · {c['instances']} deployments "
      f"· {c['runs']} runs · {c['artifacts']} artifacts")
print()
print("  Look at:")
print(f"    {base}/software                      the four tools")
print(f"    {base}/artifacts?availability=metadata-only   described but not retrievable")
print(f"    {base}/runs                          what each deployment did")
print(f"    {base}/artifacts/{pizza_id}")
print( "        the ingested ontology — two distributions, one checksum")
print(f"    {base}/api/v1/artifacts/{pizza_id}/lineage?depth=3&direction=down")
print( "        everything derived from it, as JSON")
print()
print("  Try:")
print(f"    curl -H 'Accept: text/turtle' {base}/software/…")
print(f"    curl '{base}/api/v1/capabilities?produces=…/type/shacl-shapes-graph'")
PY
}

# ------------------------------------------------------------------------ main

case "${1:-}" in
  --down) stack_down ;;
  --no-stack) ;;
  "") stack_up ;;
  *) echo "usage: $0 [--no-stack|--down]" >&2; exit 2 ;;
esac

if ! curl -sf "$TAR_URL/healthz" >/dev/null 2>&1; then
  warn "no registry at $TAR_URL — start one, or drop --no-stack"
  exit 1
fi

[ "${NO_ASSETS:-0}" = "1" ] || publish_assets
register_types
register_software
register_instances
ingest_story
cross_tool_chain
summary
