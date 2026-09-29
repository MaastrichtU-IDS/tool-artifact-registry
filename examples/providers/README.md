# Examples for service providers

Copy-and-adapt examples for a deployment that advertises to the registry. The
[guide for service providers](../../docs/providers/README.md) explains when and why to use each.

| File | What it does |
|---|---|
| `advertise.sh` | Advertise a run's artifacts, or announce the deployment, with nothing but `curl`. |
| `produced.example.json` | A payload for `advertise.sh produced`. |
| `tar_client.py` | A standard-library Python client: token handling, announce, advertise, `429` back-off. |
| `github-actions.yml` | A workflow step that advertises what a CI job produced. |
| `kubernetes-announce.yaml` | An init container that announces the deployment on every pod start. |

All of them read the same environment variables: `TAR_URL`, `TAR_TOKEN_URL`, `TAR_CLIENT_ID`
and `TAR_CLIENT_SECRET` for an identity-provider client, or `TAR_TOKEN` for a registry API
token. Each was run against a registry and a Keycloak (`deploy/keycloak/`) when it was written.
