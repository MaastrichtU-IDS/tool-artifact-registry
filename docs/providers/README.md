# Guide for service providers

This part is for teams who **run a tool or service** and want it in the registry: a validator,
a converter, a pipeline, a data service. It walks through the whole journey in order, with
commands that were run against a real registry and identity provider while it was written. The
[API chapter](../api/conventions.md) is the reference behind each step; this is the route
through it.

## What you get out of it

- **Your tool is findable by what it does.** Someone looking for "something that produces SHACL
  validation reports" finds it before anything has run, because you declared it.
- **Your output is findable, with its history.** Every run your deployment advertises links the
  artifacts it used to the ones it produced, so "where did this file come from?" has an answer.
- **You hear about other tools' output.** A subscription notifies your service, by webhook or
  by polling, when an artifact it cares about is advertised.

The registry holds **descriptions and pointers, never your data**. Access to an artifact stays
with whoever serves it.

## The journey

| Step | You do | Once or often |
|---|---|---|
| 1 | [Get access](register.md#1-get-access) — sign in, or ask a registry admin | Once |
| 2 | [Register your software](register.md#2-register-your-software) — what the program is and what it can do | Once, then per release |
| 3 | [Register each deployment](register.md#3-register-each-deployment) — where it runs, and its credential | Once per deployment |
| 4 | [Advertise runs](advertise.md) — what each job used and produced | Every job |
| 5 | [Subscribe](subscribe.md) — hear about other tools' artifacts | Optional |
| 6 | [Keep it current](advertise.md#keep-the-deployment-record-current) — announce on start | Every start |

When something is refused, [Troubleshooting](troubleshooting.md) maps each error to its fix.

## Three words used throughout

**Software** is the program, independent of where it runs: `report-validator`.
**Deployment** is one running installation of it: "report-validator, production, at
validator.example.org". The registry's API calls it an *instance*.
**Run** is one job a deployment performed, and **artifacts** are what that job used and produced.

A run always belongs to a deployment, and the deployment is decided by the credential, never
by what a request says. That is why step 3 exists: it is what lets the registry state, as a
fact, which installation made a file. [The model](../model.md) explains why.

## Before you start

- The registry's address. This guide writes `https://registry.example.org`.
- Your identity provider's token endpoint, if the registry uses one. With Keycloak it is
  `https://<keycloak>/realms/<realm>/protocol/openid-connect/token`.
- `curl`. The examples also come in Python, as a GitHub Actions step and as a Kubernetes init
  container, all in [`examples/providers/`](https://github.com/MaastrichtU-IDS/tool-artifact-registry/tree/main/examples/providers).
