# Advertise runs and artifacts

At the end of a job, your deployment tells the registry what it ran, what it used and what it
produced. Two calls, both authenticated **as the deployment**:

```
POST /api/v1/advertise/consumed     the artifacts this run used
POST /api/v1/advertise/produced     the artifacts this run generated
```

Both take one run and a list of artifacts. The first call creates the run, and the second finds
it by its key, so a job that consumes and produces makes two calls and gets one run.

Only the deployment can advertise its own runs. A curator's or an admin's token is refused,
however powerful: being trusted is not the same as being the thing that ran.

## Five rules that save you trouble

1. **Always send `run.external_key`**, from something your system already has: a CI run id, a
   job id. A retry with the same key updates the same run. Without one, every call records a new
   run.
2. **Always send an `external_key` on each artifact**, such as `<run key>/report`. A retry with
   the same keys is then recognised as a retry (`"created": false`), even if other fields
   changed.
3. **Get it right before you send it.** An artifact cannot be edited or withdrawn after it is
   advertised, and re-advertising with the same keys keeps the first version. Validate your
   payload in a test job first.
4. **Advertise late.** Post when you know the outcome, with `status` `success` or `failed`. A
   run posted as `running` and never updated stays "running".
5. **Say when the bytes are not obtainable.** Set `availability` on the distribution to
   `restricted` (with an `access_request_url`), `embargoed` or `metadata-only`. That tells a
   reader "you cannot download this from here" instead of leaving them to guess.

## The payload

```json
{{#include ../../examples/providers/produced.example.json}}
```

- **`conforms_to`** is the artifact's type, a term the registry holds (see
  [types](register.md#types-and-topics-are-looked-up-not-typed)).
- **`distributions`** say where and how the bytes can be had. Add `checksum`
  (`{"algorithm": "sha256", "value": "<hex>"}`) whenever you can: it gives the artifact a
  content identifier, so anyone holding the same file can find this record.
- **`was_derived_from`** lists the artifacts it was made from, including ones at another
  registry. That is lineage across registries, and it costs nothing at advertising time.

[Advertising runs and artifacts](../api/advertising.md) has every field, OpenLineage input, and
artifacts that have no run.

The answer names what now exists:

```json
{"run": "https://registry.example.org/run/01a0…",
 "artifacts": ["https://registry.example.org/artifact/01a0…"],
 "created": true}
```

## From a shell or any CI

[`advertise.sh`](https://github.com/MaastrichtU-IDS/tool-artifact-registry/blob/main/examples/providers/advertise.sh)
needs nothing but `curl`. It gets a token for the deployment, posts the payload, retries on
`429` and `5xx`, and on any other error prints the registry's explanation and exits non-zero:

```bash
export TAR_URL=https://registry.example.org
export TAR_TOKEN_URL=https://sso.example.org/realms/main/protocol/openid-connect/token
export TAR_CLIENT_ID=report-validator-prod TAR_CLIENT_SECRET=…   # or: TAR_TOKEN=tar_…

./advertise.sh consumed consumed.json
./advertise.sh produced produced.json
```

```sh
{{#include ../../examples/providers/advertise.sh}}
```

## From Python

[`tar_client.py`](https://github.com/MaastrichtU-IDS/tool-artifact-registry/blob/main/examples/providers/tar_client.py)
is standard library only. It renews its token before it lapses and waits out a `429`. Import it,
or copy it into your project:

```python
from tar_client import Registry, RegistryError

reg = Registry()  # reads TAR_URL, TAR_TOKEN_URL, TAR_CLIENT_ID, TAR_CLIENT_SECRET (or TAR_TOKEN)
job = "nightly/2026-09-29"
reg.advertise(
    "produced",
    run={"external_key": job, "status": "success"},
    artifacts=[{
        "external_key": f"{job}/report",
        "title": "Nightly validation report",
        "conforms_to": "https://registry.example.org/type/shacl-validation-report",
        "distributions": [{"download_url": "https://validator.example.org/reports/nightly.ttl",
                           "media_type": "text/turtle"}],
    }],
)
```

A refusal raises `RegistryError`, with the HTTP status and the registry's problem document on
it, so the reason is in your job's log.

<details>
<summary>The whole client</summary>

```python
{{#include ../../examples/providers/tar_client.py}}
```

</details>

## From GitHub Actions

Add a step after the one that produces the file. The client id and secret go in the repository's
secrets; the registry address, token endpoint and artifact type go in its variables. The run key
is built from the workflow run, so each attempt is one run.

```yaml
{{#include ../../examples/providers/github-actions.yml}}
```

## Keep the deployment record current

A deployment can maintain its own record: its endpoint, health URL and which release it runs.
`PUT /api/v1/instances/self` with the deployment's own credential updates the record that
credential is bound to, so it is safe to call on **every start**:

```bash
./advertise.sh announce record.json
```

```json
{"label": "report-validator, production",
 "endpoint_url": "https://validator.example.org",
 "health_endpoint": "https://validator.example.org/healthz",
 "release": "<the release's id, from step 2>"}
```

Name the release by its **id**. The `version` string alone does not link a release.

### On Kubernetes

An init container announces the deployment each time a pod starts, so rolling out a new release
updates the registry without anyone editing it by hand. It never blocks the pod: if the registry
or the identity provider is unreachable, it says so in the log and lets the service start.

```yaml
{{#include ../../examples/providers/kubernetes-announce.yaml}}
```

## See what you advertised

Every artifact and run has a page at its IRI: open `https://registry.example.org/artifact/01a0…`
in a browser. Or:

```
GET /api/v1/instances/{id}/runs         your deployment's runs
GET /api/v1/instances/{id}/artifacts    everything its runs touched
GET /api/v1/artifacts/{id}/lineage      where an artifact came from, and what it fed
```
