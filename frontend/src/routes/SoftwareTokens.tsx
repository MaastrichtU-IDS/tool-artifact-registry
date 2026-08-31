import { useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { CopyField, ErrorState, Modal, ProblemJsonError, Skeleton } from '../components/common'
import { RelativeTime } from '../components/chips'
import { useSession } from '../lib/session'

const SCOPES = ['register:instance', 'advertise:produce', 'advertise:consume', 'read:private']

/**
 * Auto-registration keys — the second way a deployment gets into the registry.
 *
 * The first way is a curator filling in the deployment form. That is right when deployments
 * are few and long-lived. It is wrong when they come and go on their own, which is why this
 * exists: one key belongs to the *application*, and every deployment of it registers itself and
 * publishes its own endpoint, version and health endpoint.
 */
export default function SoftwareTokens() {
  const { id = '' } = useParams()
  const { isCurator, loading: sessionLoading } = useSession()
  const sw = useAsync(() => api.getSoftware(id), [id])
  const tokens = useAsync(() => (isCurator ? api.listSoftwareTokens(id) : Promise.resolve({ items: [] })), [id, isCurator])
  const [scopes, setScopes] = useState<string[]>(['register:instance', 'advertise:produce', 'advertise:consume'])
  const [label, setLabel] = useState('')
  const [expiresIn, setExpiresIn] = useState('')
  const [minted, setMinted] = useState<string>()
  const [error, setError] = useState<Error>()
  const [confirmRevoke, setConfirmRevoke] = useState<{ id: string; prefix: string }>()

  if (sessionLoading || sw.loading) return <Skeleton rows={6} />
  if (sw.error) return <ErrorState error={sw.error} onRetry={sw.reload} />
  const software = sw.data!

  if (!isCurator) {
    return (
      <div className="banner warn">
        <h3>Curator role required</h3>
        <p>
          An auto-registration key is a standing permission to add deployment records, so only a
          curator or an administrator may issue one.
        </p>
        <Link to={`/software/${id}`}>Back to {software.name}</Link>
      </div>
    )
  }

  const mint = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(undefined)
    try {
      const res = await api.mintSoftwareToken(id, {
        scopes,
        label: label || undefined,
        expires_in: expiresIn || undefined,
      })
      setMinted(res.token)
      setLabel('')
      tokens.reload()
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    }
  }

  return (
    <>
      <div className="page-header">
        <h1>Auto-registration — {software.name}</h1>
        <p className="tagline">
          A credential that lets a deployment of {software.name} register itself, instead of
          waiting for a curator to create its record.
        </p>
      </div>

      <section className="card">
        <h2>How a deployment uses it</h2>
        <p style={{ marginTop: 0 }}>
          The deployment sends everything it knows about itself, and repeats the call whenever
          something changes. The first call creates its record; every call after that updates the
          same one. <code>instance_key</code> is how two deployments sharing this key stay apart
          — a hostname, a cluster, a namespace.
        </p>
        <pre className="report">{`curl -X PUT ${window.location.origin}/api/v1/instances/self \\
  -H "authorization: Bearer $TAR_TOKEN" \\
  -H "content-type: application/json" \\
  -d '{
    "label": "${software.name} on prod",
    "instance_key": "prod-cluster",
    "endpoint_url": "https://${software.name}.example.org",
    "health_endpoint": "https://${software.name}.example.org/healthz",
    "availability": "restricted"
  }'`}</pre>
      </section>

      {software.registration_clients.length > 0 && (
        <section className="card">
          <h2>Or through your identity provider</h2>
          <p style={{ marginTop: 0 }}>
            These OIDC clients may already register deployments of {software.name} with a token
            from their own issuer — no key from here needed, and nothing long-lived to leak.
          </p>
          <ul className="chips">
            {software.registration_clients.map((c) => (
              <li key={c}>
                <span className="chip">{c}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="card">
        <h2>Issue a key</h2>
        {error && <ProblemJsonError error={error} />}
        <form onSubmit={mint}>
          <div className="field">
            <label htmlFor="label">Label</label>
            <input
              id="label"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="What holds this key — a Helm chart, a CI pipeline"
              style={{ maxWidth: 420 }}
            />
          </div>
          <div className="field">
            <span className="label">Scopes</span>
            {SCOPES.map((s) => (
              <label key={s} className="check">
                <input
                  type="checkbox"
                  checked={scopes.includes(s)}
                  onChange={(e) =>
                    setScopes(e.target.checked ? [...scopes, s] : scopes.filter((v) => v !== s))
                  }
                />
                <code>{s}</code>
              </label>
            ))}
            <p className="hint">
              <code>register:instance</code> is what makes this key an auto-registration key. The
              advertise scopes come with it because a deployment that registers itself and then
              cannot say what it produced would need a second key immediately.
            </p>
          </div>
          <div className="field">
            <label htmlFor="expires">Expiry</label>
            <select
              id="expires"
              value={expiresIn}
              onChange={(e) => setExpiresIn(e.target.value)}
              style={{ width: 220 }}
            >
              <option value="">Never expires</option>
              <option value="30d">30 days</option>
              <option value="90d">90 days</option>
              <option value="365d">1 year</option>
            </select>
          </div>
          <div className="actions">
            <button type="submit" className="primary" disabled={scopes.length === 0}>
              Mint key
            </button>
          </div>
        </form>
      </section>

      <section className="card flush">
        <h2>Existing keys</h2>
        {tokens.loading && (
          <div style={{ padding: 16 }}>
            <Skeleton rows={3} />
          </div>
        )}
        {tokens.data && tokens.data.items.length === 0 && (
          <p className="muted" style={{ padding: '0 16px 16px' }}>
            No auto-registration keys have been issued. Deployments of {software.name} are
            created by a curator.
          </p>
        )}
        {tokens.data && tokens.data.items.length > 0 && (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">Prefix</th>
                  <th scope="col">Label</th>
                  <th scope="col">Scopes</th>
                  <th scope="col">Created</th>
                  <th scope="col">Last used</th>
                  <th scope="col">Expires</th>
                  <th scope="col">
                    <span className="sr-only">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {tokens.data.items.map((t) => (
                  <tr key={t.id} style={t.revoked_at ? { opacity: 0.55 } : undefined}>
                    <td className="mono">tar_{t.prefix}…</td>
                    <td>{t.label ?? <span className="muted">—</span>}</td>
                    <td>
                      <ul className="chips">
                        {t.scopes.map((s) => (
                          <li key={s}>
                            <span className="chip">{s}</span>
                          </li>
                        ))}
                      </ul>
                    </td>
                    <td><RelativeTime iso={t.created_at} /></td>
                    <td>
                      {t.last_used_at ? <RelativeTime iso={t.last_used_at} /> : <span className="muted">never</span>}
                    </td>
                    <td>
                      {t.expires_at ? <RelativeTime iso={t.expires_at} /> : <span className="muted">never</span>}
                    </td>
                    <td>
                      {t.revoked_at ? (
                        <span className="chip">revoked</span>
                      ) : (
                        <button
                          type="button"
                          className="danger"
                          onClick={() => setConfirmRevoke({ id: t.id, prefix: t.prefix })}
                        >
                          Revoke
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {minted && (
        <Modal title="Copy this key now" onClose={() => setMinted(undefined)} dismissible={false}>
          <div className="banner warn">
            <p>This value will not be shown again. If you lose it, revoke it and mint another.</p>
          </div>
          <CopyField value={minted} label="new auto-registration key" />
          <div className="actions">
            <button type="button" className="primary" onClick={() => setMinted(undefined)}>
              I have copied it
            </button>
          </div>
        </Modal>
      )}

      {confirmRevoke && (
        <Modal title="Revoke this key?" onClose={() => setConfirmRevoke(undefined)}>
          <p>
            Deployments still using <code>tar_{confirmRevoke.prefix}…</code> stop being able to
            update their records. The records themselves stay.
          </p>
          <div className="actions">
            <button type="button" onClick={() => setConfirmRevoke(undefined)}>
              Cancel
            </button>
            <button
              type="button"
              className="danger"
              onClick={async () => {
                await api.revokeSoftwareToken(id, confirmRevoke.id)
                setConfirmRevoke(undefined)
                tokens.reload()
              }}
            >
              Revoke
            </button>
          </div>
        </Modal>
      )}
    </>
  )
}
