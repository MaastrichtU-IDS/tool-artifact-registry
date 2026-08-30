import { useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { CopyField, ErrorState, Modal, ProblemJsonError, Skeleton } from '../components/common'
import { RelativeTime } from '../components/chips'

const SCOPES = ['advertise:produce', 'advertise:consume', 'register:software', 'register:instance', 'read:private']

export default function Tokens() {
  const { id = '' } = useParams()
  const inst = useAsync(() => api.getInstance(id), [id])
  const tokens = useAsync(() => api.listTokens(id), [id])
  const [scopes, setScopes] = useState<string[]>(['advertise:produce', 'advertise:consume'])
  const [label, setLabel] = useState('')
  const [expiresIn, setExpiresIn] = useState('')
  const [minted, setMinted] = useState<string>()
  const [error, setError] = useState<Error>()
  const [confirmRevoke, setConfirmRevoke] = useState<{ id: string; prefix: string }>()

  if (inst.loading) return <Skeleton rows={6} />
  if (inst.error) return <ErrorState error={inst.error} onRetry={inst.reload} />
  const instance = inst.data!

  const mint = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(undefined)
    try {
      const res = await api.mintToken(id, {
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
        <h1>Credentials — {instance.label}</h1>
        <p className="tagline">
          How this deployment proves it is itself when it advertises what it produced or consumed.
        </p>
      </div>

      <section className="card">
        <h2>Preferred: workload identity</h2>
        {instance.oidc_client_id ? (
          <>
            <p style={{ marginTop: 0 }}>
              This deployment is bound to the OIDC client <code>{instance.oidc_client_id}</code>
              {instance.oidc_issuer && <> at <code>{instance.oidc_issuer}</code></>}. It can
              fetch its own short-lived token and advertise with that — nothing below is needed.
            </p>
            <pre className="report">{`# in the deployment, at run time
TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" \\
  -d grant_type=client_credentials \\
  "$ISSUER/protocol/openid-connect/token" | jq -r .access_token)

curl -H "Authorization: Bearer $TOKEN" \\
     -H "content-type: application/json" \\
     --data @produced.json \\
     ${window.location.origin}/api/v1/advertise/produced`}</pre>
          </>
        ) : (
          <p style={{ marginTop: 0 }}>
            No identity-provider client is bound to this deployment, so it has to use one of the
            registry tokens below. Binding a client id means the registry never stores a secret
            for this deployment and rotation becomes the provider's job.{' '}
            <Link to={`/instances/${id}/edit`}>Bind an OIDC client →</Link>
          </p>
        )}
      </section>

      <section className="card">
        <h2>Mint a registry token</h2>
        {error && <ProblemJsonError error={error} />}
        <form onSubmit={mint}>
          <div className="field">
            <label htmlFor="label">Label</label>
            <input id="label" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="ci — github actions" />
          </div>
          <div className="field">
            <label>Scopes</label>
            {SCOPES.map((s) => (
              <label key={s} style={{ fontWeight: 400, display: 'flex', gap: 8, alignItems: 'center', marginTop: 4 }}>
                <input
                  type="checkbox"
                  style={{ width: 'auto' }}
                  checked={scopes.includes(s)}
                  onChange={(e) => setScopes(e.target.checked ? [...scopes, s] : scopes.filter((x) => x !== s))}
                />
                <code>{s}</code>
              </label>
            ))}
          </div>
          <div className="field">
            <label htmlFor="expires">Expiry</label>
            <select id="expires" value={expiresIn} onChange={(e) => setExpiresIn(e.target.value)} style={{ width: 220 }}>
              <option value="">Never expires</option>
              <option value="30d">30 days</option>
              <option value="90d">90 days</option>
              <option value="365d">1 year</option>
            </select>
          </div>
          <div className="actions">
            <button type="submit" className="primary" disabled={scopes.length === 0}>Mint token</button>
          </div>
        </form>
      </section>

      <section className="card flush">
        <h2>Existing tokens</h2>
        {tokens.loading && <div style={{ padding: 16 }}><Skeleton rows={3} /></div>}
        {tokens.data && tokens.data.items.length === 0 && (
          <p className="muted" style={{ padding: '0 16px 16px' }}>No tokens have been minted.</p>
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
                  <th scope="col"><span className="sr-only">Actions</span></th>
                </tr>
              </thead>
              <tbody>
                {tokens.data.items.map((t) => (
                  <tr key={t.id} style={t.revoked_at ? { opacity: 0.55 } : undefined}>
                    <td className="mono">tar_{t.prefix}…</td>
                    <td>{t.label ?? <span className="muted">—</span>}</td>
                    <td><ul className="chips">{t.scopes.map((s) => <li key={s}><span className="chip">{s}</span></li>)}</ul></td>
                    <td><RelativeTime iso={t.created_at} /></td>
                    <td>{t.last_used_at ? <RelativeTime iso={t.last_used_at} /> : <span className="muted">never</span>}</td>
                    <td>{t.expires_at ? <RelativeTime iso={t.expires_at} /> : <span className="muted">never</span>}</td>
                    <td>
                      {t.revoked_at ? (
                        <span className="chip">revoked</span>
                      ) : (
                        <button type="button" className="danger" onClick={() => setConfirmRevoke({ id: t.id, prefix: t.prefix })}>
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

      {/* Shown exactly once, and dismissible only by an explicit action, because the value is
          unrecoverable (handoff §5.8, §8). */}
      {minted && (
        <Modal title="Copy this token now" onClose={() => setMinted(undefined)} dismissible={false}>
          <div className="banner warn">
            <p>This value will not be shown again. If you lose it, revoke it and mint another.</p>
          </div>
          <CopyField value={minted} label="new API token" />
          <div className="actions">
            <button type="button" className="primary" onClick={() => setMinted(undefined)}>
              I have copied it
            </button>
          </div>
        </Modal>
      )}

      {confirmRevoke && (
        <Modal title="Revoke token" onClose={() => setConfirmRevoke(undefined)}>
          <p>
            Revoke <code className="mono">tar_{confirmRevoke.prefix}…</code> for{' '}
            <strong>{instance.label}</strong>? Anything still using it stops being able to
            advertise immediately.
          </p>
          <div className="actions">
            <button
              type="button"
              className="danger"
              onClick={async () => {
                await api.revokeToken(id, confirmRevoke.id)
                setConfirmRevoke(undefined)
                tokens.reload()
              }}
            >
              Revoke it
            </button>
            <button type="button" onClick={() => setConfirmRevoke(undefined)}>Cancel</button>
          </div>
        </Modal>
      )}
    </>
  )
}
