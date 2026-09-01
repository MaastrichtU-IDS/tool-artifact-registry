# Advertising runs and artifacts

This is what a tool does at the end of a job: say what it ran and what came out of it.

```
POST /api/v1/advertise/produced     artifacts this run generated
POST /api/v1/advertise/consumed     artifacts this run used
```

Both take the same body — one run, and the artifacts it touched — and both create the run if it
does not exist yet, so a job that produces and consumes makes two calls and gets one run.

The credential must act as a deployment and carry `advertise:produce` or `advertise:consume`.
The deployment comes from the credential; there is no field for it. See [How a tool
authenticates](authentication.md).

```bash
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{
       "run": {"external_key": "ci/12345/attempt-1", "status": "success",
               "started_at": "2026-08-30T14:02:11Z", "ended_at": "2026-08-30T14:02:49Z"},
       "artifacts": [{
         "title": "Validation report — input.ttl against the shapes at v3",
         "conforms_to": "https://registry.example.org/type/shacl-validation-report",
         "license": "https://spdx.org/licenses/CC-BY-4.0",
         "keywords": ["SHACL"],
         "was_derived_from": ["https://peer-registry.example.net/artifact/01J7Z…"],
         "distributions": [{
           "download_url": "https://validator.example.org/reports/9f2a.ttl",
           "media_type": "text/turtle",
           "byte_size": 2118342,
           "checksum": {"algorithm": "sha256", "value": "9f2a…"},
           "access_protocol": "https",
           "auth_method": "apikey",
           "availability": "restricted",
           "access_request_url": "https://example.org/data-access"
         }]
       }]
     }' \
     https://registry.example.org/api/v1/advertise/produced
```

The response names what now exists:

```json
{
  "run": "https://registry.example.org/run/01a05…",
  "artifacts": ["https://registry.example.org/artifact/01a05…"],
  "created": true,
  "queued_for_resolution": ["https://peer-registry.example.net/artifact/01J7Z…"]
}
```

`created` is `false` when every artifact in the payload was already recorded for this run and
role — which is the signal that your retry was a retry.

## Idempotency

Both endpoints are idempotent on `(run, artifact, role)`.

The run is identified by its `external_key` — a CI job id, a workflow attempt, whatever your
system already calls it — scoped to the advertising deployment. Post the same key twice and you
update one run rather than creating a second, and the artifacts are matched against what that
run already has.

This matters because retries are normal. A CI step that reruns, a webhook that redelivers, a
job that is restarted after an infrastructure failure: none of them should double the lineage
graph. If you send no `external_key`, every call mints a new run, which is almost never what
you want from an automated caller.

An artifact carries its own `external_key` too; when it has none, one is derived from the run's,
so the pairing still holds.

## Advertise late, not early

Advertise when you know what happened. A run posted as `running` and never updated is a run the
registry has to keep believing in. If you do want progress visible, post with `status:
"running"` and post again with the outcome — the second call finds the same run by its
`external_key` and updates it.

## Foreign inputs

`was_derived_from`, `was_revision_of` and `is_version_of` may point at an IRI at another
registry. Advertising never blocks on the network: an unknown IRI is stored verbatim, and a
background worker fetches a stub into that peer's own graph afterwards. The
`queued_for_resolution` list in the response tells you which ones that happened for.

This is what makes cross-registry lineage cheap enough to actually do. See
[Federation](federation.md).

## Types are checked, keywords are not

`conforms_to` must be a term the registry holds, and a write naming one it cannot resolve is a
`422` before anything is written. Look it up first with `GET
/api/v1/vocab/search?branch=data&q=…`. See [Artifact types and topics](../vocabulary/terms.md).

`keywords` are matched against the registry's list where they can be and kept verbatim where
they cannot — see [Artifact keywords](../vocabulary/keywords.md).

## Say "no bytes" out loud

If the artifact's bytes are not obtainable from here, do not omit the URL and leave it ambiguous
— set `availability: "metadata-only"` on the distribution, or give no distribution at all. That
makes the record's Signposting headers omit `rel="item"`, so a client can tell a policy from an
oversight. See [Availability](../model.md#availability-and-the-honest-absence).

## Content identifiers

A distribution that carries a `checksum` also gets a **content identifier**: an [RFC 6920]
`ni:///` URI derived from the digest, which is the same string for the same bytes no matter
which registry, deployment or run produced them.

That makes `GET /api/v1/artifacts?content=ni:///sha-256;…` a question about bytes rather than
about records — *has anyone here described this exact file?* — which is the question you have
when two pipelines may have produced the same output by different routes.

The identifier is a **pure function** of the algorithm and the digest: the digest in base64url
without padding, after `ni:///<algorithm>;`. Nothing about the registry goes into it, so compute
it wherever the file already is:

```bash
printf 'ni:///sha-256;%s\n' \
  "$(openssl dgst -binary -sha256 FILE | openssl base64 -A | tr '+/' '-_' | tr -d '=')"
```

`POST /api/v1/artifacts/identify` (or `GET` with the same two parameters) returns the identifier
for an `algorithm` and `value` you send. It is a convenience for checking your own
implementation, not a source of truth, and its response says so along with the code to stop
calling it.

**It will not accept the file.** Sending bytes is a `422`, deliberately: streaming a file to the
registry so it can compute a digest you can compute locally would put a network round trip, a
size limit and the registry's availability between a producer and an identifier that does not
depend on the registry at all.

## Registering an artifact with no run

`POST /api/v1/artifacts` records an artifact that no run in this registry produced — data that
predates the registry, or arrived from outside it. Same body shape as one element of
`artifacts`, same vocabulary rule.

## OpenLineage

Airflow, dbt, Spark and anything else that already emits [OpenLineage] can post its native
events instead:

```
POST /api/v1/openlineage
```

The adapter maps what OpenLineage covers onto runs, artifacts and lineage, and keeps the whole
event as `tar:openLineagePayload` so that nothing it does not model is lost. That is the honest
version of an adapter: it does not pretend the mapping is total, and it does not throw away
what it could not place.

The artifact type the adapter assigns is a term like any other, held to the same rule.

## Reading it back

```
GET /api/v1/runs                        ?q= ?instance= ?software= ?status=
GET /api/v1/runs/{id}                   one run, with what it used and generated
GET /api/v1/artifacts                   ?q= ?conforms_to= ?license= ?availability= ?keyword=
                                        ?instance= ?software= ?run= ?content= ?registry=
GET /api/v1/artifacts/{id}
GET /api/v1/artifacts/{id}/lineage      ?direction=up|down|both (default both) ?depth= (1–6, default 1)
GET /api/v1/instances/{id}/runs         one deployment's runs
GET /api/v1/instances/{id}/artifacts    the artifacts its runs touched
```

[OpenLineage]: https://openlineage.io/
[RFC 6920]: https://www.rfc-editor.org/rfc/rfc6920
