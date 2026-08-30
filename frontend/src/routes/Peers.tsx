import { useState } from 'react'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { useSession } from '../lib/session'
import { EmptyState, ErrorState, Modal, ProblemJsonError, Skeleton } from '../components/common'
import { RelativeTime } from '../components/chips'

export default function Peers() {
  const { isAdmin, loading: sessionLoading } = useSession()
  const peers = useAsync(() => api.listPeers(), [])
  const suggested = useAsync(() => api.suggestedPeers(), [])
  const [url, setUrl] = useState('')
  const [preview, setPreview] = useState<Awaited<ReturnType<typeof api.previewPeer>>>()
  const [error, setError] = useState<Error>()
  const [confirmRemove, setConfirmRemove] = useState<{ id: string; base: string; count: number }>()

  if (sessionLoading) return <Skeleton rows={4} />
  if (!isAdmin) {
    return (
      <div className="banner warn">
        <h3>Admin role required</h3>
        <p>Peer administration decides whose data this registry caches, so it is admin-only.</p>
      </div>
    )
  }

  return (
    <>
      <div className="page-header">
        <h1>Peer registries</h1>
        <p className="tagline">
          Registries whose records may be cached here and searched alongside ours. Peer data is
          always read-only and lives in its own graph.
        </p>
      </div>

      <section className="card">
        <h2>Add a peer</h2>
        {error && <ProblemJsonError error={error} />}
        <form
          className="inline"
          onSubmit={async (e) => {
            e.preventDefault()
            setError(undefined)
            try {
              setPreview(await api.previewPeer(url.trim()))
            } catch (err) {
              setError(err instanceof Error ? err : new Error(String(err)))
            }
          }}
        >
          <label className="sr-only" htmlFor="peer-url">Peer base URL</label>
          <input
            id="peer-url"
            style={{ maxWidth: 420 }}
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://reg.mumc.nl"
          />
          <button type="submit" className="primary" disabled={!url.trim()}>Look up</button>
        </form>
        <p className="hint">
          The registry fetches the peer's self-description and checks that the base IRI it
          advertises matches this URL — otherwise its cross-links would not resolve.
        </p>
      </section>

      <section className="card flush">
        <h2>Peers</h2>
        {peers.loading && <div style={{ padding: 16 }}><Skeleton rows={2} /></div>}
        {peers.error && <div style={{ padding: 16 }}><ErrorState error={peers.error} onRetry={peers.reload} /></div>}
        {peers.data && peers.data.items.length === 0 && (
          <div style={{ padding: '0 16px 16px' }}>
            <EmptyState title="No peers yet" body="Add one above to cross-link with another estate." />
          </div>
        )}
        {peers.data && peers.data.items.length > 0 && (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">Registry</th>
                  <th scope="col">Base IRI</th>
                  <th scope="col">Last seen</th>
                  <th scope="col">Resolve</th>
                  <th scope="col" className="num">Cached</th>
                  <th scope="col"><span className="sr-only">Actions</span></th>
                </tr>
              </thead>
              <tbody>
                {peers.data.items.map((p) => (
                  <tr key={p.id}>
                    <td>{p.title ?? <span className="muted">untitled</span>}</td>
                    <td><a href={p.base_iri} target="_blank" rel="noreferrer">{p.base_iri}</a></td>
                    <td><RelativeTime iso={p.last_seen_at} /></td>
                    <td>
                      <span className={p.resolve_status === 'ok' ? 'chip ok' : p.resolve_status === 'error' ? 'chip danger' : 'chip'}>
                        {p.resolve_status}
                      </span>
                      {p.last_error && <p className="hint">{p.last_error}</p>}
                    </td>
                    <td className="num">{p.record_count}</td>
                    <td>
                      <button
                        type="button"
                        className="danger"
                        onClick={() => setConfirmRemove({ id: p.id, base: p.base_iri, count: p.record_count })}
                      >
                        Remove
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section className="card flush" style={{ opacity: 0.92 }}>
        <h2>Suggested — peers of peers</h2>
        <p className="hint" style={{ padding: '0 16px' }}>
          Discovered from peers, or announced to us. Never added automatically: the trust
          boundary stays manual.
        </p>
        {suggested.data && suggested.data.items.length === 0 && (
          <p className="muted" style={{ padding: '0 16px 16px' }}>Nothing suggested.</p>
        )}
        {suggested.data && suggested.data.items.length > 0 && (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">Registry</th>
                  <th scope="col">Suggested by</th>
                  <th scope="col"><span className="sr-only">Actions</span></th>
                </tr>
              </thead>
              <tbody>
                {suggested.data.items.map((p) => (
                  <tr key={p.id}>
                    <td>{p.title ?? p.base_iri}</td>
                    <td className="muted">{p.suggested_by ?? '—'}</td>
                    <td>
                      <button
                        type="button"
                        onClick={async () => {
                          setUrl(p.base_iri)
                          setPreview(await api.previewPeer(p.base_iri))
                        }}
                      >
                        Review
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {preview && (
        <Modal title="Trust this registry?" onClose={() => setPreview(undefined)}>
          <p>Adding this peer means caching its records here and searching it when federated search is on.</p>
          <dl className="rail-section">
            <dt>Title</dt><dd>{preview.title ?? <span className="muted">not stated</span>}</dd>
            <dt>Operator</dt><dd>{preview.operator ?? <span className="muted">not stated</span>}</dd>
            <dt>Base IRI</dt><dd className="mono" style={{ fontSize: 12 }}>{preview.base_iri}</dd>
            {preview.peers_of_peer.length > 0 && (
              <>
                <dt>Its own peers</dt>
                <dd>
                  {preview.peers_of_peer.join(', ')}
                  <p className="hint">These will appear as suggestions for you to review — never added automatically.</p>
                </dd>
              </>
            )}
          </dl>
          <div className="actions">
            <button
              type="button"
              className="primary"
              onClick={async () => {
                try {
                  await api.addPeer(preview.base_iri)
                  setPreview(undefined)
                  setUrl('')
                  peers.reload()
                  suggested.reload()
                } catch (err) {
                  setError(err instanceof Error ? err : new Error(String(err)))
                  setPreview(undefined)
                }
              }}
            >
              Add peer
            </button>
            <button type="button" onClick={() => setPreview(undefined)}>Cancel</button>
          </div>
        </Modal>
      )}

      {confirmRemove && (
        <Modal title="Remove peer" onClose={() => setConfirmRemove(undefined)}>
          <p>
            Removing <strong>{confirmRemove.base}</strong> drops its cached graph —{' '}
            <strong>{confirmRemove.count} cached {confirmRemove.count === 1 ? 'record' : 'records'}</strong>{' '}
            will be deleted. Cross-links to its IRIs stay in the graph and simply stop resolving
            locally.
          </p>
          <div className="actions">
            <button
              type="button"
              className="danger"
              onClick={async () => {
                await api.removePeer(confirmRemove.id)
                setConfirmRemove(undefined)
                peers.reload()
              }}
            >
              Remove and drop cache
            </button>
            <button type="button" onClick={() => setConfirmRemove(undefined)}>Cancel</button>
          </div>
        </Modal>
      )}
    </>
  )
}
