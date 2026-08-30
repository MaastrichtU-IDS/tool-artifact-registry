#!/usr/bin/env python3
"""onto-app — a stand-in for an OntoExplorer deployment.

**This is a simulation.** It is not the real OntoExplorer and does not talk to it. What it
reproduces is the half of the story the registry exists to make possible: a deployment that
registers a *standing interest*, is told when something matching it appears, fetches it,
does real work on it, and advertises what that work produced — without ever having heard of
the tool that made the input.

The loop, in order:

1. register (or re-find) a subscription: "OWL ontologies I did not produce myself";
2. **pull** its deliveries — `GET /api/v1/subscriptions/{id}/deliveries`. Pull, not webhook,
   because this program has no public address and a real ingester behind a hospital firewall
   does not either;
3. for each delivery: fetch the ontology over HTTP, verify the advertised sha256 against the
   bytes that actually arrived, parse it, and compute the transitive closure of its asserted
   subclass edges;
4. advertise the *consumption* and the three derived artifacts on one Run, each pointing back
   at the ontology with `prov:wasDerivedFrom`;
5. acknowledge the delivery — **after** the work, never before. The guarantee is at-least-once.

Step 5 is the whole reason step 3 keys its work on the artifact IRI: a delivery can arrive
twice, and a subscriber that is not idempotent turns that into duplicated artifacts.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import sys
import threading
import time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tarclient import (  # noqa: E402
    Log,
    Registry,
    announce_endpoint,
    fetch_bytes,
    load_config,
    serve_directory,
    sha256_bytes,
    sha256_file,
    write_marker,
    write_pid,
)

LOG = Log("onto")
SUBSCRIPTION_LABEL = "owl-ontologies-to-ingest"

IDS = {
    "name": "Maastricht University — Institute of Data Science",
    "kind": "organization",
    "identifier": "https://ror.org/02jz4aj89",
}

RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
OWL_CLASS = "http://www.w3.org/2002/07/owl#Class"
OWL_OBJECT_PROPERTY = "http://www.w3.org/2002/07/owl#ObjectProperty"
OWL_DATATYPE_PROPERTY = "http://www.w3.org/2002/07/owl#DatatypeProperty"
OWL_ONTOLOGY = "http://www.w3.org/2002/07/owl#Ontology"
RDFS_SUBCLASS_OF = "http://www.w3.org/2000/01/rdf-schema#subClassOf"
RDFS_LABEL = "http://www.w3.org/2000/01/rdf-schema#label"
SKOS_DEFINITION = "http://www.w3.org/2004/02/skos/core#definition"


def now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


# ============================================================ a small Turtle reader
#
# Honest about its scope: this reads the subset of Turtle that matters for counting terms and
# walking a class hierarchy — prefixes, subject/predicate/object statements, `;` and `,`
# continuations, and blank-node and collection groups treated as opaque objects. It is not a
# conformant parser and does not pretend to be one. It is here so that "ingest" means actually
# reading the bytes rather than asserting numbers the demo made up.


def _strip_comments(text: str) -> str:
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        if c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    break
                j += 1
            out.append(text[i : j + 1])
            i = j + 1
        elif c == "<":
            j = text.find(">", i)
            j = n if j < 0 else j
            out.append(text[i : j + 1])
            i = j + 1
        elif c == "#":
            j = text.find("\n", i)
            i = n if j < 0 else j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def _split_top_level(text: str, seps: str) -> list[str]:
    """Split on separator characters that are not inside <>, "", [] or ()."""
    chunks, buf, depth, i, n = [], [], 0, 0, len(text)
    while i < n:
        c = text[i]
        if c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    break
                j += 1
            buf.append(text[i : j + 1])
            i = j + 1
            continue
        if c == "<":
            j = text.find(">", i)
            j = n - 1 if j < 0 else j
            buf.append(text[i : j + 1])
            i = j + 1
            continue
        if c in "[(":
            depth += 1
        elif c in "])":
            depth -= 1
        if depth == 0 and c in seps:
            chunks.append("".join(buf))
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    if "".join(buf).strip():
        chunks.append("".join(buf))
    return chunks


def _tokens(text: str) -> list[str]:
    """Whitespace tokens, with <IRIs>, "literals" and [ ]/( ) groups kept whole."""
    toks, buf, i, n = [], [], 0, len(text)

    def flush():
        if buf:
            toks.append("".join(buf))
            buf.clear()

    while i < n:
        c = text[i]
        if c.isspace():
            flush()
            i += 1
        elif c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    break
                j += 1
            j += 1
            # A language tag or datatype belongs to the literal it follows.
            while j < n and not text[j].isspace() and text[j] not in ",;":
                j += 1
            flush()
            toks.append(text[i:j])
            i = j
        elif c == "<":
            j = text.find(">", i)
            j = n - 1 if j < 0 else j
            flush()
            toks.append(text[i : j + 1])
            i = j + 1
        elif c in "[(":
            depth, j = 0, i
            while j < n:
                if text[j] in "[(":
                    depth += 1
                elif text[j] in "])":
                    depth -= 1
                    if depth == 0:
                        break
                j += 1
            flush()
            toks.append(text[i : j + 1])
            i = j + 1
        elif c in ",;":
            flush()
            toks.append(c)
            i += 1
        else:
            buf.append(c)
            i += 1
    flush()
    return [t for t in toks if t]


def parse_turtle(text: str) -> list[tuple[str, str, str]]:
    """Return expanded (subject, predicate, object) triples. Blank-node groups stay opaque."""
    text = _strip_comments(text)
    prefixes: dict[str, str] = {}
    triples: list[tuple[str, str, str]] = []

    def expand(tok: str) -> str:
        if tok.startswith("<") and tok.endswith(">"):
            return tok[1:-1]
        if tok == "a":
            return RDF_TYPE
        m = re.match(r"^([A-Za-z][\w.-]*)?:(\S*)$", tok)
        if m and (m.group(1) or "") in prefixes:
            return prefixes[m.group(1) or ""] + m.group(2)
        return tok

    for chunk in _split_top_level(text, "."):
        chunk = chunk.strip()
        if not chunk:
            continue
        if chunk.startswith("@prefix") or chunk[:7].upper() == "PREFIX ":
            parts = _tokens(chunk)
            if len(parts) >= 3:
                prefixes[parts[1].rstrip(":")] = parts[2].strip("<>")
            continue
        if chunk.startswith("@base") or chunk[:5].upper() == "BASE ":
            continue
        toks = _tokens(chunk)
        if len(toks) < 3:
            continue
        subject = expand(toks[0])
        predicate = None
        for tok in toks[1:]:
            if tok == ";":
                predicate = None
            elif tok == ",":
                continue
            elif predicate is None:
                predicate = expand(tok)
            else:
                triples.append((subject, predicate, expand(tok)))
    return triples


# ============================================================ the ingest itself


def ingest(turtle: str) -> dict:
    """Read the ontology and derive what can honestly be derived from it cheaply.

    The `inferred` part is the transitive closure of the asserted named-superclass edges —
    the simplest real entailment there is, and genuinely more than the file states. What it is
    *not* is an OWL reasoner: the existential restriction and the defined class in this
    ontology are exactly the axioms an ELK-style reasoner would use, and they are ignored here.
    Saying that plainly is better than calling this "reasoning".
    """
    triples = parse_turtle(turtle)
    by_type: dict[str, set[str]] = {}
    for s, p, o in triples:
        if p == RDF_TYPE:
            by_type.setdefault(o, set()).add(s)

    classes = {c for c in by_type.get(OWL_CLASS, set()) if not c.startswith("[")}
    object_properties = {c for c in by_type.get(OWL_OBJECT_PROPERTY, set()) if not c.startswith("[")}
    datatype_properties = {c for c in by_type.get(OWL_DATATYPE_PROPERTY, set()) if not c.startswith("[")}
    ontology_iri = next(iter(by_type.get(OWL_ONTOLOGY, set())), None)

    asserted: set[tuple[str, str]] = set()
    for s, p, o in triples:
        if p == RDFS_SUBCLASS_OF and not o.startswith("[") and not s.startswith("["):
            asserted.add((s, o))

    parents: dict[str, set[str]] = {}
    for child, parent in asserted:
        parents.setdefault(child, set()).add(parent)

    entailed: set[tuple[str, str]] = set()
    for child in list(parents):
        seen, frontier = set(), list(parents[child])
        while frontier:
            p = frontier.pop()
            if p in seen:
                continue
            seen.add(p)
            frontier.extend(parents.get(p, ()))
        for ancestor in seen:
            if (child, ancestor) not in asserted and child != ancestor:
                entailed.add((child, ancestor))

    labelled = {s for s, p, _ in triples if p == RDFS_LABEL}
    defined = {s for s, p, _ in triples if p == SKOS_DEFINITION}
    depth = 0
    for child in parents:
        d, cur = 0, child
        while cur in parents and d < 50:
            cur = sorted(parents[cur])[0]
            d += 1
        depth = max(depth, d)

    return {
        "ontology_iri": ontology_iri,
        "triples": len(triples),
        "classes": sorted(classes),
        "object_properties": sorted(object_properties),
        "datatype_properties": sorted(datatype_properties),
        "asserted_subclass_edges": sorted(asserted),
        "entailed_subclass_edges": sorted(entailed),
        "max_hierarchy_depth": depth,
        "classes_without_definition": sorted(classes - defined),
        "classes_without_label": sorted(classes - labelled),
    }


def render_inferred(report: dict, source_url: str, source_sum: str) -> str:
    lines = [
        "# Entailed subclass axioms, materialised as asserted triples.",
        "#",
        "# Derived by onto-app (simulated OntoExplorer) from:",
        "#   %s" % source_url,
        "#   sha256 %s" % source_sum,
        "#",
        "# Scope: the transitive closure of asserted named-superclass edges. Restrictions and",
        "# equivalent-class axioms in the source are NOT used — this is closure, not OWL",
        "# reasoning, and the artifact record says the same thing.",
        "",
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .",
        "",
    ]
    for child, ancestor in report["entailed_subclass_edges"]:
        lines.append("<%s> rdfs:subClassOf <%s> ." % (child, ancestor))
    return "\n".join(lines) + "\n"


# ============================================================ the deployment


def ensure_subscription(reg: Registry, cfg: dict) -> str:
    """Find this deployment's subscription, or create it.

    Re-finding it by label matters: a deployment restarts, and a restart that created a second
    identical subscription would double every future notification. The registry caps
    subscriptions per Instance for the same reason.
    """
    instance_id = cfg["instance_id"]
    existing = reg.get("/api/v1/instances/%s/subscriptions" % instance_id)
    for item in existing.get("items", []):
        if item.get("label") == SUBSCRIPTION_LABEL:
            LOG.say("re-using subscription %s" % item["id"])
            return item["id"]

    body = {
        "label": SUBSCRIPTION_LABEL,
        "filter": {
            # The capability question in event form: "tell me when an OWL ontology appears."
            "conforms_to": [cfg["types"]["owl"]],
            # Only what this deployment can actually fetch. metadata-only artifacts are the
            # common case on a health-data registry (spec §6.2), and an ingester that is woken
            # for one has nothing it can do about it.
            "availability": ["public", "restricted"],
            # Produce advertisements only. A consume advertisement also makes an artifact
            # appear, but it is a different event and this tool does not care about it.
            "roles": ["produced"],
            # Default anyway, and named here because it is load-bearing: everything below
            # advertises artifacts too, and without this the ingester would wake itself up.
            "exclude_own": True,
        },
        # No webhook_url. This program has no inbound address, which is the normal case for a
        # tool inside someone else's network, so it drains the queue itself.
    }
    created = reg.post("/api/v1/instances/%s/subscriptions" % instance_id, body)
    sub = created["subscription"]
    LOG.say("registered subscription %s" % sub["id"])
    LOG.detail("filter      conforms_to=%s, availability=public|restricted, roles=produced, exclude_own=true"
               % cfg["types"]["owl"].rsplit("/", 1)[-1])
    LOG.detail("mode        %s — no webhook, so nothing has to be able to reach this process" % sub["delivery_mode"])
    LOG.detail("pull_url    %s" % sub["pull_url"])
    return sub["id"]


def handle(reg: Registry, cfg: dict, item: dict, out_dir: str, base_url: str, state: dict) -> bool:
    """Process one delivery. Returns True when an ontology was actually ingested."""
    note = item.get("notification") or {}
    artifact = note.get("artifact") or {}
    artifact_iri = item["artifact_iri"]
    artifact_id = artifact_iri.rsplit("/", 1)[-1]
    title = artifact.get("title") or artifact_iri
    # "Biobank Sample Ontology (OWL, Turtle)" names a serialisation; the things derived from it
    # are not in that serialisation, so the parenthetical would be wrong on their titles.
    short = re.sub(r"\s*\([^()]*\)\s*$", "", title).strip() or title

    LOG.say("delivery seq=%s: %s" % (item["seq"], title))
    LOG.detail("advertised by  %s" % (note.get("instance") or "?"))
    LOG.detail("artifact       %s" % artifact_iri)

    # At-least-once means duplicates are the subscriber's problem, and the subscriber is the
    # only party that can solve them. Keyed on the artifact IRI rather than the delivery id: a
    # delivery row is unique per (subscription, artifact, role), so what comes back twice is
    # the *same* row, re-read because nothing acknowledged it — a crash between doing the work
    # and acknowledging it is precisely the case this covers.
    if artifact_iri in state["ingested"]:
        LOG.detail("already ingested — skipping. At-least-once delivery is why this check exists.")
        return False

    dist = None
    for d in artifact.get("distributions", []):
        if d.get("download_url"):
            dist = d
            break
    if not dist:
        LOG.warn("no distribution with a downloadURL — nothing to fetch, leaving it alone")
        state["ingested"][artifact_iri] = {"skipped": "no downloadURL"}
        return False

    started = now()
    LOG.detail("fetching       %s" % dist["download_url"])
    data = fetch_bytes(dist["download_url"])
    got = sha256_bytes(data)
    want = (dist.get("checksum") or {}).get("value")
    if want and want != got:
        LOG.warn("checksum mismatch: advertised %s…, got %s… — refusing to ingest" % (want[:16], got[:16]))
        return False
    LOG.detail("checksum       %s… matches the advertisement (%d bytes)" % (got[:16], len(data)))

    report = ingest(data.decode("utf-8"))
    LOG.say(
        "ingested: %d triples, %d classes, %d object properties, %d datatype properties"
        % (report["triples"], len(report["classes"]), len(report["object_properties"]),
           len(report["datatype_properties"]))
    )
    LOG.detail(
        "%d asserted subclass edges, %d entailed by transitive closure, max depth %d"
        % (len(report["asserted_subclass_edges"]), len(report["entailed_subclass_edges"]),
           report["max_hierarchy_depth"])
    )

    inferred_name = "%s-inferred.ttl" % artifact_id
    metrics_name = "%s-metrics.json" % artifact_id
    with open(os.path.join(out_dir, inferred_name), "w") as fh:
        fh.write(render_inferred(report, dist["download_url"], got))
    metrics = {
        "source_artifact": artifact_iri,
        "source_sha256": got,
        "ingested_at": started,
        "counts": {
            "triples": report["triples"],
            "classes": len(report["classes"]),
            "object_properties": len(report["object_properties"]),
            "datatype_properties": len(report["datatype_properties"]),
            "asserted_subclass_edges": len(report["asserted_subclass_edges"]),
            "entailed_subclass_edges": len(report["entailed_subclass_edges"]),
            "max_hierarchy_depth": report["max_hierarchy_depth"],
        },
        "quality": {
            "classes_without_label": report["classes_without_label"],
            "classes_without_definition": report["classes_without_definition"],
        },
    }
    with open(os.path.join(out_dir, metrics_name), "w") as fh:
        json.dump(metrics, fh, indent=2)

    # One Run, keyed on the artifact, so a replayed delivery attaches to the same Run instead
    # of inventing a second one. Both advertisements below use this key.
    run_key = "ontoexplorer/ingest/%s" % artifact_id

    # The consume side. Without it the graph would say these three artifacts appeared from
    # nowhere; with it, `prov:used` and `prov:generated` hang off one activity and the lineage
    # reads in both directions.
    reg.post(
        "/api/v1/advertise/consumed",
        {
            "run": {
                "external_key": run_key,
                "label": "ingest %s" % title,
                "started_at": started,
                "status": "running",
            },
            "artifacts": [{"iri": artifact_iri}],
        },
    )
    LOG.detail("advertised the consumption — prov:used, on run %s" % run_key)

    inferred_path = os.path.join(out_dir, inferred_name)
    metrics_path = os.path.join(out_dir, metrics_name)
    version = artifact.get("version")
    produced = {
        "run": {"external_key": run_key, "ended_at": now(), "status": "success"},
        "artifacts": [
            {
                "title": "%s — entailed subclass axioms" % short,
                "description": (
                    "The transitive closure of the asserted named-superclass edges of the source "
                    "ontology, materialised as asserted rdfs:subClassOf triples. Closure only: the "
                    "restriction and equivalent-class axioms in the source are not used, so this is "
                    "strictly less than an OWL reasoner would entail."
                ),
                "conforms_to": cfg["types"]["inferred"],
                "license": artifact.get("license"),
                "version": version,
                "keywords": ["inferred", "subclass", "closure"],
                "issued": now(),
                # No `creators`: nobody authored this. A program derived it, and that is already
                # recorded — the run, the instance, and prov:wasAttributedTo from the credential.
                "publisher": IDS,
                "was_derived_from": [artifact_iri],
                "external_key": "%s/inferred" % run_key,
                "distributions": [
                    {
                        "download_url": "%s/%s" % (base_url, inferred_name),
                        "media_type": "text/turtle",
                        "byte_size": os.path.getsize(inferred_path),
                        "checksum": {"algorithm": "sha256", "value": sha256_file(inferred_path)},
                        "auth_method": "none",
                        "availability": "public",
                    }
                ],
            },
            {
                "title": "%s — term index" % short,
                "description": (
                    "Labels and definitions of every term in the ontology, indexed for lookup inside "
                    "this deployment's database. There is no file: the index is a set of database rows "
                    "and always will be. It is described so it can be found and cited, and the absence "
                    "of a downloadURL is the model saying so (spec §6.2)."
                ),
                "conforms_to": cfg["types"]["term_index"],
                "keywords": ["index", "search"],
                "issued": now(),
                "publisher": IDS,
                "was_derived_from": [artifact_iri],
                "external_key": "%s/term-index" % run_key,
                "distributions": [
                    {
                        "title": "index table, inside the deployment",
                        "availability": "metadata-only",
                        "access_request_url": base_url + "/",
                    }
                ],
            },
            {
                "title": "%s — ingest metrics" % short,
                "description": (
                    "Counts and quality signals computed during the ingest: term counts, hierarchy "
                    "depth, and which classes carry no label or no definition."
                ),
                "conforms_to": cfg["types"]["metrics"],
                "license": "https://spdx.org/licenses/CC0-1.0",
                "keywords": ["metrics", "quality"],
                "issued": now(),
                "publisher": IDS,
                "was_derived_from": [artifact_iri],
                "external_key": "%s/metrics" % run_key,
                "distributions": [
                    {
                        "download_url": "%s/%s" % (base_url, metrics_name),
                        "media_type": "application/json",
                        "byte_size": os.path.getsize(metrics_path),
                        "checksum": {"algorithm": "sha256", "value": sha256_file(metrics_path)},
                        "auth_method": "none",
                        "availability": "public",
                    }
                ],
            },
        ],
    }
    result = reg.post("/api/v1/advertise/produced", produced)
    LOG.say("advertised 3 derived artifacts, each prov:wasDerivedFrom the ontology")
    for iri in result["artifacts"]:
        LOG.detail("derived   %s" % iri)
    LOG.detail("one of them is metadata-only: an index in a database has no file to fetch")

    state["ingested"][artifact_iri] = {"run": result["run"], "derived": result["artifacts"], "at": now()}
    return True


def main() -> int:
    ap = argparse.ArgumentParser(description="Simulated OntoExplorer deployment.")
    ap.add_argument("--config", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--ready-file", help="written once the subscription exists")
    ap.add_argument("--done-file", help="written once --want ontologies have been ingested")
    ap.add_argument("--pid-file")
    ap.add_argument("--want", type=int, default=1, help="ontologies to ingest before reporting done")
    ap.add_argument("--poll", type=float, default=2.0, help="seconds between polls")
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--hold", action="store_true", help="keep serving derived files after finishing")
    args = ap.parse_args()

    write_pid(args.pid_file)
    cfg = load_config(args.config)
    reg = Registry(cfg["registry"], cfg["token"])
    os.makedirs(args.out, exist_ok=True)

    me = reg.get("/api/v1/whoami")
    LOG.say("starting up as %s" % (me.get("instance") or "?"))
    LOG.detail("credential %s, scopes %s" % (me.get("credential"), ",".join(me.get("scopes") or [])))

    # The page `access_request_url` points at. A metadata-only artifact says "there are no
    # bytes, here is who to ask"; that is only useful if the asking part resolves.
    with open(os.path.join(args.out, "index.html"), "w") as fh:
        fh.write(
            "<!doctype html>\n<title>onto-app (simulated) — derived artifacts</title>\n"
            "<style>body{font:15px/1.6 system-ui,sans-serif;max-width:44rem;margin:3rem auto;"
            "padding:0 1rem}</style>\n"
            "<h1>onto-app (simulated)</h1>\n"
            "<p>Files derived by a simulated OntoExplorer deployment, in the tool-artifact-registry"
            " two-application demo. It is not the real OntoExplorer.</p>\n"
            "<p>The <strong>term index</strong> is advertised as <code>metadata-only</code>: it is a"
            " set of rows in this deployment's database and has no file form, so its record carries"
            " no <code>downloadURL</code> at all. This page is where its"
            " <code>accessRequestURL</code> points — in a real deployment it would be the access"
            " request form.</p>\n"
            "<p>Everything else below is a real file with a real checksum.</p>\n"
            '<p><a href="ingested.json">ingested.json</a> — what this deployment has already '
            "handled, which is how a duplicate delivery becomes a no-op.</p>\n"
        )
    base_url, httpd = serve_directory(args.out)
    LOG.say("serving its derived files at %s" % base_url)
    announce_endpoint(reg, cfg["instance_id"], base_url)
    LOG.detail("recorded that endpoint on its own Instance record — a deployment may maintain it")

    sid = ensure_subscription(reg, cfg)
    state = {"ingested": {}}
    state_path = os.path.join(args.out, "ingested.json")
    if os.path.exists(state_path):
        with open(state_path) as fh:
            state["ingested"] = json.load(fh)

    write_marker(args.ready_file, {"subscription": sid})
    LOG.say("polling for deliveries every %ss — nothing needs to reach this process" % args.poll)

    deadline = time.time() + args.timeout
    ingested = 0
    idle_notices = 0
    while ingested < args.want and time.time() < deadline:
        # No cursor: the registry resumes from this subscription's own acknowledged position,
        # so a subscriber that keeps no state of its own still makes progress.
        page = reg.get("/api/v1/subscriptions/%s/deliveries" % sid, limit=10)
        items = page.get("items", [])
        if not items:
            idle_notices += 1
            if idle_notices == 1:
                LOG.detail("queue empty (cursor %s) — waiting" % page.get("cursor"))
            time.sleep(args.poll)
            continue
        idle_notices = 0
        LOG.say("%d delivery/deliveries waiting, %s more after this page"
                % (len(items), page.get("remaining")))
        for item in items:
            try:
                if handle(reg, cfg, item, args.out, base_url, state):
                    ingested += 1
            except Exception as exc:  # noqa: BLE001 - a failed ingest must not lose the queue
                LOG.warn("could not handle delivery %s: %s" % (item.get("seq"), exc))
                LOG.detail("not acknowledging it; at-least-once means it will come back")
                with open(state_path, "w") as fh:
                    json.dump(state["ingested"], fh, indent=2)
                time.sleep(args.poll)
                continue
            # Acknowledged only now that the work is on disk and in the registry. Acknowledging
            # on read would turn a crash here into a silently dropped ontology.
            acked = reg.post("/api/v1/subscriptions/%s/deliveries/ack" % sid, {"cursor": item["seq"]})
            LOG.detail("acknowledged up to seq %s, %s left" % (acked["cursor"], acked["remaining"]))
        with open(state_path, "w") as fh:
            json.dump(state["ingested"], fh, indent=2)

    if ingested < args.want:
        LOG.warn("timed out after %.0fs having ingested %d of %d" % (args.timeout, ingested, args.want))
        write_marker(args.done_file, {"ingested": ingested, "timed_out": True})
        httpd.shutdown()
        return 1

    # One deliberate replay, because the guarantee is worth seeing rather than being told. The
    # same delivery comes back when read from an earlier cursor; the dedupe above makes the
    # second pass a no-op instead of a second set of artifacts.
    replay = reg.get("/api/v1/subscriptions/%s/deliveries" % sid, cursor=0, limit=10)
    LOG.say("replay check: reading from cursor 0 returns %d delivery/deliveries again"
            % len(replay.get("items", [])))
    for item in replay.get("items", []):
        handle(reg, cfg, item, args.out, base_url, state)

    LOG.say("done — %d ontology/ontologies ingested" % ingested)
    write_marker(args.done_file, {"ingested": ingested, "subscription": sid, "base_url": base_url})

    if not args.hold:
        httpd.shutdown()
        return 0

    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    signal.signal(signal.SIGINT, lambda *_: stop.set())
    stop.wait()
    LOG.say("shutting down")
    httpd.shutdown()
    return 0


if __name__ == "__main__":
    sys.exit(main())
