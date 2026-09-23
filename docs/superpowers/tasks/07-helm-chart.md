# 07 — Helm chart and ServiceMonitor, or drop the promise

**Kind:** Needs decision · **Source:** spec §10.3

## The gap

The design promises a Helm chart at `deploy/helm/tool-artifact-registry` (Deployment with one
replica and `strategy: Recreate` because Oxigraph is single-writer, RWO PVC, Service, Ingress,
ConfigMap, Secret, **ServiceMonitor**). What exists is plain Kustomize under `deploy/kubernetes/`,
documented in `docs/operations/deployment.md`. `/metrics` exists; nothing tells Prometheus about it.

## Decide first (the user's call)

1. **Build the chart** — for operators outside IDS who expect `helm install`. Costs a second set
   of manifests to keep in step with Kustomize.
2. **Keep Kustomize only** — amend spec §10.3 to say so and why; add a `ServiceMonitor` (and the
   rate-limit `TAR_TRUSTED_PROXIES` note) to the Kustomize set as an optional component.
3. **Chart generated from, or replacing, Kustomize** — one source of truth.

The ids3 deployment (spec §10.4) uses Kustomize + ArgoCD and lives in the services repository,
not here — check with the user whether that already exists before duplicating anything.

## Done looks like

Whichever option: manifests that `kubectl apply -k` / `helm template` render without error
(add a CI step that renders them), one replica and `Recreate` preserved, a ServiceMonitor
scraping `/metrics`, `docs/operations/deployment.md` and spec §10.3 agreeing with what ships.

## Decisions (user, 2026-09-24)

- **Kustomize only.** Amend spec §10.3 to say so and why, and add a **ServiceMonitor as an
  optional Kustomize component**. Render the manifests in CI.
- The ids3 deployment **already exists in the services repository**. This repo ships only the
  generic base and the component, and the docs point to the services repo for ids3.
