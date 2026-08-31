# The hosted MCP server

The registry speaks the **Model Context Protocol** on a route of its own web server, so a
coding agent can read and fill in registry data over HTTP with nothing installed locally.

```
POST https://your-registry.example/mcp
```

That is the whole client configuration. There is no `tar` binary to install, no stdio
subprocess, no second deployment. It is one route on the same axum process, at the same origin,
behind the same credentials as the REST API — because the registry already *is* an
authenticated web server, and asking users to install a copy of it to reach it would add an
install step, a version skew and a second set of authorisation rules to keep in step.

```
claude mcp add --transport http tar-registry https://your-registry.example/mcp
```

---

## 1. Protocol revision and transport

| | |
|---|---|
| **Pinned revision** | `2026-07-28` |
| **Transport** | Streamable HTTP (`POST /mcp`) |
| **Worked from** | <https://modelcontextprotocol.io/specification/2026-07-28/> — `basic/transports/streamable-http`, `basic/versioning`, `server/discover`, `server/tools`, `basic/authorization`, `basic/authorization/authorization-server-discovery` |
| **Also served** | `2025-11-25`, `2025-06-18`, `2025-03-26` via the legacy `initialize` handshake |

`2026-07-28` is a substantial change from the revisions before it, and this server implements
the current shape rather than the remembered one:

* **No protocol-level session.** No `Mcp-Session-Id` is minted or echoed; one is ignored if
  sent. Every request stands alone, carrying its own credential and its own protocol version.
* **No standalone GET stream, no DELETE teardown.** `GET /mcp` and `DELETE /mcp` answer
  `405 Method Not Allowed`, which is what the spec asks a modern-only endpoint to do.
* **`server/discover`** replaces the `initialize` handshake: it reports supported versions,
  capabilities and server identity in one unauthenticated request.
* **Request metadata is mirrored into HTTP headers** — `MCP-Protocol-Version`, `Mcp-Method`,
  `Mcp-Name` — so intermediaries can route without parsing bodies, and the server **validates
  the mirror against the body**. A divergence is refused with `400` and JSON-RPC `-32020`
  (`HeaderMismatch`); that is the point of the mechanism, because a gateway acting on a header
  while the server acts on a different body is a real vulnerability.
* **Caching metadata.** `tools/list` returns `ttlMs` and `cacheScope: "private"` — private,
  not public, because the tool set is filtered by the caller's authority and a shared cache
  must never serve one credential's list to another. `server/discover` is `public`.
* **Version negotiation without a handshake.** An unknown `MCP-Protocol-Version` gets `400`
  with `-32022` `UnsupportedProtocolVersionError` and the `supported` list, so a client can
  retry rather than give up.

### Why also serve the legacy era

`basic/versioning` describes a dual-era server, and being one costs about fifty lines here
because there is no session to keep: the legacy `initialize` is a handshake in name only, and
every subsequent request is authenticated on its own exactly as a modern one is. It is what
makes the endpoint usable by the clients that exist rather than only the clients the spec
describes. A request carrying modern metadata is served per this revision; an `initialize`
request selects legacy semantics and gets the version it asked for, if we speak it.

### Why hand-written JSON-RPC rather than an SDK

`rmcp` 3.1.4 is the maintained Rust SDK, and it would have been a reasonable choice. It was not
taken because the framing is about a hundred lines of `serde`, while the parts that are actually
hard here — the header/body mirror validation, dual-era dispatch, and filtering the tool list by
an axum-resolved `Principal` — are precisely the parts an SDK's own transport and service model
wants to own. Adopting it would have meant running that model beside axum's and re-deriving the
registry's credential handling inside it, to gain code that would still need auditing against
this revision. The framing lives in `src/mcp/rpc.rs` and is unit-tested.

---

## 2. How a client discovers and authenticates

The registry is an OAuth 2.1 **protected resource**. It does not become an authorization
server; it says where one is, in the two places the spec requires.

```
Client                        Registry (/mcp)                 Keycloak
  |  POST /mcp tools/list  ------>  |
  |  <-- 401 WWW-Authenticate: Bearer resource_metadata="…"
  |  GET /.well-known/oauth-protected-resource  -->  |
  |  <-- { "resource": "...", "authorization_servers": ["…/realms/tar"] }
  |  GET …/.well-known/openid-configuration  -------------------->  |
  |  <-- authorization server metadata  --------------------------  |
  |  … OAuth 2.1 + PKCE + RFC 8707 `resource` …  ----------------->  |
  |  <-- access token  -------------------------------------------  |
  |  POST /mcp tools/list  (Authorization: Bearer …)  -->  |
```

**RFC 9728 Protected Resource Metadata** is served at both locations a client must probe:

* `/.well-known/oauth-protected-resource/mcp` — path-inserted, naming `{base}/mcp`
* `/.well-known/oauth-protected-resource` — root, naming `{base}`

Each names `authorization_servers` from `TAR_OIDC_ISSUER` and `TAR_WORKLOAD_ISSUERS`, plus
`bearer_methods_supported: ["header"]`. `resource_documentation` is deliberately omitted: the
registry serves its SPA on any unrouted path, so any URL named there would resolve to the app
shell rather than to documentation. Both are unauthenticated: they are what a client reads
*before* it has a credential.

**The `WWW-Authenticate` challenge** on every 401 points at the **root** document. That is
deliberate. RFC 9728 requires each document's `resource` to match the URL it was fetched for,
so the two documents necessarily name different identifiers — but `src/auth/jwt.rs` validates
`aud` against a single configured audience defaulting to the base IRI. Pointing the challenge
at the root document means a client sends `resource={base}`, the authorization server mints
`aud={base}`, and the token verifies. An operator whose authorization server honours RFC 8707
per-path resources should set `TAR_OIDC_AUDIENCE` to `{base}/mcp` instead.

**The challenge carries no `scope` parameter**, and `scopes_supported` is omitted from the
metadata unless `TAR_MCP_SCOPES` is set. Read tools need authentication and nothing more, and
per the scope-selection strategy a client with neither falls back to omitting the parameter —
which is the behaviour that actually works against a stock Keycloak realm, where the registry's
roles arrive in the token without any custom scope having to be requested. Advertising scope
names the authorization server has never heard of turns a working sign-in into
`invalid_scope`. Operators who *have* modelled `register:software` and friends as OAuth scopes
set `TAR_MCP_SCOPES` and get least privilege.

### The credentials that work

Whatever the REST API accepts, this accepts, because it is the same `crate::auth::authenticate`:

* a **Keycloak/OIDC JWT for a person**, carrying `reader` / `curator` / `admin` roles;
* an **OIDC workload token** whose client id an Instance declares — the credential a deployment
  uses to advertise its own runs;
* an opaque **`tar_…` registry token** minted per Instance, for deployments with no identity
  provider. Simplest for a quick trial:

```
claude mcp add --transport http tar-registry https://your-registry.example/mcp \
  -H "Authorization: Bearer tar_…"
```

---

## 3. The tools

Seventeen, not a mirror of forty REST routes. Each entry costs context on every request, and
near-duplicates make a model choose badly.

### Orientation

| Tool | What it is for |
|---|---|
| `registry_info` | What this registry holds, and — from your credential — exactly what you may write. **Call it first.** |

### Vocabulary — deliberately first-class

| Tool | What it is for |
|---|---|
| `vocab_search` | Search the controlled vocabulary. `branch=topic` (EuroSciVoc: what software is *about*), `branch=data` (EDAM: what an artifact *is*), or everything including locally minted types. |
| `vocab_resolve` | Check that IRIs you were handed are real, and get their labels. |
| `list_enumerations` | Every closed value set the registry validates against — kinds, maturity, availability, access protocols, auth methods, run statuses, scopes. |
| `register_artifact_type` | Mint a local type when the vocabulary genuinely has no term. The honest alternative to a fabricated IRI. |

### Reading

| Tool | What it is for |
|---|---|
| `search_registry` | Free-text across software, instances, artifacts and runs; optionally federated. |
| `list_records` | Filtered, paginated listing of one record kind. |
| `get_record` | One record in full. |
| `find_capable_software` | Matchmaking: what can produce or consume this artifact type — answerable before any run exists. |
| `get_artifact_lineage` | Walk provenance up, down or both. |

### Writing

| Tool | Authority required |
|---|---|
| `register_software` | curator role, or `register:software` |
| `update_software` | curator role, or `register:software` |
| `add_release` | curator role, or `register:software` |
| `declare_capability` | curator role, or `register:software` |
| `register_artifact_type` | curator role, or `register:software` |
| `register_instance` | curator role, or `register:instance` |
| `advertise_produced` | a credential that **acts as an Instance**, plus `advertise:produce` |
| `advertise_consumed` | a credential that **acts as an Instance**, plus `advertise:consume` |

`tools/list` returns only the tools the caller can actually use — the spec explicitly permits
the set to vary by the authorization presented, and it should: a model shown a tool it cannot
use will call it, read a refusal and try again. A person's curator token never sees the
advertisement tools (a person is not a deployment); an instance token scoped to
`advertise:produce` sees the read tools and `advertise_produced`, and nothing else.

### What is deliberately *not* a tool

Minting or revoking API tokens; deleting or tombstoning records; adding, removing or announcing
to peers; managing subscriptions; raw SPARQL; OpenLineage ingestion. Credential issuance and
deletion are person-operations and stay in the UI. Raw SPARQL against a network-reachable
endpoint driven by a model is an exfiltration and cost surface with no matching benefit — the
typed read tools cover what an agent needs. A test asserts the catalogue contains no tool whose
name matches any of these, so adding one later trips it.

---

## 4. Stopping an agent inventing metadata

A model asked to fill in a form will fill it in. A guessed EDAM IRI or a confident "MIT licence"
for a repository that states none produces a record that *looks* right and is wrong — strictly
worse than an empty one, because the UI renders an absent licence honestly as "licence not
stated" and there is no rendering for "invented".

Three measures, in increasing order of how much they can be relied on.

### 4.1 The descriptions say so

Every write tool's description ends with the same contract, and every vocabulary-valued
*parameter* repeats it in its own `description`, because a model reads the parameter it is
filling:

> **DO NOT INVENT VALUES.** Ontology IRIs (topics, artifact types) MUST come from `vocab_search`
> or `register_artifact_type`; this server refuses an IRI from a vocabulary it bundles that it
> cannot resolve, so guessing fails loudly rather than quietly. Closed value sets MUST come from
> `list_enumerations`. Omit any field you cannot confirm from the repository, the package
> metadata or the user — the registry renders an absent field honestly ("licence not stated"),
> while a plausible wrong value is undetectable.

### 4.2 Looking up is cheaper than recalling

`vocab_search` and `list_enumerations` exist so that the correct behaviour is also the easy one,
and a search returning nothing says what to do next — omit the field, or mint a local type —
rather than leaving the model to fill the silence.

### 4.3 The server checks — this is the one that holds

Prose is a suggestion. Before any write, `guard_vocabulary` extracts every ontology IRI from the
arguments at any depth and asks two questions of the registry's own vocabulary graph.

**Does it exist?** An IRI in a vocabulary the registry bundles (EDAM, EuroSciVoc) or minted
itself (`{base}/type/…`) that resolves to nothing is **fatal**, because the registry is
authoritative about the contents of those. Any other unresolved IRI is only a **warning**
attached to the successful result: a foreign type IRI belonging to another registry is
legitimate by design (spec D11 — an ArtifactType is any IRI), and refusing it would break
federation to prevent a mistake it cannot make.

**Is it the right kind of thing?** This half was found by pointing a real coding agent at this
server and telling it to guess. It produced `http://edamontology.org/topic_3170`, which *does*
exist — it is EDAM's "RNA-Seq" — and an existence check alone waved it onto a record that had
nothing to do with RNA-Seq. But software topics come from EuroSciVoc here, and `build.rs` marks
EDAM's topic branch `topic-edam` precisely so the picker never offers it. So the rule is not
"does this term exist" but **"could `vocab_search` have returned this term for this field"**,
which rejects a real term in the wrong branch as firmly as an invented one:

```
Refused before writing anything — 1 vocabulary problem(s) in these arguments:
- http://edamontology.org/topic_3170 is an EDAM topic. EDAM's topic branch is bundled only so
  that older records citing one still render a label; software is classified with EuroSciVoc
  here, and `vocab_search` with branch=topic never returns an EDAM topic. Search again with
  branch=topic and use what it gives you.
```

Nothing is written when this fires.

### 4.4 The SHACL correction loop

A write the shapes reject comes back `422` with an RFC 9457 problem document whose `detail` is
already `field: message`, built from `tar:jsonField` on each validation result. That is
surfaced verbatim with instructions:

> The registry refused this write: it does not satisfy the registry's SHACL shapes.
> Offending fields: `kind: value must be one of service, library, cli, desktop, workflow`
> Fix exactly the named field(s) and retry. **If you cannot establish the true value of a field,
> remove it from the request rather than substituting a plausible one** — the registry renders
> an absent field honestly.

It closes: the model retries with the one named field changed rather than re-guessing the
record. A test walks the loop end to end.

A `403` gets the opposite instruction — *do not retry with different arguments; this is an
authorisation limit* — because a model that reads "refused" and starts varying its input is
the failure mode there.

---

### Listings are summaries

`list_records` returns a projection — the fields you would choose a record on — not the records
themselves. It used to return the REST body verbatim, and a software record carries its whole
README: four of them came to 112 KB and overran a client's tool-output cap, so browsing a
four-record catalogue failed outright. `get_record` returns the complete record once the caller
has chosen one.

## 5. Safety

**MCP follows the registry's own read policy — no looser, no tighter.** With `TAR_PUBLIC_READ`
on (the default), an unauthenticated caller gets the read-only tools and may call them: they
reach the same records anyone can already fetch over REST and query over SPARQL, so refusing
them here bought no secrecy. It cost something real, though — it forced every client into an
OAuth flow it did not need, and a misconfigured identity provider then turned "read the
catalogue" into "cannot connect at all".

With `TAR_PUBLIC_READ` off, an unauthenticated caller gets the protocol handshake only — server
name, version, capabilities, the same thing `/healthz` already reveals — and a 401 with the
discovery challenge on everything else. Not the tool list, not a count, not a record. The
handshake stays open because that is what lets a client reach the challenge and start the OAuth
flow rather than failing at connection time.

Either way the tool list is filtered by the caller's authority and every call is executed by the
REST handler with the caller's own credential, so "anonymous" means "can see what anonymous can
see", never "can do more". A credential that is *offered and rejected* is always a 401, whatever
the read policy: the caller meant to authenticate and needs to know it failed.

**A tool call can never do more than the credential could over REST — structurally.** Every
tool executes as an ordinary HTTP request dispatched through `crate::api::router` in-process,
carrying the caller's own `Authorization` header verbatim. The REST handler runs its own
`require_curator()` / `require_scope()` / `require_instance()`, its own SHACL validation and its
own audit write. There is no second authorisation path to keep in step, and no tool can reach an
operation the credential could not. A test proves the two paths refuse the same call.

**Authorisation failures are tool errors, not transport errors.** A missing role or scope comes
back as `isError: true` with a sentence naming what is missing, rather than a 403 that kills the
connection — a model can act on the first and not the second. Only *authentication* failure is a
transport 401, because that is what drives OAuth discovery.

**Operations reserved for a person.** Token minting and revocation, deletion and tombstoning,
peer management, subscriptions and raw SPARQL are not exposed at all. Not gated — absent.

**Read-only mode.** `TAR_MCP_READ_ONLY=1` hides every write tool from `tools/list` and refuses
it if called anyway. `TAR_MCP_ENABLED=0` removes the endpoint entirely (404).

**Origin validation.** The transport spec requires it against DNS rebinding, so an `Origin`
header that is neither the registry's own nor in `TAR_MCP_ALLOWED_ORIGINS` gets `403`. Absent
`Origin` — every non-browser client — passes.

**Header/body divergence** is refused with `-32020`, so a gateway routing on `Mcp-Name` cannot
disagree with what the server executes.

**Body size** is bounded by the router's existing `TAR_MAX_PAYLOAD_BYTES` limit; list tools clamp
their own page sizes.

---

## 6. Configuration

| Variable | Default | Meaning |
|---|---|---|
| `TAR_MCP_ENABLED` | `true` | Serve `/mcp` at all. |
| `TAR_MCP_READ_ONLY` | `false` | Hide and refuse every write tool. |
| `TAR_MCP_ALLOWED_ORIGINS` | *(base IRI only)* | Comma-separated extra `Origin` values to accept. |
| `TAR_MCP_SCOPES` | *(unset)* | `scopes_supported` for the metadata document. Set only if your authorization server actually knows these scopes. |

Everything else — issuer, audience, roles, scopes, tokens — is the registry's existing
configuration, unchanged.

---

## 7. Verification

`tests/mcp.rs` covers the protocol (handshake, mirrored-header validation, version negotiation,
notifications, the 405s, the legacy fallback), the discovery documents and challenge, the tool
surface and its authority filtering, the vocabulary guard in both halves, the SHACL correction
loop, and REST/MCP authorisation parity. `src/mcp/*.rs` carry unit tests for framing, header
decoding and gate logic.

Verified against **Claude Code 2.1.251** as a real MCP client: it negotiates `2026-07-28` via
`server/discover`, lists the tools its credential allows, and calls them. A raw wire transcript,
should you want one:

```
$ curl -s -X POST http://127.0.0.1:8100/mcp \
    -H 'content-type: application/json' \
    -H 'mcp-protocol-version: 2026-07-28' -H 'mcp-method: server/discover' \
    -d '{"jsonrpc":"2.0","id":"d1","method":"server/discover","params":{}}'
{"id":"d1","jsonrpc":"2.0","result":{
  "resultType":"complete",
  "supportedVersions":["2026-07-28","2025-11-25","2025-06-18","2025-03-26"],
  "capabilities":{"tools":{}},
  "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"tool-artifact-registry","version":"0.1.0"}},
  "ttlMs":3600000,"cacheScope":"public"}}

$ curl -s -X POST … -H 'mcp-method: tools/list' -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
HTTP/1.1 401 Unauthorized
www-authenticate: Bearer realm="tool-artifact-registry",
  resource_metadata="http://127.0.0.1:8100/.well-known/oauth-protected-resource",
  error="invalid_token", error_description="an MCP request needs a bearer token: …"

$ curl -s -X POST … -H 'mcp-method: tools/call' -H 'mcp-name: vocab_search' \
    -d '{"jsonrpc":"2.0","id":3,"method":"tools/call",
         "params":{"name":"register_software","arguments":{"name":"x"}}}'
{"id":3,"jsonrpc":"2.0","error":{"code":-32020,
  "message":"Mcp-Name header \"vocab_search\" does not match the body value \"register_software\""}}
```
