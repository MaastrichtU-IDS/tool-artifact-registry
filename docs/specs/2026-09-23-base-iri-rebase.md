# Changing the Base IRI — Design Note

| | |
|---|---|
| **Status** | Implemented |
| **Date** | 2026-09-23 |
| **Spec** | [`2026-08-30-tool-artifact-registry-design.md`](2026-08-30-tool-artifact-registry-design.md) — answers Q9 |
| **Code** | `src/rebase.rs`, `src/store/` (`GraphTx::clear_graphs`), `src/config.rs`, `src/main.rs`, `src/api/mod.rs`, `src/auth/jwt.rs`, `src/api/registry.rs`, `tests/rebase.rs` |

---

## 1. Why

Every identifier this registry mints is `{TAR_BASE_IRI}/{kind}/{uuid}`. Until now the deployment
guide said the base could not be changed, and that the only help was `tar dump` showing how many
identifiers a change had broken. Q9 asked for a migration before the first production
deployment has to move domain. Domains do move: an institute renames itself, or a pilot on
`registry.dev.example.org` becomes the real service.

There are two separate problems:

1. **The data.** The registry's own records must be renamed, and so must every reference to
   them in its own store.
2. **Everyone else.** Peers, exported files, papers and CI pipelines already hold the old IRIs.
   Nothing the registry does can rewrite those. It can only keep answering for them.

---

## 2. Where the base lives

I measured this against a seeded store (12,398 quads) and read the schema:

| Place | Holds our IRIs | After a base change |
|---|---|---|
| `<urn:tar:local>` | Every record's subject, and references between records | **Must be rewritten** |
| `<urn:tar:bundle:keywords>` | The keyword scheme, minted under the base | Reloaded by itself: the bundle digest includes the base (`bundles.rs`) |
| Other bundle graphs, `<urn:tar:shapes>` | Nothing | — |
| `<urn:tar:peer:*>` | A peer's statements, which may cite our records | Left alone: it is the peer's data, refreshed from the peer |
| Literals, anywhere | None found | — |
| Ops DB | `api_tokens.instance_iri`, `.software_iri`; `subscriptions.instance_iri`, `.filter` (JSON); `subscription_deliveries.artifact_iri`, `.run_iri`, `.payload` (JSON); `run_keys.instance_iri`, `.run_iri`; `artifact_keys.artifact_iri`; `advertise_idem.run_iri`, `.artifact_iri`; `audit_log.target` | **Must be rewritten**, or a deployment's token stops mapping to its Instance |

The ops DB is what makes a hand-edited dump insufficient. A token bound to
`https://old/instance/…` authenticates as an Instance that no longer exists, and every
advertisement from that deployment is refused.

---

## 3. `tar rebase --from <old>`

An offline command, run against a stopped registry, like `restore`: both stores are
single-writer. It renames everything under `<old>/` to the same path under the current
`TAR_BASE_IRI`.

- **Graph.** It reads `<urn:tar:local>` and rewrites every IRI that starts with `<old>/`, plus
  `<old>` itself, which is the IRI of the registry's own catalog and the object of every
  Instance's `dcat:inCatalog`. It writes the result back in **one** `GraphTx`, which clears the
  graph and inserts the rewritten one. `GraphTx` gained `clear_graphs` for this: the local
  graph also holds foreign subjects (organisations, content hashes) and top-level blank nodes,
  and a per-subject delete would miss orphans. The transaction is atomic on both backends
  (`CLEAR SILENT GRAPH` in the same update request on an external endpoint), so a failure
  leaves the graph untouched.
- **Ops DB.** One SQLite transaction. IRI columns are rewritten by prefix. The two JSON
  columns get a string replace of `"<old>/` with `"<new>/`, which only matches an IRI where a
  JSON string begins.
- **Order and reruns.** The graph goes first, then the ops DB. Each step only touches values
  that still carry the old prefix, so if the command dies between the two, running it again
  finishes the job. A second run on a finished store changes nothing, and says so.
- **`--dry-run`** counts what would change and writes nothing.
- It refuses when `<old>` equals the new base, when `<old>` is not an `http(s)` URL, and when
  one base is a prefix of the other (`https://x.org` → `https://x.org/registry`), because a
  second run would then rewrite the new IRIs again.

It does not take a backup for you. The docs say to take one first (`/admin/dump` while running,
or a volume snapshot), as they already do for upgrades.

---

## 4. Keep answering for the old IRIs: `TAR_PREVIOUS_BASE_IRIS`

A comma-separated list of bases this registry used to have. With it set:

1. **Redirects.** A request whose `Host` (plus path prefix) matches a previous base gets `308
   Permanent Redirect` to the same path and query under the current base. This covers the common
   move, where the old DNS name still points at the same service. If the old name points
   somewhere else, that server has to redirect instead, and the docs show the one-line nginx
   rule.
2. **Translating input.** Old IRIs keep arriving after the move: a CI pipeline advertising an
   artifact it consumed by its old IRI, a peer calling `/resolve`, a SPARQL query pasted from a
   paper. A middleware rewrites `<old>/` to `<new>/` in the query string (each parameter decoded,
   rewritten and re-encoded) and in the body when it is JSON, SPARQL, or a form. Doing it in one
   place stores a single canonical form, so a lineage edge to an old IRI does not dangle, and no
   handler needs to know. Octet-stream and multipart bodies (artifact bytes) are never touched.
3. **Token audience.** When `TAR_OIDC_AUDIENCE` is unset, the expected audience is the base.
   Tokens minted for the old base are accepted too, so signed-in users and workloads keep
   working while the identity provider's audience mapper is updated. An explicit
   `TAR_OIDC_AUDIENCE` is left exactly as configured.
4. **Discovery.** `/.well-known/tar-registry` lists `previous_base_iris`, so a peer or a client
   can tell that the registry moved.

At boot the registry refuses a previous base that equals the current one, or one that is a
prefix of it (or the reverse). It also logs a warning if `<urn:tar:local>` still holds a record
under any base other than the current one, naming that base and the command that fixes it. That
catches the mistake the deployment guide warned about: someone edits the setting and forgets
the migration.

---

## 5. Order of operations for an operator

1. Take a backup.
2. Stop the registry.
3. Set `TAR_BASE_IRI` to the new base, and `TAR_PREVIOUS_BASE_IRIS` to the old one.
4. `tar rebase --from <old> --dry-run`, then `tar rebase --from <old>`.
5. Point the old DNS name at the service (or redirect it elsewhere). Update the ingress, the
   certificate, and the identity provider's audience mapper.
6. Start the registry.

---

## 6. Tests

**Unit** (`src/rebase.rs`): IRI prefix rewriting (only a whole `<old>/` prefix matches, so
`https://old.orgx/…` is left alone); query-string and JSON rewriting; refusal of equal or nested
bases.

**End-to-end** (`tests/rebase.rs`): records, a run and a software token are created under an
old base, rebased, and then read under the new one:

- every record resolves under the new IRI, and none remains under the old;
- a token minted before the move still authenticates, as the same Instance;
- a second run changes nothing;
- `--dry-run` writes nothing;
- a request to the old host redirects with `308`;
- an advertisement citing an old artifact IRI is stored under the new one.

---

## 7. Not done, deliberately

- **Peers do not follow a move automatically.** A peer holding our old base keeps resolving it,
  and the redirect keeps that working. Having a peer notice `previous_base_iris` and update its
  own records is a change to the peer resolver. It is worth doing once there is a federation of
  registries run by different people.
- **`owl:sameAs` from old to new in the graph.** The redirect and the input translation already
  answer for old IRIs. Keeping the old names in the graph as well would double every
  identifier for SPARQL users. Add it if someone asks a SPARQL question the redirect cannot
  answer.
- **Online rebase.** The command needs the registry stopped. The move is a planned, one-off
  outage anyway, since DNS and certificates change at the same time.
- **A full IRI in a path position** (`/api/v1/software/https%3A…`) is not translated. The
  documented way to address a record is its id, which does not contain the base.
