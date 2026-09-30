# Conventions

Everything in this section is under `/api/v1` on the registry's own origin. The registry
describes itself at `GET /.well-known/tar-registry`, which is the first thing a client should
read: it reports the API base, whether reads are public, which authentication methods are
configured, the SPARQL endpoint, the `llms.txt` location and the peers it federates with.

```bash
curl https://registry.example.org/.well-known/tar-registry
```

`GET /api/v1/registry` is the catalogue's own summary — title, operator, version, record counts,
peer count — and `GET /api/v1/context` is the registry's JSON-LD `@context`.

## Authentication

A credential is a bearer token, whatever kind it is:

```
Authorization: Bearer <token>
```

Reads are anonymous by default (`TAR_PUBLIC_READ`). Writes always need a credential. Which
credential to use, and how a deployment gets one, is [How a tool
authenticates](authentication.md).

`GET /api/v1/whoami` reports what a credential resolved to — the principal, its roles, its
scopes and the deployment it acts as, if any. It is the first thing to call when a job gets a
`403`.

### Roles and scopes

Two orthogonal things. **Roles** come from a person's identity provider token, or from the root
token; **scopes** are carried by registry-minted API tokens and bound the deployment
credentials.

| Role | May |
|---|---|
| `reader` | read |
| `curator` | register and edit software, deployments, releases and vocabulary terms, and withdraw them |
| `admin` | everything, including peers and token administration |

| Scope | Permits |
|---|---|
| `advertise:produce` | advertise artifacts a run produced |
| `advertise:consume` | advertise artifacts a run consumed |
| `register:software` | register and update software, its releases and capabilities — not deployments, and not withdrawing software |
| `register:instance` | register deployments |
| `read:private` | read records that are not publicly readable |
| `subscribe:artifacts` | manage the deployment's own [subscriptions](subscriptions.md), and nothing else — not its tokens, and not its record |
| `admin:*` | everything; minted by an admin, never taken from an identity provider's token |

A token minted for a deployment stays within that deployment's `allowed_scopes` when it has any,
and a deployment's own credential cannot mint a scope it does not hold. `subscribe:artifacts` is
the exception to both: it grants less than any of the deployment's credentials already has.
Scopes in an OIDC token are read only from the registry's own identity provider
(`TAR_OIDC_ISSUER`), and a bound deployment's are capped by its `allowed_scopes`.

## Errors

Every error path returns [RFC 9457] `application/problem+json`:

```json
{
  "type": "https://w3id.org/tar/problem/forbidden",
  "title": "Forbidden",
  "status": 403,
  "detail": "credential lacks the advertise:produce scope (has: advertise:consume)"
}
```

| Status | `type` suffix | When |
|---|---|---|
| `400` | `bad-request` | Malformed request. |
| `401` | `unauthorized` | No credential, or one that did not verify. `WWW-Authenticate: Bearer` is set. |
| `403` | `forbidden` | A valid credential lacking the role or scope. Retrying with different arguments will not help. |
| `404` | `not-found` | No such record. |
| `405` | `method-not-allowed` | The path exists but not with this method. `Allow` lists the ones it takes. |
| `406` | `not-acceptable` | An API route asked for `text/html`. Pages are served at the record's IRI, not under `/api/v1`. |
| `409` | `conflict` | A uniqueness rule was violated. |
| `410` | — | The record was withdrawn. The body is **the record itself**, in the representation asked for, with `tombstoned: true` — not a problem document. A write to it is also `410`, and that one is a problem (`tombstoned`). |
| `413` | `payload-too-large` | The body is over `TAR_MAX_PAYLOAD_BYTES`. |
| `415` | `unsupported-media-type` | A write without `Content-Type: application/json`. |
| `422` | `invalid-body` | The body is JSON but a field has the wrong type or a required one is missing. |
| `422` | `shacl-validation-failed` | The write is well-formed JSON but not a record the registry accepts. |
| `422` | `not-deployable` | An `endpoint_url` on a deployment of software marked `deployable: false`. `field` names it. |
| `422` | `invalid-checksum` | `/artifacts/identify` got a digest it cannot use. `field` and the accepted `algorithms` are in the body. |
| `422` | `bytes-not-accepted` | `/artifacts/identify` was sent the data instead of its digest. |
| `422` | `result-too-large` | A query would return more than 100 000 rows. Add a `LIMIT` and page. |
| `429` | `rate-limited` | Over a rate limit. `Retry-After` says when to try again. |
| `500` | `internal` | A fault on the server. The `detail` gives a reference to quote to the operator, who finds the internals in the log under it. |
| `502` | `upstream-failed` | A peer or repository the registry had to reach did not answer. |
| `502` | `forge-unreachable` | Repository sync could not fetch from the forge. |
| `503` | `query-timeout` | A query ran longer than `TAR_SPARQL_TIMEOUT` and was stopped. |
| `503` | `suggestions-full` | A peer announcement arrived while too many suggestions await review. Try again later. |

### The `422`, specifically

A rejected write carries the SHACL engine's own report alongside the problem document:

```json
{
  "type": "https://w3id.org/tar/problem/shacl-validation-failed",
  "title": "Write rejected by SHACL validation",
  "status": 422,
  "detail": "kind: value must be one of service, library, cli, desktop, workflow",
  "report": "@prefix sh: … a sh:ValidationReport ; sh:result [ … ] .",
  "report_media_type": "text/turtle"
}
```

Each result in the report carries a `tar:jsonField` naming the JSON field that caused it, so a
form can attach the message to the input that produced it without parsing `sh:resultPath` back
into a field name.

The vocabulary rule reports through the *same* report, deliberately. A caller has one error
shape to handle, not two — see [Artifact types and topics](../vocabulary/terms.md#the-refusal).

## Listing, filtering and pagination

List endpoints return:

```json
{ "items": [ … ], "total": 42, "next_cursor": "https://registry.example.org/software/01a05…", "facets": [ … ] }
```

Pagination is **keyset**, not offset. Pass the `next_cursor` you were given back as `?cursor=`
to get the following page; a `null` cursor means the end. The cursor is a record IRI, and the
ordering is descending IRI string — which is newest-first within one registry, because ids are
UUIDv7 and sort by mint time. `?limit=` defaults to 25 and is clamped to 200.

`facets` accompanies the software listing with the value counts a filter UI needs, so a client
does not have to fetch the whole catalogue to know what is worth filtering on. Other listings
omit it.

## Idempotency

The advertisement endpoints are idempotent on `(run, artifact, role)`, keyed by the run's
`external_key`. A retried CI step does not duplicate lineage. See [Advertising runs and
artifacts](advertising.md).

## Request size

Bodies are capped by `TAR_MAX_PAYLOAD_BYTES`, default 2 MiB. Software records carry whole
READMEs, so this is worth raising if you are importing large ones.

## Rate limits

A client over its limit gets `429 Too Many Requests` as `application/problem+json`, with
`Retry-After` in seconds and a `detail` naming the limit that applied. Wait that long and retry;
retrying sooner is refused again.

Every limited response, accepted or not, carries `RateLimit-Policy` (the quota per 60-second
window) and `RateLimit` (`r`, what is left; `t`, seconds until it is fully replenished), in the
form of the IETF `draft-ietf-httpapi-ratelimit-headers`, so a client can pace itself instead of
finding the limit by hitting it.

Authenticated requests are charged to the credential, not the address, and have higher limits;
a batch job should authenticate even for reads. Repeatedly presenting a bad credential gets an
address refused authentication for a while — check the credential rather than retrying it.

## Audit

Every write is recorded. `GET /api/v1/audit` returns the most recent entries, for admins;
`?limit=` defaults to 100 and is clamped to 1000.

[RFC 9457]: https://www.rfc-editor.org/rfc/rfc9457
