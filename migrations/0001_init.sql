-- Operational state (spec §5.1). Secrets, cursors and counters live here and are never
-- exposed through the public SPARQL endpoint.

CREATE TABLE IF NOT EXISTS api_tokens (
    id            TEXT PRIMARY KEY,
    prefix        TEXT NOT NULL UNIQUE,
    hash          TEXT NOT NULL,
    instance_iri  TEXT,
    subject_kind  TEXT NOT NULL DEFAULT 'instance',
    scopes        TEXT NOT NULL DEFAULT '',
    label         TEXT,
    created_at    TEXT NOT NULL,
    created_by    TEXT,
    expires_at    TEXT,
    last_used_at  TEXT,
    revoked_at    TEXT
);
CREATE INDEX IF NOT EXISTS api_tokens_instance ON api_tokens(instance_iri);

CREATE TABLE IF NOT EXISTS peers (
    id             TEXT PRIMARY KEY,
    base_iri       TEXT NOT NULL UNIQUE,
    title          TEXT,
    operator       TEXT,
    added_at       TEXT NOT NULL,
    added_by       TEXT,
    last_seen_at   TEXT,
    resolve_status TEXT NOT NULL DEFAULT 'unknown',
    last_error     TEXT,
    record_count   INTEGER NOT NULL DEFAULT 0,
    state          TEXT NOT NULL DEFAULT 'active',   -- active | suggested | dismissed
    suggested_by   TEXT
);

CREATE TABLE IF NOT EXISTS audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          TEXT NOT NULL,
    actor       TEXT,
    actor_kind  TEXT,
    action      TEXT NOT NULL,
    target      TEXT,
    detail      TEXT,
    remote_addr TEXT
);
CREATE INDEX IF NOT EXISTS audit_target ON audit_log(target);

-- Lazy federation bookkeeping (spec §9.4): unknown foreign IRIs queued for resolution,
-- with TTL and exponential backoff, both visible in the peer admin UI.
CREATE TABLE IF NOT EXISTS resolve_queue (
    iri             TEXT PRIMARY KEY,
    peer_id         TEXT,
    status          TEXT NOT NULL DEFAULT 'pending',  -- pending | resolved | failed
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_attempt_at TEXT,
    next_attempt_at TEXT,
    resolved_at     TEXT,
    last_error      TEXT
);
CREATE INDEX IF NOT EXISTS resolve_next ON resolve_queue(status, next_attempt_at);

-- Advertisement idempotency on (run_key, artifact_iri, role) — spec §7.5. A retried CI step
-- must not duplicate lineage.
CREATE TABLE IF NOT EXISTS advertise_idem (
    idem_key     TEXT PRIMARY KEY,
    run_iri      TEXT NOT NULL,
    artifact_iri TEXT NOT NULL,
    role         TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

-- (external_key, instance) -> run IRI, so a second advertisement for the same CI attempt
-- attaches to the same Run.
CREATE TABLE IF NOT EXISTS run_keys (
    external_key TEXT NOT NULL,
    instance_iri TEXT NOT NULL,
    run_iri      TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (external_key, instance_iri)
);

-- Artifacts advertised by external identity (OpenLineage namespace/name), for idempotent
-- re-advertisement of the same dataset (spec §7.6).
CREATE TABLE IF NOT EXISTS artifact_keys (
    external_key TEXT PRIMARY KEY,
    artifact_iri TEXT NOT NULL,
    created_at   TEXT NOT NULL
);
