-- Federated search propagation bookkeeping (spec §9.6, federation-propagation note).
--
-- A federated search carries a query id that travels with it across the peer mesh. Every
-- registry that handles a leg of that query claims the id here first. A second arrival of
-- the same id — which is what a cycle A->B->C->A looks like from the inside — is refused
-- with an explicit `already_handled` answer instead of being handled again.
--
-- Rows are short-lived on purpose: the id only has to outlive one in-flight query, so the
-- table is swept by `expires_at` (and hard-capped) on every claim. It is not an audit
-- surface; the audit log is.
CREATE TABLE IF NOT EXISTS federated_queries (
    query_id      TEXT PRIMARY KEY,
    first_seen_at TEXT NOT NULL,
    expires_at    TEXT NOT NULL,
    -- Base IRI of the registry that minted the query, as claimed by the request. Advisory
    -- only: it is never trusted for authorisation, just reported back for tracing.
    origin        TEXT,
    -- Base IRI of the peer that handed us this leg, for tracing a storm back to its source.
    received_from TEXT,
    -- How many times we refused a repeat of this id. A cycle shows up here as > 0.
    repeat_count  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS federated_queries_expiry ON federated_queries(expires_at);
