# Artifact Subscriptions — Design Note

| | |
|---|---|
| **Status** | Implemented |
| **Date** | 2026-08-31 |
| **Spec** | [`2026-08-30-tool-artifact-registry-design.md`](2026-08-30-tool-artifact-registry-design.md) — extends §7.5, §8.3, §9.3 |
| **Code** | `src/api/subscriptions.rs`, `src/ops/subscriptions.rs`, `migrations/0003_subscriptions.sql`, `frontend/src/routes/Subscriptions.tsx`, `tests/subscriptions.rs` |

---

## 1. Why

D6 says capability and lineage are both first-class, and gives the reason: capability answers
*"what can produce a SHACL report?"* before anything has run, and the run graph answers *"where
did this come from?"* afterwards. There is a third tense neither covers — **"what just appeared
that I care about?"** — and today the only way to ask it is to poll `/api/v1/artifacts` on a
timer and diff the result. Every downstream tool that wants to react to an artifact has to
build that loop itself, badly, and each one adds a fixed query load to the registry whether or
not anything happened.

A subscription is that third tense made first-class: an Instance registers a standing filter,
and the registry tells it when something matches.

---

## 2. The filter model

```jsonc
{
  "conforms_to":  ["http://edamontology.org/data_2048"],  // artifact type
  "software":     ["https://reg/software/01a…"],          // who made it, class level
  "instance":     ["https://reg/instance/01b…"],          // who made it, deployment level
  "keywords":     ["fhir", "cohort-b"],
  "license":      ["https://spdx.org/licenses/CC-BY-4.0"],
  "availability": ["public", "restricted"],
  "q":            "patients.ttl",                         // title or description contains
  "roles":        ["produced"],                           // or "consumed"
  "exclude_own":  true
}
```

**Semantics: OR within a field, AND across fields.** An empty field is "don't care". So the
example above reads *"a validation report, from any deployment of shacl-manager, tagged fhir,
CC-BY, that I can actually retrieve"*. This is the only combination rule that stays predictable
as fields are added: values within one axis are alternatives, and different axes are
independent constraints.

**An artifact that lacks the field never matches a constraint on it.** An artifact with no
stated licence does not match `license: [CC-BY]`. Absent is not permissive, and a subscription
that quietly assumed otherwise would be worse than no subscription.

### Why these fields

A filter set nobody can express their real interest in is decoration. Each of these answers a
question someone actually has:

| Field | The question | Why it earns its place |
|---|---|---|
| `conforms_to` | "tell me when a SHACL report appears" | The reason the feature exists — the capability question in event form. Everything else narrows it. |
| `availability` | "…that I can actually fetch" | §6.2 says `metadata-only` is the *common* case, not an edge case. Without this filter, most notifications on a health-data registry are unactionable by construction, and the subscriber learns to ignore them. |
| `instance` / `software` | "…from someone I trust" | The trust axis. `instance` names one deployment; `software` generalises to "any deployment of this tool, anywhere", which is exactly the join key D5 says exists across registries. Resolved through the denormalised `tar:instanceOf` so it stays a single lookup. |
| `keywords` | "…for cohort B" | The project axis. No ontology covers a study name, a cohort id, or a sprint; `dcat:keyword` is where that already lives. |
| `q` | "…mentioning patients.ttl" | Filenames and dataset names live in titles, not in vocabularies. Cheap, and it is the escape hatch for everything the structured fields do not model. |
| `license` | "…that I am permitted to ingest" | Weakest of the set on its own, but it is the one field a data-governance rule is actually written against, and it costs one comparison. |
| `roles` | "made, or merely used?" | A consume advertisement also makes an artifact appear. They are different events, so the default is `produced` and `consumed` is opt-in. |
| `exclude_own` | — | Default on. A tool that both produces and subscribes would otherwise wake itself on every run, and its own output is the one thing it certainly knows about. |

**Deliberately not included:** a run-status filter (a produced artifact from a failed run is
already rare and arguably still interesting), byte-size and media-type thresholds (properties of
a *distribution*, not of the artifact, and a subscriber that cares can look after being told),
and a SHACL-shape filter (spec Q5 defers capability-as-shape; a subscription filter should not
get there first).

### Matching is a function, not a query

`ops::subscriptions::matches(&Filter, owner_instance, &Candidate) -> bool` is pure: no
database, no graph, no network. Every rule in the table above has a unit test in that file. Two
consequences follow:

- **It is testable in isolation.** "Does `availability: [public]` match a metadata-only
  artifact?" is a two-line test, not an integration fixture.
- **The advertisement path stays cheap.** The candidate is built once per artifact and tested
  against each subscription in memory, instead of running one SPARQL query per subscription.

The candidate is read back **from the graph after the write commits**, not from the request
body. So the matcher sees exactly what a reader of that artifact would see — including fields
set by an earlier advertisement that this one did not mention. A foreign IRI that has not been
resolved yet simply has few fields, and a filter on a field it lacks correctly does not match;
that is honest rather than optimistic.

---

## 3. Delivery: two channels, one queue

A match inserts one row into `subscription_deliveries`. That row is the notification. Both
channels drain the same rows, so the two can never disagree about what matched.

```
advertise ──► match ──► INSERT delivery row ──┬──► worker POSTs it        (webhook)
             (pure)     (SQLite, no socket)   └──► subscriber GETs it     (pull)
```

### Pull is the default, not the fallback

The registry already models CLI, desktop and batch tools — `deployable = false` software, an
Instance with no `dcat:endpointURL`, "no endpoint — CLI/batch" as a normal state in the UI
(handoff §5.3). A webhook-only design would exclude exactly those deployments, which is most of
them. So:

```
GET  /api/v1/subscriptions/{id}/deliveries?cursor=&limit=&ack=
POST /api/v1/subscriptions/{id}/deliveries/ack   {"cursor": 42}
```

- The cursor is the delivery row's `seq` — `INTEGER PRIMARY KEY AUTOINCREMENT`, so a deleted row
  can never let a later row reuse a number a client has already passed.
- **Omitting `cursor` resumes from the subscription's own acknowledged position**, so a
  subscriber that keeps no state still makes progress.
- Acknowledgement is **monotonic**: a stale ack never rewinds and replays work already done.
- Nothing is acknowledged by being read. The guarantee is at-least-once, which is the only
  honest one when the subscriber may crash between reading and acting.
- `remaining` in the response says how much is left, so a subscriber decides whether to keep
  going without a second round trip.

A subscription with no `webhook_url` is an ordinary pull subscription, and that is what the UI
creates unless you type a URL. **A subscription works for a tool behind a firewall by default.**

### Webhook

`POST` of the frozen payload, with:

```
x-tar-delivery:     <uuid>          idempotency key for the receiver
x-tar-subscription: <id>
x-tar-timestamp:    <unix seconds>
x-tar-attempt:      <n>
x-tar-signature:    sha256=<hex HMAC-SHA256(secret, "<timestamp>.<body>")>
```

The secret is generated at creation and **shown exactly once**, like a token. Unlike a token it
is stored recoverably, because HMAC needs the key itself rather than a hash — the one place
this feature departs from the `api_tokens` pattern, and the reason is stated in the migration
next to the column. Signing over `timestamp.body` rather than `body` alone lets a receiver
reject a replayed capture by age.

The body is the artifact record that `GET /api/v1/artifacts/{id}` already returns to anonymous
callers, plus the run, instance and software IRIs. **A webhook never carries anything the
receiver could not already have read.**

---

## 4. Failure

> Advertisement must not block on the network (§9.3).

The advertise handler calls `notify_advertised`, which reads the local graph and inserts rows.
No HTTP client is constructed on that path. Everything with a timeout attached happens on the
worker task spawned in `serve()`, next to the peer resolver, for the same reason.

When a delivery fails:

| | |
|---|---|
| **Backoff** | `30s × 2^(attempts-1)`, capped at 6h. Same shape as the federation resolver's. |
| **Give up on the delivery** | After 8 attempts it becomes `dead`. It is never retried, and it stays visible — it is not swept away. |
| **Give up on the endpoint** | After 12 consecutive failed attempts the *subscription* is `suspended`: the worker stops selecting it entirely. This is what stops the registry hammering a host that is gone. |
| **Keep serving** | A suspended subscription keeps matching and keeps queueing. The pull path keeps working. Being unable to push is not being unable to notify. |
| **Make it visible** | `delivery_state`, `consecutive_failures`, `last_error`, `last_error_at`, and per-delivery `status`/`attempts`/`next_attempt_at`/`last_error` are all on the API and all rendered on the management screen. The owner sees *why*, not just *that*. |
| **Recover** | `PATCH {"resume": true}` un-suspends and re-arms the deliveries that died while the endpoint was down — losing them would punish the person who fixed the problem. |

`reqwest`'s error chain is translated before storage, because "could not connect" and "did not
answer within the timeout" and "your receiver said 500" need different fixes.

---

## 5. Abuse

A webhook makes the registry issue outbound HTTP to an address someone else chose. That is a
capability, and it is the security-relevant part of this feature.

| Case | What is done |
|---|---|
| **SSRF into the registry's own network** — `169.254.169.254`, RFC1918, loopback | Refused at registration (literal IPs, `localhost`, `.local`, `.internal`) **and again before every attempt**, on the resolved A/AAAA records. IPv4-mapped IPv6 (`::ffff:127.0.0.1`) is judged on the embedded address, carrier-grade NAT and the reserved ranges included. |
| **Redirect laundering** — a public URL that 302s to the metadata endpoint | The webhook client follows **no** redirects. A redirect is a delivery failure. |
| **Credential leakage in the URL** | `user:pw@host` is refused; the URL is displayed in the UI. |
| **Plaintext** | `https` only. `http` needs `TAR_SUBSCRIPTION_ALLOW_HTTP=true`, for a registry and its subscribers inside one trusted network. |
| **Traffic amplification / DDoS by proxy** | 32 subscriptions per Instance, a bounded batch per tick, one POST per unique `(subscription, artifact, role)`, and backoff-then-suspend means a victim host sees a *decreasing* rate, not an increasing one. |
| **The registry as a confused deputy leaking data** | The payload is exactly what an anonymous `GET /api/v1/artifacts/{id}` returns. No credential, no private field, nothing the subscriber could not have polled for. |
| **Receiver cannot authenticate us** | Every POST is signed, with a replay-resistant timestamp in the signed material. |
| **A hostile receiver replying with a gigabyte** | Response bodies are read only far enough to quote the failure back to the owner. |
| **Enumerating another deployment's subscriptions** | A subscription id that is not yours returns `403`, never `404` — the split would itself be an oracle. |
| **Managing someone else's subscription** | `may_manage` is character-for-character the rule `api::tokens` uses: the owning Instance's credential, a curator, or an admin. The Instance comes from the credential, never from the path (§8.3). |

**The gap that remained, and how it was closed.** Between the pre-flight resolution check and
the connection `reqwest` opened, the name was resolved twice, and a DNS-rebinding attacker with
a very short TTL could win that race. The check now returns the addresses it approved and the
delivery is made with a client pinned to them via `resolve_to_addrs`, so there is no second
lookup. The hostname is still what TLS verifies, so pinning replaces DNS without weakening
identity. The whole list is closed.

---

## 6. Endpoints

```
GET    /api/v1/instances/{id}/subscriptions        list          owner | curator | admin
POST   /api/v1/instances/{id}/subscriptions        create        owner | curator | admin
GET    /api/v1/subscriptions/{sid}                 detail + recent deliveries
PATCH  /api/v1/subscriptions/{sid}                 filter, webhook, pause, resume, rotate secret
DELETE /api/v1/subscriptions/{sid}
GET    /api/v1/subscriptions/{sid}/deliveries      the pull path
POST   /api/v1/subscriptions/{sid}/deliveries/ack  advance the cursor
```

Settings are read from the environment, with the same `TAR_*` naming and duration grammar as
the rest of the registry, because `src/config.rs` is owned elsewhere while this lands:
`TAR_SUBSCRIPTION_WEBHOOKS`, `_ALLOW_HTTP`, `_ALLOW_PRIVATE_TARGETS`, `_TIMEOUT`, `_TICK`,
`_BATCH`, `_MAX_ATTEMPTS`, `_SUSPEND_AFTER`, `_BACKOFF_BASE`, `_BACKOFF_MAX`. Moving them into
`Config` is mechanical.

---

## 7. Known gaps

1. **Matching scans every enabled subscription per advertisement.** Correct at the scale this
   registry targets (tens of deployments) and next to a graph write it is not the bottleneck. An
   index on `conforms_to` is the obvious next step, and the reason it is not there yet is that
   an index disagreeing with `matches()` would be a correctness bug — the index has to be
   derived from the same function, not written twice.
2. **Deliveries are never pruned.** A busy registry accumulates rows for a subscription nobody
   drains. A sweeper keyed on `matched_at` and the acknowledged cursor is a few lines and no
   design work; it is left out rather than guessed at.
3. **`Retry-After` from the receiver is ignored** in favour of our own backoff.
4. **No fan-out across registries.** A subscription only sees what this registry is told. The
   federated version — subscribing at a peer, or a peer forwarding matches — has the same loop
   and trust problems federated search does (§9.6), and deserves its own note.
5. **No `subscribe:*` scope.** Authorisation reuses the token rule, so an Instance credential
   with only `advertise:produce` can also manage that Instance's subscriptions — the same
   latitude `api::tokens` already grants. A dedicated scope belongs in `auth::ALL_SCOPES`.
6. **DNS rebinding**, as above.
