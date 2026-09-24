# Repository Liveness — Design Note

| | |
|---|---|
| **Status** | Implemented |
| **Date** | 2026-09-24 |
| **Spec** | [`2026-08-30-tool-artifact-registry-design.md`](2026-08-30-tool-artifact-registry-design.md) — §10.5 `TAR_FORGE_POLL_INTERVAL`; handoff §9 |
| **Code** | `src/domain/forge.rs`, `src/ops/mod.rs`, `src/model.rs`, `migrations/0005_repository_stats.sql`, `src/api/software.rs`, `src/config.rs`, `src/main.rs`, `frontend/src/routes/SoftwareDetail.tsx`, `tests/repository_stats.rs` |

---

## 1. Why

The handoff's v1 scope (§9) lists repository liveness — stars, forks, last-commit age — in the
software page's signal bar. Repository *sync* was built; the poller spec §10.5 names for these
numbers was not, so the UI has always omitted the cells (limitations #4). This is the last unmet
item of the v1 UI scope.

The user decided the four open questions on 2026-09-24:

- **SQLite only.** The numbers are operational data, like the result of a health probe's
  bookkeeping. They are not written to the graph, so neither `/sparql` nor a peer ever sees them.
  A peer that wants a repository's star count can ask GitHub itself; it should not take ours,
  which may be a day old.
- **GitHub only**, matching sync. A record hosted anywhere else keeps the cells omitted.
- **A background poller** on `TAR_FORGE_POLL_INTERVAL` (default `24h`), paced to stay inside
  GitHub's rate limit.
- **The same client, token and egress path as sync.**

---

## 2. What is fetched

One request per record: `GET https://api.github.com/repos/{owner}/{name}` — the very request sync
already makes first. It carries `stargazers_count`, `forks_count` and `pushed_at`, so no second
call is needed.

**Last commit is `pushed_at`.** Sync never reads commit dates, and asking for the default branch's
latest commit would double the requests. `pushed_at` is the time of the last push to *any* branch,
which is a fair reading of "is anyone working on this" — the question the cell answers — and it
is labelled "Last push" in the UI so it does not claim more than it is. The API field is
`last_commit_at`, as the brief named it.

**Which repository.** A record's repository is its sync repository when sync is configured, else
its `code_repository`, and only if that names a GitHub `owner/name` (`forge::github_repo`, now
shared with sync). Only local, non-withdrawn software is polled: a peer's records are the peer's
to poll, and spending our rate budget on them would buy numbers we are told not to publish.

---

## 3. Storage

A new table, `repository_stats` (`migrations/0005_repository_stats.sql`), one row per software
IRI:

| Column | |
|---|---|
| `repo` | the `owner/name` the numbers belong to |
| `stars`, `forks`, `last_commit_at` | from the last successful fetch; null until there is one |
| `fetched_at` | when that successful fetch happened |
| `checked_at` | when the last attempt, successful or not, happened |
| `last_error` | the last attempt's error, null after a success |

**A failure keeps the old numbers.** A day-old star count is still true enough to show; replacing
it with nothing because GitHub had a bad minute would make the page flicker for no reason. The
error is recorded next to it, so a fetch that has been failing for a week is visible.

**Unless the repository changed.** If the record now points at a different repository, the old
numbers describe the wrong project. A failed fetch for the new repository clears them, and the
read path shows a row only when its `repo` matches the record's current repository — which also
covers the window between a curator changing the link and the next poll.

---

## 4. The poller

`forge::poll_loop`, spawned from `serve()` beside the health prober. Every
`min(TAR_FORGE_POLL_INTERVAL, 1h)` it lists the local GitHub-hosted software and refreshes each
record whose last attempt is older than the interval. The short wake-up is so a record created
this morning gets numbers within the hour rather than tomorrow; the due check is so a restart
does not re-spend the budget on every record.

**Pacing.** Requests are spread out: the gap between two is the interval divided by the number of
due records, but never less than a floor set by the credential —

| Credential | GitHub's limit | Floor | Budget used by the poller |
|---|---|---|---|
| `TAR_FORGE_TOKEN` | 5,000/h | 2 s | at most 1,800/h |
| anonymous | 60/h | 120 s | at most 30/h |

The floor leaves the rest of the hour's budget for curators pressing "sync", which shares it and
costs up to four requests a press. When the floor wins, a pass takes longer than the interval,
and the next pass starts when it ends. Anonymously that is 720 records a day, which is more than
any registry in this estate holds.

**Egress.** The fetch goes through `AppState::http`, the client sync uses, so it takes whatever
proxy the process is configured with (`HTTPS_PROXY`, which reqwest reads) exactly as sync does.
The credential is `forge::token_for(None)`, sync's own: `TAR_FORGE_TOKEN` when set. The poller
has no user, so there is no brokered token to prefer.

The rate-limiting middleware charges inbound requests; the poller makes none, so it is untouched.

---

## 5. API and UI

`GET /api/v1/software/{id}` gains an optional `repository_stats`:

```json
"repository_stats": {
  "stars": 42, "forks": 7,
  "last_commit_at": "2026-09-20T10:00:00Z",
  "fetched_at": "2026-09-24T03:00:00Z",
  "last_error": "…"
}
```

It is absent until a fetch has succeeded. Unknown is never reported as zero. `last_error` is
present only when the latest attempt failed, and says why the numbers may be stale. The list
endpoint does not carry it; nothing on a list page shows it.

The signal bar gains **Stars**, **Forks** and **Last push** cells, rendered only when
`repository_stats` is present — as the handoff requires, a missing value removes the cell rather
than showing `0` or `—`.

---

## 6. Tests

The forge is stubbed without a network and without a test server: `forge::poll_once` takes the
fetch as a closure, and the loop passes the real one. The tests pass a closure.

**Unit** (`src/domain/forge.rs`): which repository strings name a GitHub repository; the pacing
floor and spread.

**End-to-end** (`tests/repository_stats.rs`, real router and in-memory stores):

- a poll stores stats for a GitHub-hosted record, and `GET /software/{id}` returns them;
- a record hosted elsewhere is never fetched, and has no `repository_stats`;
- a forge error is recorded and the previous numbers are kept;
- a record whose repository changed does not show the old repository's numbers;
- a record polled within the interval is not fetched again, so a restart costs nothing.

**Frontend** (`frontend/src/routes/routes.test.tsx`): the cells show the numbers when
present, and are absent when `repository_stats` is.

---

## 7. Not done, deliberately

- **GitLab**, and any other forge. Sync is GitHub-only too; both would grow together.
- **Commit counts** ("commits" in the handoff's list). GitHub has no cheap count; the
  contributors-stats endpoint is asynchronous and expensive. Last push answers the same question.
- **Reading `X-RateLimit-Remaining`.** The fixed floor keeps the poller well inside the limit;
  if the budget is exhausted anyway, the fetches fail, the errors are recorded, and the old
  numbers stay.
- **A brokered per-curator token.** The poller acts for nobody, so it uses the registry token.
