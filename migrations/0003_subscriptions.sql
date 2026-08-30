-- Artifact subscriptions (spec §7.10, `docs/specs/2026-08-31-artifact-subscriptions.md`).
--
-- A subscription is an Instance saying "tell me when an artifact like this appears". It is
-- operational state, exactly like an API token: it belongs to an Instance, it is managed by
-- that Instance's credential, and it never enters the RDF graph or the SPARQL endpoint.
--
-- Two things live here. `subscriptions` is the standing interest plus the health of its
-- delivery channel. `subscription_deliveries` is the queue: one row per (subscription,
-- artifact, role) match, written synchronously on the advertisement path — a SQLite insert,
-- never a socket — and drained afterwards by a background worker for webhook subscribers, or
-- read directly from a cursor by subscribers that cannot accept an inbound connection.

CREATE TABLE IF NOT EXISTS subscriptions (
    id                   TEXT PRIMARY KEY,
    -- Owning deployment. §8.3's rule applied to reads: a subscription belongs to exactly one
    -- Instance and is managed by that Instance's credential, a curator, or an admin.
    instance_iri         TEXT NOT NULL,
    label                TEXT,
    -- The filter, as JSON. Kept opaque to SQL on purpose: matching is a pure Rust function
    -- over a candidate struct (`ops::subscriptions::matches`) so it is unit-testable without
    -- a database, and so the semantics cannot drift between the writer and the reader.
    filter               TEXT NOT NULL DEFAULT '{}',
    -- NULL means a pull-only subscription: the tool has no inbound endpoint (a CLI, a laptop,
    -- a batch job — `deployable = false` software is the normal case, not the exception) and
    -- polls `/deliveries` instead. This is the default.
    webhook_url          TEXT,
    -- Recoverable, unlike an API token hash, because HMAC signing needs the key itself. It is
    -- shown once at creation and rotatable; it never leaves SQLite except in that one
    -- response. See the design note for why this asymmetry with `api_tokens` is unavoidable.
    webhook_secret       TEXT,
    enabled              INTEGER NOT NULL DEFAULT 1,
    -- active | suspended. `suspended` is what stops the registry hammering a dead endpoint:
    -- after enough consecutive failures the webhook is not attempted at all until the owner
    -- resumes it. The pull path keeps working while suspended, which is the right degradation.
    delivery_state       TEXT NOT NULL DEFAULT 'active',
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    -- How far a pulling subscriber has acknowledged. Lets the UI show a truthful backlog for
    -- a subscriber the registry cannot reach out to.
    cursor_seq           INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    created_by           TEXT,
    updated_at           TEXT,
    last_match_at        TEXT,
    last_success_at      TEXT,
    last_error           TEXT,
    last_error_at        TEXT,
    last_polled_at       TEXT
);
CREATE INDEX IF NOT EXISTS subscriptions_instance ON subscriptions(instance_iri);

CREATE TABLE IF NOT EXISTS subscription_deliveries (
    -- Monotonic, and therefore the pull cursor. AUTOINCREMENT so a deleted row can never let
    -- a later row reuse a sequence number a client has already passed.
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    id              TEXT NOT NULL UNIQUE,
    subscription_id TEXT NOT NULL,
    artifact_iri    TEXT NOT NULL,
    run_iri         TEXT,
    role            TEXT NOT NULL,
    -- The notification body, frozen at match time so the webhook and the pull path deliver
    -- byte-identical content and neither has to re-read the graph on the delivery path.
    payload         TEXT NOT NULL,
    matched_at      TEXT NOT NULL,
    -- pending | delivered | failed | dead. `dead` is a delivery that exhausted its attempts;
    -- it is never retried and stays visible to the owner rather than being swept away.
    status          TEXT NOT NULL DEFAULT 'pending',
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_attempt_at TEXT,
    next_attempt_at TEXT,
    last_error      TEXT,
    last_status     INTEGER,
    delivered_at    TEXT,
    FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
);

-- Idempotency, mirroring `advertise_idem`: re-advertising the same artifact for the same run
-- and role must not notify twice.
CREATE UNIQUE INDEX IF NOT EXISTS subscription_deliveries_unique
    ON subscription_deliveries(subscription_id, artifact_iri, role);
CREATE INDEX IF NOT EXISTS subscription_deliveries_due
    ON subscription_deliveries(status, next_attempt_at);
CREATE INDEX IF NOT EXISTS subscription_deliveries_sub
    ON subscription_deliveries(subscription_id, seq);
