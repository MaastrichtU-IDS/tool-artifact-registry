#!/usr/bin/env python3
"""sulo-app — a stand-in for a sulo-schema-builder deployment.

**This is a simulation.** It is not the real sulo-schema-builder and does not talk to it. What
it faithfully reproduces is the part that matters here: a deployment that builds a schema,
emits real files, serves them, and *advertises* what it produced to the registry using nothing
but its own credential and the public HTTP API.

What it does, in order:

1. builds a small schema model in memory (classes, object properties, datatype properties);
2. renders that model to a genuinely parseable OWL 2 ontology in Turtle, and to a SHACL
   shapes graph derived from the same model;
3. serves both, plus a landing page and the schema model itself, over HTTP on a free port —
   so every URL it puts in the registry actually resolves while the demo is up;
4. advertises one Run that generated two Artifacts, with real checksums, real byte sizes,
   real timestamps, and full authorship.

It never learns that anything downstream exists. The only thing it says to the registry is
"I made this, here is how to get it, here is who is responsible for it."
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import sys
import threading
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tarclient import (  # noqa: E402
    Log,
    Registry,
    announce_endpoint,
    load_config,
    serve_directory,
    sha256_file,
    write_marker,
    write_pid,
)

LOG = Log("sulo")

ONT_IRI = "https://ontology.example.org/biobank"
ONT_NS = ONT_IRI + "#"
ONT_VERSION = "1.0.0"

# ORCID's own public example record — a deliberately fictitious researcher. Using a real
# person's ORCID to decorate a demo would be inventing a fact about a real human being; using
# a made-up number would produce an identifier that resolves to nothing. This one is neither.
CREATOR = {
    "name": "Josiah Carberry",
    "kind": "person",
    "identifier": "https://orcid.org/0000-0002-1825-0097",
}
IDS = {
    "name": "Maastricht University — Institute of Data Science",
    "kind": "organization",
    "identifier": "https://ror.org/02jz4aj89",
}


def now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


# ---------------------------------------------------------------- the schema model

# What a schema builder actually holds: a model, not a file. The Turtle and the shapes below
# are both *renderings* of this, which is why they cannot drift apart.
MODEL = {
    "iri": ONT_IRI,
    "title": "Biobank Sample Ontology",
    "version": ONT_VERSION,
    "classes": [
        ("MaterialEntity", None, "Material entity", "Anything with mass that occupies space."),
        ("Process", None, "Process", "Something that unfolds over time."),
        ("InformationEntity", None, "Information entity", "A record about something else."),
        ("Specimen", "MaterialEntity", "Specimen", "Material taken from a donor and kept for study."),
        ("TissueSpecimen", "Specimen", "Tissue specimen", "A specimen of solid tissue."),
        ("TumourTissueSpecimen", "TissueSpecimen", "Tumour tissue specimen", "Tissue taken from a tumour."),
        ("FluidSpecimen", "Specimen", "Fluid specimen", "A specimen of a body fluid."),
        ("BloodSpecimen", "FluidSpecimen", "Blood specimen", "Whole blood drawn from a donor."),
        ("PlasmaSpecimen", "BloodSpecimen", "Plasma specimen", "The plasma fraction separated from whole blood."),
        ("UrineSpecimen", "FluidSpecimen", "Urine specimen", "A urine sample."),
        ("Donor", "MaterialEntity", "Donor", "The person a specimen was taken from."),
        ("StorageContainer", "MaterialEntity", "Storage container", "A vessel a specimen is kept in."),
        ("CryoVial", "StorageContainer", "Cryovial", "A container rated for cryogenic storage."),
        ("CollectionEvent", "Process", "Collection event", "The act of taking a specimen from a donor."),
        ("AliquotingProcess", "Process", "Aliquoting process", "Dividing a specimen into aliquots."),
        ("StorageProcess", "Process", "Storage process", "Placing a specimen into a container."),
        ("ConsentRecord", "InformationEntity", "Consent record", "What the donor agreed to."),
    ],
    "object_properties": [
        ("derivedFromSpecimen", "Specimen", "Specimen", "Derived from specimen", True),
        ("collectedFrom", "Specimen", "Donor", "Collected from", False),
        ("storedIn", "Specimen", "StorageContainer", "Stored in", False),
        ("hasOutput", "CollectionEvent", "Specimen", "Has output", False),
        ("coveredByConsent", "Specimen", "ConsentRecord", "Covered by consent", False),
    ],
    "datatype_properties": [
        ("sampleIdentifier", "Specimen", "xsd:string", "Sample identifier", True),
        ("collectionDate", "CollectionEvent", "xsd:date", "Collection date", True),
        ("storageTemperatureCelsius", "StorageContainer", "xsd:decimal", "Storage temperature (°C)", True),
        ("volumeMillilitres", "FluidSpecimen", "xsd:decimal", "Volume (mL)", True),
        ("aliquotCount", "Specimen", "xsd:nonNegativeInteger", "Aliquot count", True),
    ],
    "disjoint": [("TissueSpecimen", "FluidSpecimen"), ("Donor", "Specimen")],
}


def render_ontology(issued: str) -> str:
    """Render the model as OWL 2 in Turtle.

    This is a real ontology, not a stub string: a versioned `owl:Ontology` header with its own
    licence and creator, a named class hierarchy, typed object and datatype properties, two
    disjointness axioms, an existential restriction and an equivalent-class definition. The
    last two are there on purpose — they are exactly the axioms a real reasoner would use and
    the ingester downstream deliberately does not, and the demo says so rather than pretending
    its cheap closure is OWL reasoning.
    """
    out = [
        "@prefix bs:   <%s> ." % ONT_NS,
        "@prefix owl:  <http://www.w3.org/2002/07/owl#> .",
        "@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .",
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .",
        "@prefix skos: <http://www.w3.org/2004/02/skos/core#> .",
        "@prefix dct:  <http://purl.org/dc/terms/> .",
        "@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .",
        "",
        "<%s> a owl:Ontology ;" % ONT_IRI,
        '    dct:title "%s"@en ;' % MODEL["title"],
        '    dct:description "A small domain ontology for biobank specimens, generated by a '
        'simulated sulo-schema-builder deployment for the tool-artifact-registry demo. The '
        'subject matter is illustrative; the OWL is real."@en ;',
        '    owl:versionInfo "%s" ;' % ONT_VERSION,
        "    dct:license <https://spdx.org/licenses/CC-BY-4.0> ;",
        "    dct:creator <%s> ;" % CREATOR["identifier"],
        "    dct:publisher <%s> ;" % IDS["identifier"],
        '    dct:issued "%s"^^xsd:dateTime .' % issued,
        "",
        "# ---------------------------------------------------------------- classes",
        "",
    ]
    for name, parent, label, definition in MODEL["classes"]:
        lines = ["bs:%s a owl:Class ;" % name]
        if parent:
            lines.append("    rdfs:subClassOf bs:%s ;" % parent)
        lines.append('    rdfs:label "%s"@en ;' % label)
        lines.append('    skos:definition "%s"@en .' % definition)
        out.extend(lines + [""])

    out += [
        "# A specimen has to come from somewhere: plasma is separated from whole blood. An",
        "# existential restriction, which is what makes this OWL rather than a taxonomy.",
        "bs:PlasmaSpecimen rdfs:subClassOf",
        "    [ a owl:Restriction ; owl:onProperty bs:derivedFromSpecimen ;",
        "      owl:someValuesFrom bs:BloodSpecimen ] .",
        "",
        "# A defined class: membership follows from the axioms, it is never asserted.",
        "bs:CryopreservedSpecimen a owl:Class ;",
        '    rdfs:label "Cryopreserved specimen"@en ;',
        '    skos:definition "A specimen stored in a container rated for cryogenic storage."@en ;',
        "    owl:equivalentClass",
        "    [ a owl:Class ; owl:intersectionOf",
        "      ( bs:Specimen",
        "        [ a owl:Restriction ; owl:onProperty bs:storedIn ;",
        "          owl:someValuesFrom bs:CryoVial ] ) ] .",
        "",
    ]
    for a, b in MODEL["disjoint"]:
        out.append("bs:%s owl:disjointWith bs:%s ." % (a, b))
    out += ["", "# ------------------------------------------------------ object properties", ""]
    for name, domain, rng, label, transitive in MODEL["object_properties"]:
        types = "owl:ObjectProperty, owl:TransitiveProperty" if transitive else "owl:ObjectProperty"
        out += [
            "bs:%s a %s ;" % (name, types),
            "    rdfs:domain bs:%s ;" % domain,
            "    rdfs:range bs:%s ;" % rng,
            '    rdfs:label "%s"@en .' % label,
            "",
        ]
    out += ["# ---------------------------------------------------- datatype properties", ""]
    for name, domain, rng, label, functional in MODEL["datatype_properties"]:
        types = "owl:DatatypeProperty, owl:FunctionalProperty" if functional else "owl:DatatypeProperty"
        out += [
            "bs:%s a %s ;" % (name, types),
            "    rdfs:domain bs:%s ;" % domain,
            "    rdfs:range %s ;" % rng,
            '    rdfs:label "%s"@en .' % label,
            "",
        ]
    return "\n".join(out).rstrip() + "\n"


def render_shapes() -> str:
    """The SHACL shapes graph the builder emits beside the ontology, from the same model.

    It exists here for one reason beyond realism: it is advertised in the same Run as the
    ontology, and the subscriber downstream is *not* interested in it. Two artifacts go out,
    one delivery comes back — which is the filter doing its job where it can be seen.
    """
    out = [
        "@prefix sh:   <http://www.w3.org/ns/shacl#> .",
        "@prefix bs:   <%s> ." % ONT_NS,
        "@prefix bsh:  <%s/shapes#> ." % ONT_IRI,
        "@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .",
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .",
        "",
    ]
    by_domain: dict[str, list] = {}
    for name, domain, rng, label, functional in MODEL["datatype_properties"]:
        by_domain.setdefault(domain, []).append((name, rng, label, functional))
    for name, domain, rng, label, transitive in MODEL["object_properties"]:
        by_domain.setdefault(domain, []).append((name, "bs:" + rng, label, False))

    for domain, props in by_domain.items():
        out.append("bsh:%sShape a sh:NodeShape ;" % domain)
        out.append("    sh:targetClass bs:%s ;" % domain)
        for i, (name, rng, label, functional) in enumerate(props):
            kind = "sh:datatype %s" % rng if rng.startswith("xsd:") else "sh:class %s" % rng
            card = " ; sh:maxCount 1" if functional else ""
            end = " ." if i == len(props) - 1 else " ;"
            out.append(
                '    sh:property [ sh:path bs:%s ; %s%s ; sh:name "%s" ]%s' % (name, kind, card, label, end)
            )
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def docs_page() -> str:
    """The page `documentation` points at. Generated from the same model as everything else,
    so it cannot describe an ontology different from the one that is served."""
    rows = "\n".join(
        "<tr><td><code>bs:%s</code></td><td>%s</td><td><code>%s</code></td><td>%s</td></tr>"
        % (name, label, "bs:" + parent if parent else "—", definition)
        for name, parent, label, definition in MODEL["classes"]
    )
    props = "\n".join(
        "<tr><td><code>bs:%s</code></td><td>%s</td><td><code>bs:%s</code></td><td><code>%s</code></td></tr>"
        % (name, label, domain, rng)
        for name, domain, rng, label, _ in MODEL["object_properties"] + MODEL["datatype_properties"]
    )
    return f"""<!doctype html>
<title>{MODEL['title']} — documentation</title>
<style>body{{font:15px/1.6 system-ui,sans-serif;max-width:56rem;margin:3rem auto;padding:0 1rem}}
table{{border-collapse:collapse;width:100%}}td,th{{border-bottom:1px solid #ddd;padding:.35rem .5rem;
text-align:left;vertical-align:top}}code{{background:#f4f4f5;padding:.1rem .3rem;border-radius:3px}}</style>
<h1>{MODEL['title']} <small>v{ONT_VERSION}</small></h1>
<p><a href="/">back</a> · namespace <code>{ONT_NS}</code></p>
<p>Generated documentation, rendered from the same schema model as the OWL and the SHACL shapes.
   The tool that produced it is a simulation written for the tool-artifact-registry demo.</p>
<h2>Classes</h2>
<table><tr><th>Term</th><th>Label</th><th>Superclass</th><th>Definition</th></tr>
{rows}</table>
<h2>Properties</h2>
<table><tr><th>Term</th><th>Label</th><th>Domain</th><th>Range</th></tr>
{props}</table>
"""


def landing_page(files: dict[str, str]) -> str:
    rows = "\n".join(
        '      <li><a href="/%s">%s</a></li>' % (f, f) for f in list(files) + ["docs.html"]
    )
    return f"""<!doctype html>
<title>sulo-app (simulated) — {MODEL['title']}</title>
<style>body{{font:15px/1.6 system-ui,sans-serif;max-width:44rem;margin:3rem auto;padding:0 1rem}}
code{{background:#f4f4f5;padding:.1rem .3rem;border-radius:3px}}</style>
<h1>{MODEL['title']} <small>v{ONT_VERSION}</small></h1>
<p>Served by <strong>sulo-app</strong>, a simulated <code>sulo-schema-builder</code> deployment
   in the tool-artifact-registry two-application demo. It is not the real tool.</p>
<ul>
{rows}
</ul>
<p>Everything above is advertised to the registry as a <code>dcat:Distribution</code> with the
   checksum and byte size of the exact bytes this server returns.</p>
"""


# ---------------------------------------------------------------- the deployment


def main() -> int:
    ap = argparse.ArgumentParser(description="Simulated sulo-schema-builder deployment.")
    ap.add_argument("--config", required=True, help="this deployment's own config (registry URL, instance, token)")
    ap.add_argument("--out", required=True, help="directory to write and serve its output from")
    ap.add_argument("--ready-file", help="written once the ontology is advertised")
    ap.add_argument("--pid-file")
    ap.add_argument("--hold", action="store_true", help="keep serving after advertising, until SIGTERM")
    args = ap.parse_args()

    write_pid(args.pid_file)
    cfg = load_config(args.config)
    reg = Registry(cfg["registry"], cfg["token"])

    me = reg.get("/api/v1/whoami")
    LOG.say("starting up as %s" % (me.get("instance") or "?"))
    LOG.detail("credential %s, scopes %s" % (me.get("credential"), ",".join(me.get("scopes") or [])))
    LOG.detail("the registry takes the Instance from this credential; the payload never names it (§8.3)")

    started = now()

    # ---- 1. build ---------------------------------------------------------
    os.makedirs(args.out, exist_ok=True)
    issued = now()
    ontology = render_ontology(issued)
    shapes = render_shapes()
    files = {
        "biobank.ttl": ontology,
        "biobank-shapes.ttl": shapes,
        "model.json": json.dumps(MODEL, indent=2) + "\n",
    }
    for name, text in files.items():
        with open(os.path.join(args.out, name), "w") as fh:
            fh.write(text)
    with open(os.path.join(args.out, "index.html"), "w") as fh:
        fh.write(landing_page(files))
    with open(os.path.join(args.out, "docs.html"), "w") as fh:
        fh.write(docs_page())
    LOG.say(
        "built %s v%s — %d classes, %d object properties, %d datatype properties"
        % (
            MODEL["title"],
            ONT_VERSION,
            len(MODEL["classes"]) + 1,  # the defined class is rendered separately
            len(MODEL["object_properties"]),
            len(MODEL["datatype_properties"]),
        )
    )

    # A real parse, when a parser happens to be installed. Not a dependency: the point of the
    # demo is the HTTP contract, and a demo that will not run without rdflib is a worse demo.
    try:
        import rdflib  # type: ignore

        g = rdflib.Graph().parse(os.path.join(args.out, "biobank.ttl"), format="turtle")
        LOG.detail("rdflib parsed the ontology: %d triples" % len(g))
    except ImportError:
        LOG.detail("rdflib is not installed here — skipping the local parse check")

    # ---- 2. serve ---------------------------------------------------------
    base_url, httpd = serve_directory(args.out)
    LOG.say("serving its output at %s" % base_url)
    announce_endpoint(reg, cfg["instance_id"], base_url)
    LOG.detail("recorded that endpoint on its own Instance record — a deployment may maintain it")

    ont_path = os.path.join(args.out, "biobank.ttl")
    shapes_path = os.path.join(args.out, "biobank-shapes.ttl")
    ont_sum, ont_size = sha256_file(ont_path), os.path.getsize(ont_path)
    shapes_sum, shapes_size = sha256_file(shapes_path), os.path.getsize(shapes_path)

    # Note what is *absent* from the distributions below: `access_protocol`. Its vocabulary is
    # https | s3 | sparql | oci | ipfs | file, and this server speaks plain http on loopback.
    # Writing "https" would be a lie in a field a client might act on, so the field is left
    # out. See demo/README-two-apps.md — it is the one thing the API could not express here.

    # ---- 3. advertise -----------------------------------------------------
    body = {
        "run": {
            "external_key": "sulo-schema-builder/build/biobank-%s" % ONT_VERSION,
            "label": "generate the Biobank Sample Ontology and its shapes",
            "started_at": started,
            "ended_at": now(),
            "status": "success",
        },
        "artifacts": [
            {
                "title": "Biobank Sample Ontology (OWL, Turtle)",
                "description": (
                    "An OWL 2 ontology for biobank specimens: a named class hierarchy, typed object and "
                    "datatype properties, disjointness, one existential restriction and one defined class. "
                    "Generated by a simulated sulo-schema-builder deployment."
                ),
                "conforms_to": cfg["types"]["owl"],
                "license": "https://spdx.org/licenses/CC-BY-4.0",
                "version": ONT_VERSION,
                "keywords": ["biobank", "specimen", "owl", "ontology", "demo"],
                "issued": issued,
                "language": ["en"],
                "creators": [CREATOR],
                "publisher": IDS,
                "contact": {
                    "name": "sulo-app demo operator",
                    "kind": "organization",
                    "homepage": base_url + "/",
                },
                "landing_page": base_url + "/",
                "documentation": base_url + "/docs.html",
                # Where it came from, when the source is not itself a registered artifact: the
                # builder's own project model. It is served, so the claim is checkable.
                "source": base_url + "/model.json",
                "external_key": "sulo/biobank/%s" % ONT_VERSION,
                "distributions": [
                    {
                        "title": "Turtle serialisation",
                        "access_url": base_url + "/",
                        "download_url": base_url + "/biobank.ttl",
                        "media_type": "text/turtle",
                        "byte_size": ont_size,
                        "checksum": {"algorithm": "sha256", "value": ont_sum},
                        "auth_method": "none",
                        "availability": "public",
                    }
                ],
            },
            {
                "title": "Biobank Sample Ontology — SHACL shapes",
                "description": (
                    "Shapes generated from the same schema model as the ontology, for validating "
                    "instance data against it."
                ),
                "conforms_to": cfg["types"]["shapes"],
                "license": "https://spdx.org/licenses/CC-BY-4.0",
                "version": ONT_VERSION,
                "keywords": ["biobank", "shacl", "shapes", "demo"],
                "issued": issued,
                "creators": [CREATOR],
                "publisher": IDS,
                "distributions": [
                    {
                        "download_url": base_url + "/biobank-shapes.ttl",
                        "media_type": "text/turtle",
                        "byte_size": shapes_size,
                        "checksum": {"algorithm": "sha256", "value": shapes_sum},
                        "auth_method": "none",
                        "availability": "public",
                    }
                ],
            },
        ],
    }
    result = reg.post("/api/v1/advertise/produced", body)
    LOG.say("advertised 1 run, 2 artifacts to %s" % cfg["registry"])
    LOG.detail("run       %s" % result["run"])
    for iri in result["artifacts"]:
        LOG.detail("artifact  %s" % iri)
    LOG.detail("sha256(biobank.ttl) = %s… (%d bytes, computed from the bytes it serves)" % (ont_sum[:16], ont_size))
    LOG.say("done. It has no idea whether anyone is listening — that is the registry's problem.")

    write_marker(
        args.ready_file,
        {"run": result["run"], "artifacts": result["artifacts"], "base_url": base_url},
    )

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
