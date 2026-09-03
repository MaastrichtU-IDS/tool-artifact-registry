//! Operational state in SQLite (spec §5.1): credentials, peers, audit, federation cursors.
//!
//! Nothing in here is ever exposed through `/sparql`. Secrets do not belong in a queryable
//! graph.

pub mod federation;

pub mod subscriptions;

use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

#[derive(Clone)]
pub struct Ops {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenRecord {
    pub id: String,
    pub prefix: String,
    pub instance_iri: Option<String>,
    /// Set instead of `instance_iri` for an auto-registration credential: the token names the
    /// software, and each deployment of it registers itself on first announcement.
    pub software_iri: Option<String>,
    pub subject_kind: String,
    pub scopes: Vec<String>,
    pub label: Option<String>,
    pub created_at: String,
    pub created_by: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecord {
    pub id: String,
    pub base_iri: String,
    pub title: Option<String>,
    pub operator: Option<String>,
    pub added_at: String,
    pub last_seen_at: Option<String>,
    pub resolve_status: String,
    pub last_error: Option<String>,
    pub record_count: i64,
    pub state: String,
    pub suggested_by: Option<String>,
}

impl Ops {
    pub async fn open(path: &str) -> Result<Self> {
        let opts = if path == ":memory:" || path == "memory" {
            SqliteConnectOptions::from_str("sqlite::memory:")?
        } else {
            if let Some(dir) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(dir).ok();
            }
            SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?.create_if_missing(true)
        };
        // One connection for :memory: — a pool of them would each get a *different* database.
        let max = if path == ":memory:" || path == "memory" { 1 } else { 5 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max)
            .connect_with(opts)
            .await
            .with_context(|| format!("opening ops db at {path}"))?;
        sqlx::migrate!("./migrations").run(&pool).await.context("running ops migrations")?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // ---------------------------------------------------------------- tokens

    /// Mint a token. The plaintext is returned exactly once — it is never stored (handoff §5.8).
    pub async fn mint_token(
        &self,
        instance_iri: Option<&str>,
        software_iri: Option<&str>,
        subject_kind: &str,
        scopes: &[String],
        label: Option<&str>,
        created_by: Option<&str>,
        ttl: Option<ChronoDuration>,
    ) -> Result<(TokenRecord, String)> {
        let id = uuid::Uuid::now_v7().to_string();
        // Random, not a slice of the UUID: two tokens minted in the same millisecond share
        // the UUIDv7 timestamp prefix, and the prefix is the lookup key.
        let mut prefix_bytes = [0u8; 6];
        OsRng.fill_bytes(&mut prefix_bytes);
        let prefix = hex::encode(prefix_bytes);
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        let secret = hex::encode(secret);
        let plaintext = format!("tar_{prefix}_{secret}");
        let hash = hash_secret(&plaintext)?;
        let now = Utc::now();
        let expires = ttl.map(|d| (now + d).to_rfc3339());
        let scopes_s = scopes.join(" ");
        sqlx::query(
            "INSERT INTO api_tokens (id, prefix, hash, instance_iri, software_iri, subject_kind, scopes, label, created_at, created_by, expires_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&id)
        .bind(&prefix)
        .bind(&hash)
        .bind(instance_iri)
        .bind(software_iri)
        .bind(subject_kind)
        .bind(&scopes_s)
        .bind(label)
        .bind(now.to_rfc3339())
        .bind(created_by)
        .bind(&expires)
        .execute(&self.pool)
        .await?;
        Ok((
            TokenRecord {
                id,
                prefix,
                instance_iri: instance_iri.map(str::to_string),
                software_iri: software_iri.map(str::to_string),
                subject_kind: subject_kind.to_string(),
                scopes: scopes.to_vec(),
                label: label.map(str::to_string),
                created_at: now.to_rfc3339(),
                created_by: created_by.map(str::to_string),
                expires_at: expires,
                last_used_at: None,
                revoked_at: None,
            },
            plaintext,
        ))
    }

    /// Verify a presented opaque token. Returns the record when live, unrevoked, unexpired.
    pub async fn verify_token(&self, presented: &str) -> Result<Option<TokenRecord>> {
        let Some(rest) = presented.strip_prefix("tar_") else { return Ok(None) };
        let Some((prefix, _)) = rest.split_once('_') else { return Ok(None) };
        let row =
            sqlx::query("SELECT * FROM api_tokens WHERE prefix = ?").bind(prefix).fetch_optional(&self.pool).await?;
        let Some(row) = row else { return Ok(None) };
        let hash: String = row.try_get("hash")?;
        if !verify_secret(presented, &hash) {
            return Ok(None);
        }
        let rec = token_from_row(&row)?;
        if rec.revoked_at.is_some() {
            return Ok(None);
        }
        if let Some(exp) = &rec.expires_at {
            if let Ok(t) = DateTime::parse_from_rfc3339(exp) {
                if t < Utc::now() {
                    return Ok(None);
                }
            }
        }
        let _ = sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(&rec.id)
            .execute(&self.pool)
            .await;
        Ok(Some(rec))
    }

    /// Tokens bound to one subject — an instance or a software record. One query, because a
    /// token is bound to exactly one of the two and the caller knows which it is asking about.
    pub async fn list_tokens(&self, subject_iri: &str) -> Result<Vec<TokenRecord>> {
        let rows = sqlx::query(
            "SELECT * FROM api_tokens WHERE instance_iri = ?1 OR software_iri = ?1 ORDER BY created_at DESC",
        )
        .bind(subject_iri)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(token_from_row).collect()
    }

    pub async fn revoke_token(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE api_tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn get_token(&self, id: &str) -> Result<Option<TokenRecord>> {
        let row = sqlx::query("SELECT * FROM api_tokens WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        row.as_ref().map(token_from_row).transpose()
    }

    // ---------------------------------------------------------------- peers

    pub async fn upsert_peer(&self, p: &PeerRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO peers (id, base_iri, title, operator, added_at, last_seen_at, resolve_status, last_error, record_count, state, suggested_by)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(base_iri) DO UPDATE SET
               title=excluded.title, operator=excluded.operator, last_seen_at=excluded.last_seen_at,
               resolve_status=excluded.resolve_status, last_error=excluded.last_error, state=excluded.state",
        )
        .bind(&p.id).bind(&p.base_iri).bind(&p.title).bind(&p.operator)
        .bind(&p.added_at).bind(&p.last_seen_at).bind(&p.resolve_status)
        .bind(&p.last_error).bind(p.record_count).bind(&p.state).bind(&p.suggested_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_peers(&self, state: Option<&str>) -> Result<Vec<PeerRecord>> {
        let rows = match state {
            Some(s) => {
                sqlx::query("SELECT * FROM peers WHERE state = ? ORDER BY added_at DESC")
                    .bind(s)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => sqlx::query("SELECT * FROM peers ORDER BY added_at DESC").fetch_all(&self.pool).await?,
        };
        rows.iter().map(peer_from_row).collect()
    }

    pub async fn get_peer(&self, id: &str) -> Result<Option<PeerRecord>> {
        let row = sqlx::query("SELECT * FROM peers WHERE id = ? OR base_iri = ?")
            .bind(id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(peer_from_row).transpose()
    }

    pub async fn delete_peer(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM peers WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn set_peer_record_count(&self, id: &str, n: i64) -> Result<()> {
        sqlx::query("UPDATE peers SET record_count = ? WHERE id = ?").bind(n).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ------------------------------------------------------- resolve queue

    pub async fn queue_resolve(&self, iri: &str, peer_id: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT INTO resolve_queue (iri, peer_id, status, next_attempt_at)
             VALUES (?,?, 'pending', ?)
             ON CONFLICT(iri) DO NOTHING",
        )
        .bind(iri)
        .bind(peer_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn due_resolves(&self, limit: i64) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT iri FROM resolve_queue
             WHERE status != 'resolved' AND (next_attempt_at IS NULL OR next_attempt_at <= ?)
             ORDER BY next_attempt_at LIMIT ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().filter_map(|r| r.try_get::<String, _>("iri").ok()).collect())
    }

    pub async fn mark_resolved(&self, iri: &str, ttl: ChronoDuration) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE resolve_queue SET status='resolved', attempts=0, resolved_at=?, last_attempt_at=?, next_attempt_at=?, last_error=NULL WHERE iri=?",
        )
        .bind(now.to_rfc3339()).bind(now.to_rfc3339()).bind((now + ttl).to_rfc3339()).bind(iri)
        .execute(&self.pool).await?;
        Ok(())
    }

    /// Exponential backoff, visible in the peer admin UI (spec §9.4).
    pub async fn mark_resolve_failed(&self, iri: &str, err: &str) -> Result<()> {
        let now = Utc::now();
        let row = sqlx::query("SELECT attempts FROM resolve_queue WHERE iri = ?")
            .bind(iri)
            .fetch_optional(&self.pool)
            .await?;
        let attempts: i64 = row.and_then(|r| r.try_get("attempts").ok()).unwrap_or(0) + 1;
        let backoff_secs = 60i64.saturating_mul(1 << attempts.min(10));
        sqlx::query(
            "UPDATE resolve_queue SET status='failed', attempts=?, last_attempt_at=?, next_attempt_at=?, last_error=? WHERE iri=?",
        )
        .bind(attempts).bind(now.to_rfc3339())
        .bind((now + ChronoDuration::seconds(backoff_secs)).to_rfc3339())
        .bind(err).bind(iri)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn resolve_status(&self, iri: &str) -> Result<Option<(String, Option<String>)>> {
        let row = sqlx::query("SELECT status, last_error FROM resolve_queue WHERE iri = ?")
            .bind(iri)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| {
            (
                r.try_get::<String, _>("status").unwrap_or_default(),
                r.try_get::<Option<String>, _>("last_error").ok().flatten(),
            )
        }))
    }

    // -------------------------------------------------- runs & idempotency

    pub async fn run_for_key(&self, external_key: &str, instance_iri: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT run_iri FROM run_keys WHERE external_key = ? AND instance_iri = ?")
            .bind(external_key)
            .bind(instance_iri)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.try_get("run_iri").ok()))
    }

    pub async fn remember_run(&self, external_key: &str, instance_iri: &str, run_iri: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO run_keys (external_key, instance_iri, run_iri, created_at) VALUES (?,?,?,?)",
        )
        .bind(external_key)
        .bind(instance_iri)
        .bind(run_iri)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn artifact_for_key(&self, external_key: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT artifact_iri FROM artifact_keys WHERE external_key = ?")
            .bind(external_key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.try_get("artifact_iri").ok()))
    }

    pub async fn remember_artifact(&self, external_key: &str, artifact_iri: &str) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO artifact_keys (external_key, artifact_iri, created_at) VALUES (?,?,?)")
            .bind(external_key)
            .bind(artifact_iri)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// `true` when this `(run, artifact, role)` triple is new. Idempotency for §7.5.
    pub async fn claim_advertisement(&self, run_iri: &str, artifact_iri: &str, role: &str) -> Result<bool> {
        let key = format!("{run_iri}|{artifact_iri}|{role}");
        let r = sqlx::query("INSERT OR IGNORE INTO advertise_idem (idem_key, run_iri, artifact_iri, role, created_at) VALUES (?,?,?,?,?)")
            .bind(&key).bind(run_iri).bind(artifact_iri).bind(role).bind(Utc::now().to_rfc3339())
            .execute(&self.pool).await?;
        Ok(r.rows_affected() > 0)
    }

    // ----------------------------------------------------------------- audit

    pub async fn audit(
        &self,
        actor: Option<&str>,
        actor_kind: &str,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
        remote: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_log (at, actor, actor_kind, action, target, detail, remote_addr) VALUES (?,?,?,?,?,?,?)",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .bind(actor_kind)
        .bind(action)
        .bind(target)
        .bind(detail)
        .bind(remote)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recent_audit(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        let rows =
            sqlx::query("SELECT * FROM audit_log ORDER BY id DESC LIMIT ?").bind(limit).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "at": r.try_get::<String, _>("at").unwrap_or_default(),
                    "actor": r.try_get::<Option<String>, _>("actor").ok().flatten(),
                    "actor_kind": r.try_get::<Option<String>, _>("actor_kind").ok().flatten(),
                    "action": r.try_get::<String, _>("action").unwrap_or_default(),
                    "target": r.try_get::<Option<String>, _>("target").ok().flatten(),
                    "detail": r.try_get::<Option<String>, _>("detail").ok().flatten(),
                })
            })
            .collect())
    }
}

fn token_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<TokenRecord> {
    let scopes: String = row.try_get("scopes")?;
    Ok(TokenRecord {
        id: row.try_get("id")?,
        prefix: row.try_get("prefix")?,
        instance_iri: row.try_get("instance_iri")?,
        software_iri: row.try_get("software_iri")?,
        subject_kind: row.try_get("subject_kind")?,
        scopes: scopes.split_whitespace().map(str::to_string).collect(),
        label: row.try_get("label")?,
        created_at: row.try_get("created_at")?,
        created_by: row.try_get("created_by")?,
        expires_at: row.try_get("expires_at")?,
        last_used_at: row.try_get("last_used_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

fn peer_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<PeerRecord> {
    Ok(PeerRecord {
        id: row.try_get("id")?,
        base_iri: row.try_get("base_iri")?,
        title: row.try_get("title")?,
        operator: row.try_get("operator")?,
        added_at: row.try_get("added_at")?,
        last_seen_at: row.try_get("last_seen_at")?,
        resolve_status: row.try_get("resolve_status")?,
        last_error: row.try_get("last_error")?,
        record_count: row.try_get("record_count")?,
        state: row.try_get("state")?,
        suggested_by: row.try_get("suggested_by")?,
    })
}

fn hash_secret(secret: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
        .to_string())
}

fn verify_secret(secret: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default().verify_password(secret.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mints_and_verifies_a_token_once() {
        let ops = Ops::open(":memory:").await.unwrap();
        let (rec, plaintext) = ops
            .mint_token(
                Some("https://r/instance/1"),
                None,
                "instance",
                &["advertise:produce".into()],
                Some("ci"),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(plaintext.starts_with("tar_"));
        let verified = ops.verify_token(&plaintext).await.unwrap().expect("verifies");
        assert_eq!(verified.id, rec.id);
        assert_eq!(verified.instance_iri.as_deref(), Some("https://r/instance/1"));

        assert!(ops.verify_token("tar_deadbeef_nope").await.unwrap().is_none());
        ops.revoke_token(&rec.id).await.unwrap();
        assert!(ops.verify_token(&plaintext).await.unwrap().is_none(), "revoked token must not verify");
    }

    #[tokio::test]
    async fn advertisement_claims_are_idempotent() {
        let ops = Ops::open(":memory:").await.unwrap();
        assert!(ops.claim_advertisement("run/1", "art/1", "produced").await.unwrap());
        assert!(!ops.claim_advertisement("run/1", "art/1", "produced").await.unwrap());
        assert!(ops.claim_advertisement("run/1", "art/1", "consumed").await.unwrap());
    }
}
