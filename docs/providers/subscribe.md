# React to other tools' artifacts

A subscription asks the registry to tell your deployment when an artifact it cares about is
advertised, for example "any SHACL shapes graph, from anyone but me". It belongs to a
deployment and is managed with that deployment's credential. In the UI: the deployment's page
→ **Subscriptions** → **New subscription**.

## Create one

```bash
curl -X POST https://registry.example.org/api/v1/instances/$DEPLOYMENT_ID/subscriptions \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "label": "shapes graphs to validate against",
    "filter": {
      "conforms_to": ["https://registry.example.org/type/shacl-shapes-graph"],
      "roles": ["produced"],
      "exclude_own": true
    },
    "webhook_url": "https://validator.example.org/hooks/registry"
  }'
```

**Filter on the type** (`conforms_to`), not on keywords. A type is a checked term; a keyword is
a spelling, and a subscription on the wrong spelling never fires, which looks exactly like a
quiet week. Within a field the values are alternatives; across fields all must match.
[Subscriptions](../api/subscriptions.md#the-filter) lists every filter field.

`exclude_own` (the default) keeps your deployment from being woken by its own output.

## Webhook or pull

**Webhook**: give a `webhook_url`, and the registry `POST`s each match to it, retrying with
backoff if your service is down. The URL must be `https` and publicly reachable: the registry
refuses to deliver into private networks.

**Pull**: leave `webhook_url` out. Right for a job that runs on a schedule, sits behind a
firewall, or cannot accept inbound connections. Read the queue, act on it, then acknowledge:

```bash
curl -H "Authorization: Bearer $TOKEN" \
  https://registry.example.org/api/v1/subscriptions/$SUBSCRIPTION_ID/deliveries
# {"items": [{"seq": 1, "role": "produced", "artifact_iri": "…",
#             "notification": {"artifact": {"title": "…", …}, "run": "…", …}}],
#  "next_cursor": 1, "remaining": 0}

curl -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"cursor": 1}' \
  https://registry.example.org/api/v1/subscriptions/$SUBSCRIPTION_ID/deliveries/ack
```

Acknowledge **after** the work is done. Reading again without a cursor resumes after the last
acknowledgement, so a job that crashes halfway sees the same deliveries next time.

A webhook subscription can be pulled as well: when your receiver was down long enough for the
webhook to be suspended, nothing is lost, and the queue is still there to read.

## Verify webhook deliveries

Every delivery is signed. Check the signature before trusting the body. The headers:

```
x-tar-timestamp: 1759140000
x-tar-signature: sha256=<hex>
```

The signature is HMAC-SHA256, keyed with the subscription's secret, over `"<timestamp>.<raw
body>"`. The registry generates the secret and returns it **once**, as `secret` in the answer
to the create call; store it then. Or send your own as `webhook_secret`. In Python:

```python
import hashlib, hmac, time

def verify(secret: str, timestamp: str, body: bytes, signature: str, max_age=300) -> bool:
    if abs(time.time() - int(timestamp)) > max_age:
        return False  # an old delivery replayed
    mac = hmac.new(secret.encode(), timestamp.encode() + b"." + body, hashlib.sha256)
    return hmac.compare_digest("sha256=" + mac.hexdigest(), signature)
```

Use the raw request body, exactly as received, and not a re-serialised copy: the signature is
over the bytes.

## A credential that can only subscribe

If the thing that receives deliveries should not be able to advertise, mint the deployment a
token with only `subscribe:artifacts`. It can create, read, edit and acknowledge that
deployment's subscriptions, and nothing else.
