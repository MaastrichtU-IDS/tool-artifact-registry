-- Auto-registration: a credential bound to a piece of *software* rather than to one of its
-- deployments.
--
-- The existing binding is token -> instance_iri, which presumes a curator already created the
-- Instance record. That is the curated mode, and it stays. The second mode is the one an
-- operator wants when deployments come and go without a human in the loop: hand the
-- application one key, and let each deployment of it register itself and publish its own
-- details. The key names the software; the deployment record is created on first announcement.
--
-- Nullable and additive, so every token already issued keeps working unchanged.
ALTER TABLE api_tokens ADD COLUMN software_iri TEXT;
CREATE INDEX IF NOT EXISTS api_tokens_software ON api_tokens(software_iri);
