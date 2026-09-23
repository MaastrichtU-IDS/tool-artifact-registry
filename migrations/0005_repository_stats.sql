-- Repository liveness: stars, forks and last push for software hosted on GitHub, refreshed by
-- the forge poller (design note: repository liveness).
--
-- Here rather than in the graph on purpose. The numbers are a day-old copy of somebody else's
-- data; publishing them through /sparql and to peers would present a cache as a fact.
--
-- The counts stay null until a fetch succeeds, so "unknown" is never stored as zero. A failed
-- fetch writes only checked_at and last_error and keeps the previous numbers.
CREATE TABLE IF NOT EXISTS repository_stats (
    software_iri   TEXT PRIMARY KEY,
    repo           TEXT NOT NULL,
    stars          INTEGER,
    forks          INTEGER,
    last_commit_at TEXT,
    fetched_at     TEXT,
    checked_at     TEXT NOT NULL,
    last_error     TEXT
);
