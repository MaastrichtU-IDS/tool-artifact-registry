# Subscriptions

A subscription says: *tell me when an artifact matching this appears.* It belongs to a
deployment, which is what makes it a tool-to-tool mechanism rather than a notification feature —
the deployment that wants to know is the one that will act on it.

```
GET    /api/v1/instances/{id}/subscriptions
POST   /api/v1/instances/{id}/subscriptions
GET    /api/v1/subscriptions/{sid}
PATCH  /api/v1/subscriptions/{sid}
DELETE /api/v1/subscriptions/{sid}
GET    /api/v1/subscriptions/{sid}/deliveries
POST   /api/v1/subscriptions/{sid}/deliveries/ack
```

Managing a subscription needs `admin`, `curator`, or the credential of the deployment that owns
it. A mismatched subscription id returns `403` rather than `404`, so the endpoint cannot be used
to enumerate what exists.

## Creating one

```bash
curl -X POST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{
       "label": "shapes graphs to validate against",
       "filter": {
         "conforms_to": ["https://registry.example.org/type/shacl-shapes-graph"],
         "availability": ["public", "restricted"],
         "roles": ["produced"],
         "exclude_own": true
       },
       "webhook_url": "https://validator.example.org/hooks/tar"
     }' \
     https://registry.example.org/api/v1/instances/$INSTANCE_ID/subscriptions
```

Omit `webhook_url` and the subscription is pull-only. That is the whole difference between the
two modes; there is one queue underneath.

## The filter

Every field is a list, and they combine **OR within a field, AND across fields**. An empty
filter matches everything.

| Field | Matches |
|---|---|
| `conforms_to` | Artifact type IRIs. |
| `software` | Any deployment of that software. |
| `instance` | One named deployment. |
| `keywords` | `dcat:keyword`, case-insensitively. |
| `license` | SPDX IRIs, exactly. An artifact with no licence never matches a non-empty licence filter. |
| `availability` | `public`, `restricted`, `embargoed`, `metadata-only`. |
| `q` | Substring of title or description, case-insensitively. |
| `roles` | `produced`, `consumed`. Empty means `produced` only. |
| `exclude_own` | Default `true` — do not notify a deployment about its own output. |

Registry-minted software and deployment ids may be sent bare and are expanded server-side.
Type and licence IRIs must be full IRIs, because the registry has no basis for guessing what a
bare one meant.

`exclude_own` defaults to `true` because the overwhelmingly common subscription is "tell me what
*somebody else* made", and a tool woken by its own output is a loop.

### Filter on the type, not the keyword

A subscription written against a keyword is a subscription written against a spelling. This is
the case where the [vocabulary rules](../vocabulary/terms.md#why-this-is-not-free-text) earn
their keep: a subscription that never fires is indistinguishable from a quiet week, so nobody
notices it is broken.

## Webhook delivery

The registry `POST`s to `webhook_url`:

```
POST https://validator.example.org/hooks/tar
x-tar-delivery: 01a05…
x-tar-subscription: 01a05…
x-tar-timestamp: 1756567331
x-tar-attempt: 1
x-tar-signature: sha256=<hex>

{
  "type": "artifact.advertised",
  "subscription": "…", "registry": "…",
  "role": "produced",
  "run": "…", "instance": "…", "software": "…",
  "artifact_iri": "…",
  "artifact": { … }
}
```

`artifact` is exactly what an anonymous `GET /api/v1/artifacts/{id}` returns, so a receiver does
not have to call back for the ordinary case.

### Verifying the signature

`x-tar-signature` is `sha256=` followed by the hex HMAC-SHA256 of `"{timestamp}.{body}"` under
the subscription's secret. Supply `webhook_secret` when you create the subscription, or let the
registry generate one — it is returned once.

Include the timestamp in what you verify, and reject old ones. Signing the body alone would let
anyone who saw one delivery replay it forever.

### What the registry will not deliver to

HTTPS only unless `TAR_SUBSCRIPTION_ALLOW_HTTP` is set, and never to a private, loopback or
link-local address unless `TAR_SUBSCRIPTION_ALLOW_PRIVATE_TARGETS` is set. Redirects are not
followed.

A webhook URL is chosen by whoever registers the subscription and can point anywhere, so
refusing private targets is what stops the registry being used to reach inside a network on
somebody's behalf. The address is checked at registration *and* re-resolved at send
time, and the delivery then connects to exactly the address that check approved — the name is
not looked up again, so a record that changes in between cannot redirect the connection. The
certificate is still verified against the hostname.

Note the contrast with health probing, which allows private addresses by default. The two look
alike and are not: a deployment's endpoint is an address in your own estate, and for an internal
registry it is normally private.

### Retries

| | Default | |
|---|---|---|
| `TAR_SUBSCRIPTION_MAX_ATTEMPTS` | 8 | Attempts before one delivery is marked dead. |
| `TAR_SUBSCRIPTION_SUSPEND_AFTER` | 12 | Consecutive failures before the subscription's webhook is suspended. |
| `TAR_SUBSCRIPTION_BACKOFF_BASE` | 30s | |
| `TAR_SUBSCRIPTION_BACKOFF_MAX` | 6h | |
| `TAR_SUBSCRIPTION_TIMEOUT` | 5s | Per attempt, capped at 30s. |
| `TAR_SUBSCRIPTION_TICK` | 5s | Worker poll interval. |
| `TAR_SUBSCRIPTION_BATCH` | 20 | Deliveries attempted per tick. |
| `TAR_SUBSCRIPTION_WEBHOOKS` | `true` | The delivery worker at all. |

Backoff is `base × 2^(attempts−1)`, capped at max.

Suspension stops webhook attempts; **the pull path keeps working**, so a subscription whose
receiver was down for a day is not a subscription that lost its data. `PATCH {"resume": true}`
un-suspends it and re-arms failed and dead deliveries for another attempt.

## Pull delivery

For a tool that cannot accept an inbound connection — behind a firewall, on a laptop, running
only during a job.

```bash
curl -H "Authorization: Bearer $TOKEN" \
     'https://registry.example.org/api/v1/subscriptions/01a05…/deliveries?limit=50'
```

Returns the queued deliveries plus `next_cursor` and `remaining`. The cursor is a monotonic
sequence number, not an IRI. `limit` defaults to 25 and is clamped 1–200.

Acknowledge either by passing `ack=true` on the read — which acknowledges everything in that
response — or afterwards:

```bash
curl -X POST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"cursor": 4711}' \
     https://registry.example.org/api/v1/subscriptions/01a05…/deliveries/ack
```

The acknowledged cursor only ever advances. Acknowledging an older value is a no-op rather than
a rewind, so a slow consumer racing itself cannot replay what it has already handled.

Omit the cursor on a read and you resume from wherever you last acknowledged.

`ack=true` is the convenient form and the lossy one: if your process dies between receiving the
response and acting on it, that work is gone. Acknowledge separately if the work matters.
