# Deployment Manifests — Design Note

| | |
|---|---|
| **Status** | Implemented |
| **Date** | 2026-09-24 |
| **Spec** | [`2026-08-30-tool-artifact-registry-design.md`](2026-08-30-tool-artifact-registry-design.md) — amends §10.3 |
| **Code** | `deploy/kubernetes/`, `deploy/kubernetes/components/servicemonitor/`, `.github/workflows/ci.yml` |

---

## 1. Why

§10.3 promised a Helm chart at `deploy/helm/tool-artifact-registry`, with a ServiceMonitor among
its objects. What was built is plain Kustomize under `deploy/kubernetes/`, and nothing told
Prometheus that `/metrics` exists. The spec and the repository disagreed, and nothing checked
that the manifests that do exist still render.

## 2. Decision

**Kustomize only. No Helm chart.**

- **One set of manifests to keep correct.** A chart next to the Kustomize set is a second copy
  of every object, and the two drift the first time someone fixes one and forgets the other.
  The deployment is one workload, one service and one volume; a chart's templating buys nothing
  that a kustomize `replacement` does not already do.
- **The ids3 deployment uses Kustomize and ArgoCD anyway** (§10.4). Its manifests live in the
  services repository, not here, so this repository ships only the generic base.

The ServiceMonitor is an **optional component**, off by default. It needs the Prometheus
Operator's CRDs, and applying it to a cluster without them fails the whole `kubectl apply -k`.
A default that breaks on a plain cluster is the wrong default.

## 3. What ships

- **`deploy/kubernetes/components/servicemonitor/`**, a `kind: Component` holding one
  ServiceMonitor that scrapes `/metrics` on the Service's `http` port every 60 seconds. It is
  enabled by adding it to `components:` in `deploy/kubernetes/kustomization.yaml`. Being part
  of the base kustomization, it picks up the base's namespace and labels like everything else.
- **The Service carries `app.kubernetes.io/name: tool-artifact-registry`**, the label the
  ServiceMonitor selects on. Before, the Service had only the decorative labels kustomize adds,
  and a ServiceMonitor selects Services, not pods.
- **A `manifests` CI job** renders the base with `kubectl kustomize`, then renders it again with
  the component enabled, so a manifest that no longer assembles fails CI. `kubectl` is already
  on the hosted runner; no action is added.
- **Spec §10.3** says Kustomize only, and why.

One replica and `strategy: Recreate` are unchanged: both stores are single-writer.

## 4. Not done, deliberately

- **A Helm chart**, generated or hand-written. Worth revisiting only if operators outside IDS
  ask for `helm install` and a Kustomize base is not enough for them.
- **Schema validation of the rendered output.** `kubectl kustomize` checks that the manifests
  assemble, not that every field is valid for a given Kubernetes version. `kubectl apply
  --dry-run=server` would, but it needs a cluster, and the ServiceMonitor needs one with the
  CRDs installed.
- **ids3-specific manifests.** They live in the services repository.
