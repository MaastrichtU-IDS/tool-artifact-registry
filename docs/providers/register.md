# Register your software and deployments

## 1. Get access

**If the registry has sign-in** (a **Sign in** button in the top bar), sign in with your
organisation account. What you may do depends on the role your account has in the identity
provider:

| Role | You can |
|---|---|
| none, or `reader` | Browse and search. With anonymous reads turned off, `reader` is what lets you read at all. |
| `curator` | Register and edit software, releases and deployments, and mint deployment tokens. |
| `admin` | Everything, plus peers, the audit log and admin tokens. |

A service provider needs **`curator`**. Ask whoever administers your identity provider to add
it; in Keycloak it is a realm role.

> **`curator` is registry-wide.** It lets you edit and withdraw *any* software or deployment,
> not only your own: the registry has no per-owner permissions yet. Treat it as a trusted role,
> and change only your own records.

**If the registry has no sign-in**, or you do not have `curator`, ask a registry admin to do
steps 2 and 3 for you. Send them the details from the two sections below, and they will hand
back a credential for your deployment. Everything from [step 4](advertise.md) on you then do
yourself.

To check what the registry thinks you are, at any point:

```bash
curl -H "Authorization: Bearer $TOKEN" https://registry.example.org/api/v1/whoami
```

```json
{"credential": "oidc-human", "roles": ["reader", "curator"], "is_curator": true, "is_admin": false}
```

## 2. Register your software

In the UI: **Software → Register software** (`/software/new`). Or with the API, using your
signed-in token:

```bash
curl -X POST https://registry.example.org/api/v1/software \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "name": "report-validator",
    "tagline": "Validates RDF graphs and publishes a report.",
    "code_repository": "https://github.com/your-org/report-validator",
    "license": "https://spdx.org/licenses/Apache-2.0",
    "kinds": ["service"],
    "deployable": true,
    "topics": ["<a topic IRI, see below>"],
    "contact": {"name": "A Maintainer", "kind": "person", "email": "maintainer@example.org"}
  }'
```

The answer carries the record's `id`. Keep it; everything below needs it.

A few fields decide how useful the record is:

- **`license`** is an SPDX IRI, not a name: `https://spdx.org/licenses/MIT`, not `MIT`. Leave it
  out if the project states none.
- **`kinds`** can be several of `service`, `library`, `cli`, `desktop`, `workflow`.
- **`deployable: false`** for a library or desktop program: it has no endpoint to run at.
- **`code_repository`** on GitHub also gets you stars, forks and last push on the software page,
  and makes [repository sync](../api/software.md) possible.

[Registering software](../api/software.md) lists every field.

### Say what it consumes and produces

This is what makes your tool findable by what it does, before it has run once:

```bash
curl -X PUT https://registry.example.org/api/v1/software/$SOFTWARE_ID/capability \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"consumes": ["https://registry.example.org/type/rdf-graph"],
       "produces": ["https://registry.example.org/type/shacl-validation-report"]}'
```

### Types and topics are looked up, not typed

Artifact types (`consumes`, `produces`, and later `conforms_to`) and `topics` must be terms the
registry holds. Search for them and use the `iri` that comes back:

```bash
curl 'https://registry.example.org/api/v1/vocab/search?branch=data&q=validation%20report'
# {"items":[{"iri":"https://registry.example.org/type/shacl-validation-report",
#            "label":"SHACL validation report"}, …]}

curl 'https://registry.example.org/api/v1/vocab/search?branch=topic&q=ontology'
```

Send a term it does not hold and the write is refused with a `422` that says what to do, which
is one of:

1. **Pick an existing term.** Almost always the answer: search with a different word.
2. **Adopt a term defined elsewhere** that you have the IRI for: `POST /api/v1/types` with that
   `iri`.
3. **Mint a new type**, only when nothing anywhere names the thing: `POST /api/v1/types`
   without an `iri`.

This strictness is on purpose. A type spelt two ways is two types, and a subscription written
against the other spelling never fires. [Artifact types and topics](../vocabulary/terms.md) says
more.

### Releases

A release records a version you ship, with its image or download:

```bash
curl -X POST https://registry.example.org/api/v1/software/$SOFTWARE_ID/releases \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"version": "2.2.0", "container_image": "ghcr.io/your-org/report-validator:2.2.0"}'
```

The answer's `id` is what a deployment names to say which release it runs (see
[step 6](advertise.md#keep-the-deployment-record-current)).

## 3. Register each deployment

Each running installation gets its own deployment record **and its own credential**. The
credential is how the registry knows, for every run advertised later, which installation ran
it.

### The recommended way: an identity-provider client per deployment

Ask your identity-provider administrator for a **client for this deployment**. For Keycloak:

- **Client authentication** on (a confidential client), with **Service accounts** enabled. That
  is what allows the `client_credentials` flow.
- An **audience mapper** adding the registry's address (`https://registry.example.org`) to the
  access token's `aud`. Without it the registry rejects the token as `InvalidAudience`, even
  though Keycloak issued it happily. This is the step most often missed.

You get a client id and a secret. The secret stays with your deployment; **the registry never
stores it**, and rotating it is a job for the identity provider, where your organisation already
manages credentials.

Then create the deployment record, bound to that client. In the UI, open your software and
choose **Create deployment**: that page also prints the exact commands for your registry. Or
with the API:

```bash
curl -X POST https://registry.example.org/api/v1/instances \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "label": "report-validator, production",
    "software": "'$SOFTWARE_ID'",
    "endpoint_url": "https://validator.example.org",
    "health_endpoint": "https://validator.example.org/healthz",
    "oidc_client_id": "report-validator-prod",
    "allowed_scopes": ["advertise:produce", "advertise:consume"]
  }'
```

- **`oidc_client_id`** is the client id you were given. It binds the client to this deployment.
- **`health_endpoint`** should answer `2xx` when the service is healthy. The registry probes it
  every five minutes (by default) and shows the result. Without it, the `endpoint_url` is probed, and any
  answer at all counts as up.
- **`allowed_scopes`** is what the deployment may do: advertise what it produced and what it
  consumed.

Check that it works, from wherever the deployment runs:

```bash
TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" -d grant_type=client_credentials \
  "$TOKEN_URL" | jq -r .access_token)
curl -H "Authorization: Bearer $TOKEN" https://registry.example.org/api/v1/whoami
```

```json
{"credential": "oidc-workload", "subject": "report-validator-prod",
 "instance": "https://registry.example.org/instance/01a0…",
 "scopes": ["advertise:consume", "advertise:produce"]}
```

`"instance"` naming your deployment is the sign that everything is wired up.

### The fallback: a registry API token

When the deployment cannot reach the identity provider, or the registry has none, mint a token
for the deployment instead. In the UI: the deployment's page → **Credentials**. Or:

```bash
curl -X POST https://registry.example.org/api/v1/instances/$DEPLOYMENT_ID/tokens \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"scopes": ["advertise:produce", "advertise:consume"], "label": "ci", "expires_in": "90d"}'
```

The token is shown **once**. Store it as a secret, and give it an expiry: unlike an
identity-provider client, rotating it is your job, done in the registry. Every example in this
guide takes `TAR_TOKEN` in place of the client id and secret.

### Many deployments of the same program

If deployments come and go (per cluster, per customer, per branch), creating each by hand does
not scale. [Self-registration](../api/deployments.md#self-registering) lets the program create
its own records, all with one shared credential (a software key, or a client listed in the
software's `registration_clients`).

Each deployment announces itself under its own `instance_key` (a cluster name, a hostname), and
names that key again when it advertises:

```json
{"instance_key": "cluster-a",
 "run": {"external_key": "job/77", "status": "success"},
 "artifacts": [ … ]}
```

The shared credential registered every one of these deployments, so naming the key gives it
nothing it did not already have. With only one deployment registered, the key can be left out;
with several, an advertisement without it is refused rather than attributed to the wrong one.

For a handful of long-lived deployments, one client each, as above, is simpler: no keys to
keep apart.
