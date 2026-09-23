# Design record

The documents that were written before and during the build. They are kept as a record of what
was decided and why, not as a description of the current system — where a spec and the code
disagree, the code is right and the rest of this site describes it.

They are worth reading for the reasoning. The chapters elsewhere say what the registry does; these
say what else was considered.

| | |
|---|---|
| [Design](2026-08-30-tool-artifact-registry-design.md) | The whole system: the model, the endpoints, the authorisation rules, the open questions. |
| [Vocabulary audit](2026-08-30-vocabulary-audit.md) | Which standard vocabularies the RDF terms were checked against, and where a registry-specific term was unavoidable. |
| [Workload identity](2026-08-30-workload-identity-addendum.md) | How a deployment authenticates without a stored secret. |
| [Artifact subscriptions](2026-08-31-artifact-subscriptions.md) | Filters, delivery, retries, and the security argument for refusing private webhook targets. |
| [Federated search propagation](2026-08-31-federated-search-propagation.md) | Live fan-out across a graph of registries without looping. |
| [Rate limiting](2026-09-23-rate-limiting.md) | Answers Q8: cost classes, keying on the verified principal, and finding the client behind a proxy. |
| [Changing the base IRI](2026-09-23-base-iri-rebase.md) | Answers Q9: renaming the registry's own records, and answering for the old IRIs afterwards. |
| [Deployment manifests](2026-09-24-deployment-manifests.md) | Amends §10.3: Kustomize only, no Helm chart, and an optional ServiceMonitor. |

The [frontend handoff](../design-handoff.md) is the corresponding document for the UI, including
the questions it left open and the answers they got.
