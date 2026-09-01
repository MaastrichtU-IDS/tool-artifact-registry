#!/usr/bin/env python3
"""One program, two deployments, two kinds of credential.

`run-two-credentials-demo.sh` starts this three times:

  --phase export    as graph-publisher, holding a registry API token a curator minted for it
  --phase validate  as shacl-manager, holding nothing but a Keycloak client secret
  --phase revise    as graph-publisher again, acting on what shacl-manager reported

It is deliberately *one* file. The two deployments differ in how they authenticate and in how
their records came to exist; they do not differ in a single line of what follows, and splitting
them into two programs would have implied otherwise. The whole difference lives in
`open_registry` below and in the twenty lines of `announce_self`.

Standard library only, like everything else in this directory. Every call is `/api/v1/...`.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import tarclient  # noqa: E402
from tarclient import ClientCredentials, Log, Registry  # noqa: E402

EX = "https://example.org/cohort#"


def now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat()


# ------------------------------------------------------------------ credentials


def open_registry(cfg: dict, log: Log) -> Registry:
    """Build this deployment's view of the registry from what its operator provisioned.

    This is the entire contrast the demo exists to show, and it is nine lines long. One
    deployment was handed a secret the registry issued; the other was handed a secret its
    identity provider issued, and the registry was told to recognise it. Everything after this
    function is common code.
    """
    cred = cfg["credential"]
    if cred["kind"] == "registry-token":
        log.say("credential: a registry API token, minted by a curator, held until rotated")
        log.detail("prefix %s… — the registry stores its hash and can revoke it" % cred["token"][:12])
        return Registry(cfg["registry"], token=cred["token"])

    if cred["kind"] == "oidc-client-credentials":
        log.say("credential: a client secret for %s" % cred["issuer"])
        log.detail("no registry token exists for this deployment, and none ever will")
        provider = ClientCredentials(cred["issuer"], cred["client_id"], cred["client_secret"])
        claims = tarclient.jwt_claims(provider.token())
        aud = claims.get("aud")
        log.detail("fetched a token: azp=%s, expires in %ss"
                   % (claims.get("azp"),
                      int(claims.get("exp", 0) - datetime.datetime.now(datetime.timezone.utc).timestamp())))
        log.detail("aud: %s" % (", ".join(aud) if isinstance(aud, list) else aud))
        log.detail("it will be refetched when it expires; the registry holds nothing to rotate")
        return Registry(cfg["registry"], credential=provider)

    raise SystemExit("unknown credential kind %r" % cred["kind"])


def announce_self(reg: Registry, cfg: dict, log: Log) -> dict:
    """Create or maintain this deployment's own record — the self-registration path.

    No curator touched this record. The credential says which client id is calling; the
    software record says that client id may register deployments of *it*; the registry does the
    rest. Which Instance this is never comes from the body — sending an `oidc_client_id` here
    is not even possible, because `SelfAnnounceIn` has no such field.

    Idempotent by `instance_key`: a deployment that announces on every boot updates one record
    instead of littering the registry with a new one each time.
    """
    ann = cfg["self_registration"]
    body = {
        "software": ann["software"],
        "instance_key": ann["instance_key"],
        "label": ann["label"],
        "description": ann["description"],
        "availability": ann.get("availability", "restricted"),
        "jurisdiction": ann.get("jurisdiction", "NL"),
    }
    record = reg.put("/api/v1/instances/self", body)
    log.say("PUT /api/v1/instances/self → %s" % record["iri"])
    log.detail("instance_key %r is what makes the next announcement update this record"
               % ann["instance_key"])
    return record


def show_whoami(reg: Registry, log: Log) -> dict:
    """What the registry makes of this credential — the first thing to curl when a write is
    refused, and the only place the two deployments' differing credentials are visible side by
    side."""
    who = reg.get("/api/v1/whoami")
    log.say("whoami: credential=%s, acting as %s" % (who.get("credential"), who.get("instance") or "no deployment"))
    if who.get("may_register_deployments_of"):
        log.detail("may register deployments of %s" % who["may_register_deployments_of"])
    log.detail("scopes: %s" % (", ".join(who.get("scopes") or []) or "none"))
    return who


# ------------------------------------------------------------------ the work


def distribution(path: str, media_type: str, title: str) -> dict:
    """Describe bytes that really exist, on terms that are really true.

    `file:` because these two deployments run on one machine and write to their own directories
    — there is no server to point at, and inventing an `https://` URL that resolves to nothing
    would make the record a decoration. The checksum and the byte size are computed from the
    file that was just written, so a consumer can verify what it fetched.
    """
    return {
        "title": title,
        "download_url": "file://" + os.path.abspath(path),
        "media_type": media_type,
        "byte_size": os.path.getsize(path),
        "access_protocol": "file",
        "auth_method": "none",
        "availability": "restricted",
        "checksum": {"algorithm": "sha256", "value": tarclient.sha256_file(path)},
    }


COHORT = [
    ("p01", "Utrecht", True),
    ("p02", "Maastricht", True),
    ("p03", "Maastricht", True),
    ("p04", "Nijmegen", False),   # the one the shapes graph will catch
    ("p05", "Utrecht", True),
    ("p06", "Maastricht", True),
]


def write_cohort(path: str, records: list[tuple[str, str, bool]]) -> None:
    """A small RDF graph, written one triple per line so the validator downstream can read it
    with the standard library and no parser. Fixed content, so a second run of the demo
    produces the same bytes and the same checksum."""
    with open(path, "w") as fh:
        fh.write("@prefix ex: <%s> .\n" % EX)
        fh.write("@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\n")
        for pid, site, consent in records:
            fh.write("ex:%s a ex:Participant .\n" % pid)
            fh.write('ex:%s ex:site "%s" .\n' % (pid, site))
            if consent:
                fh.write('ex:%s ex:consent "true"^^xsd:boolean .\n' % pid)
            fh.write("\n")


SHAPES = """@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <%s> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:ParticipantShape a sh:NodeShape ;
    sh:targetClass ex:Participant ;
    sh:property [ sh:path ex:site ;    sh:datatype xsd:string ;  sh:minCount 1 ] ;
    sh:property [ sh:path ex:consent ; sh:datatype xsd:boolean ; sh:minCount 1 ;
                  sh:message "a participant with no recorded consent may not be published" ] .
""" % EX


def read_graph(text: str) -> dict[str, set[str]]:
    """Predicates per subject, from the one-triple-per-line form written above.

    Not a Turtle parser and not pretending to be one. It reads the shape of file this
    deployment's own producer emits, which is what a real ingest of a known feed does too.
    """
    out: dict[str, set[str]] = {}
    for line in text.splitlines():
        m = re.match(r"^ex:(\S+)\s+(?:a\s+ex:(\S+)|ex:(\S+))\s", line)
        if not m:
            continue
        subject = m.group(1)
        predicate = "rdf:type/" + m.group(2) if m.group(2) else m.group(3)
        out.setdefault(subject, set()).add(predicate)
    return out


def phase_export(reg: Registry, cfg: dict, out: str, log: Log) -> None:
    """Produce a graph and the shapes it claims to satisfy, and advertise both."""
    graph_path = os.path.join(out, "cohort.ttl")
    shapes_path = os.path.join(out, "cohort-shapes.ttl")
    write_cohort(graph_path, COHORT)
    with open(shapes_path, "w") as fh:
        fh.write(SHAPES)
    log.say("wrote %d participants to %s" % (len(COHORT), os.path.basename(graph_path)))

    key = "two-credentials/export"
    result = reg.post("/api/v1/advertise/produced", {
        "run": {"external_key": key, "label": "Cohort export", "status": "success",
                "started_at": now(), "ended_at": now()},
        "artifacts": [
            {
                "external_key": key + "/graph",
                "title": "Participant cohort graph",
                "description": "A small RDF graph of study participants, written by the "
                               "simulated graph-publisher deployment in this demo.",
                "conforms_to": cfg["types"]["graph"],
                "keywords": ["RDF Graphs"],
                "license": "https://creativecommons.org/licenses/by/4.0/",
                "distributions": [distribution(graph_path, "text/turtle", "Turtle serialisation")],
            },
            {
                "external_key": key + "/shapes",
                "title": "Participant cohort shapes",
                "description": "The SHACL shapes the cohort graph is expected to satisfy.",
                "conforms_to": cfg["types"]["shapes"],
                "keywords": ["SHACL"],
                "license": "https://creativecommons.org/licenses/by/4.0/",
                "distributions": [distribution(shapes_path, "text/turtle", "Turtle serialisation")],
            },
        ],
    })
    log.say("advertised 2 artifacts on run %s" % result["run"].rsplit("/", 1)[-1])
    for iri in result["artifacts"]:
        log.detail(iri)


def newest_of_type(reg: Registry, type_iri: str, log: Log) -> dict:
    """Find something to work on, through the same public search anyone else would use.

    This deployment has never heard of the one that produced it. It knows what it can consume —
    its own capability — and asks the registry who has any.
    """
    found = reg.get("/api/v1/artifacts", conforms_to=type_iri, limit=20)
    items = sorted(found["items"], key=lambda a: a["iri"])
    if not items:
        raise SystemExit("nothing of type %s is registered here" % type_iri)
    log.detail("%d artifact(s) of that type; taking the earliest, %r" % (len(items), items[0]["title"]))
    return reg.get("/api/v1/artifacts/%s" % items[0]["id"])


def fetch_verified(artifact: dict, log: Log) -> str:
    """Fetch a distribution and check it against the checksum the record carries.

    The registry stores no bytes (spec D1). It said where they are and what they should hash
    to; believing the second half without checking is how a pipeline ingests the wrong file.
    """
    dist = artifact["distributions"][0]
    raw = tarclient.fetch_bytes(dist["download_url"])
    digest = tarclient.sha256_bytes(raw)
    expected = (dist.get("checksum") or {}).get("value")
    if expected and digest != expected:
        raise SystemExit("checksum mismatch on %s" % dist["download_url"])
    log.detail("fetched %d bytes, sha256 matches the record" % len(raw))
    return raw.decode()


def phase_validate(reg: Registry, cfg: dict, out: str, log: Log) -> None:
    """Consume the other deployment's graph and shapes, and produce what checking them yields."""
    graph_art = newest_of_type(reg, cfg["types"]["graph"], log)
    shapes_art = newest_of_type(reg, cfg["types"]["shapes"], log)
    graph_text = fetch_verified(graph_art, log)
    fetch_verified(shapes_art, log)

    key = "two-credentials/validate"
    # Advertise the consumption *before* doing the work, with the run still `running`. A run
    # that dies mid-way then still says what it had taken in, which is the half of the
    # provenance a crash usually destroys.
    reg.post("/api/v1/advertise/consumed", {
        "run": {"external_key": key, "label": "Validate cohort graph", "status": "running",
                "started_at": now()},
        "artifacts": [{"iri": graph_art["iri"]}, {"iri": shapes_art["iri"]}],
    })
    log.say("consumed %s and its shapes" % graph_art["title"])

    subjects = read_graph(graph_text)
    participants = {s: p for s, p in subjects.items() if "rdf:type/Participant" in p}
    violations = sorted(s for s, p in participants.items() if "consent" not in p)
    log.say("%d participants checked, %d violation(s)" % (len(participants), len(violations)))

    report_path = os.path.join(out, "validation-report.ttl")
    with open(report_path, "w") as fh:
        fh.write("@prefix sh: <http://www.w3.org/ns/shacl#> .\n@prefix ex: <%s> .\n\n" % EX)
        fh.write("[] a sh:ValidationReport ;\n    sh:conforms %s" % ("true" if not violations else "false"))
        for subject in violations:
            fh.write(" ;\n    sh:result [ a sh:ValidationResult ;\n")
            fh.write("        sh:focusNode ex:%s ;\n        sh:resultPath ex:consent ;\n" % subject)
            fh.write("        sh:sourceConstraintComponent sh:MinCountConstraintComponent ;\n")
            fh.write('        sh:resultMessage "a participant with no recorded consent may not be published" ]')
        fh.write(" .\n")

    summary_path = os.path.join(out, "conformance-summary.json")
    with open(summary_path, "w") as fh:
        json.dump({"conforms": not violations, "participants": len(participants),
                   "violations": violations, "validated": graph_art["iri"]}, fh, indent=2)

    result = reg.post("/api/v1/advertise/produced", {
        "run": {"external_key": key, "status": "success", "ended_at": now()},
        "artifacts": [
            {
                "external_key": key + "/report",
                "title": "Cohort graph validation report",
                "description": "One constraint — every participant must record consent — checked "
                               "by direct inspection of the graph. Not the output of a full SHACL "
                               "engine; this deployment is a simulation written for the demo.",
                "conforms_to": cfg["types"]["report"],
                "keywords": ["SHACL"],
                "was_derived_from": [graph_art["iri"], shapes_art["iri"]],
                "distributions": [distribution(report_path, "text/turtle", "Turtle serialisation")],
            },
            {
                "external_key": key + "/summary",
                "title": "Cohort graph conformance summary",
                "description": "Counts from the same check, for a dashboard that will not parse RDF.",
                "conforms_to": cfg["types"]["summary"],
                "was_derived_from": [graph_art["iri"]],
                "distributions": [distribution(summary_path, "application/json", "JSON summary")],
            },
        ],
    })
    log.say("advertised the report and the summary, derived from the other deployment's graph")
    for iri in result["artifacts"]:
        log.detail(iri)

    # The same advertisement, sent twice. A retried CI step must not double the lineage
    # (spec §7.5), and the only way to show that is to retry one.
    again = reg.post("/api/v1/advertise/consumed", {
        "run": {"external_key": key, "status": "success"},
        "artifacts": [{"iri": graph_art["iri"]}, {"iri": shapes_art["iri"]}],
    })
    log.say("re-sent the consume advertisement: created=%s, same run %s"
            % (again["created"], again["run"].rsplit("/", 1)[-1]))


def phase_revise(reg: Registry, cfg: dict, out: str, log: Log) -> None:
    """Act on what the other deployment reported: fix the graph and advertise the revision."""
    summary_art = newest_of_type(reg, cfg["types"]["summary"], log)
    report_art = newest_of_type(reg, cfg["types"]["report"], log)
    summary = json.loads(fetch_verified(summary_art, log))
    graph_art = reg.get("/api/v1/artifacts/%s" % summary["validated"].rsplit("/", 1)[-1])

    key = "two-credentials/revise"
    reg.post("/api/v1/advertise/consumed", {
        "run": {"external_key": key, "label": "Revise cohort graph", "status": "running",
                "started_at": now()},
        "artifacts": [{"iri": report_art["iri"]}, {"iri": summary_art["iri"]}],
    })
    log.say("consumed the report the other deployment produced (%d violation(s) to fix)"
            % len(summary["violations"]))

    fixed = [(pid, site, True) for pid, site, _ in COHORT]
    revised_path = os.path.join(out, "cohort-v2.ttl")
    write_cohort(revised_path, fixed)

    result = reg.post("/api/v1/advertise/produced", {
        "run": {"external_key": key, "status": "success", "ended_at": now()},
        "artifacts": [{
            "external_key": key + "/graph-v2",
            "title": "Participant cohort graph (revision 2)",
            "description": "The cohort graph with the consent the validation report flagged.",
            "conforms_to": cfg["types"]["graph"],
            "keywords": ["RDF Graphs"],
            "version": "2",
            "license": "https://creativecommons.org/licenses/by/4.0/",
            # Both, and they are not the same claim: the revision *replaces* the graph, and it
            # was *derived from* the report that said what to change. Dropping either leaves a
            # reader unable to answer one of "which is current" or "why did this change".
            "was_revision_of": graph_art["iri"],
            "was_derived_from": [graph_art["iri"], report_art["iri"]],
            "distributions": [distribution(revised_path, "text/turtle", "Turtle serialisation")],
        }],
    })
    log.say("advertised the revision: %s" % result["artifacts"][0])


PHASES = {"export": phase_export, "validate": phase_validate, "revise": phase_revise}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--config", required=True, help="what the operator provisioned for this deployment")
    ap.add_argument("--phase", required=True, choices=sorted(PHASES))
    ap.add_argument("--out", required=True, help="where this deployment writes its own files")
    args = ap.parse_args()

    cfg = tarclient.load_config(args.config)
    os.makedirs(args.out, exist_ok=True)
    log = Log(cfg["tag"])

    reg = open_registry(cfg, log)
    # Asked before announcing as well as after, because the answer changes: a credential
    # authorised through a software's registration clients starts out as "nobody in
    # particular, but allowed to create a deployment of that", and becomes a deployment.
    show_whoami(reg, log)
    if cfg.get("self_registration"):
        announce_self(reg, cfg, log)
        show_whoami(reg, log)
    PHASES[args.phase](reg, cfg, args.out, log)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except tarclient.RegistryError as e:
        print("  ! %s" % e, file=sys.stderr)
        sys.exit(1)
