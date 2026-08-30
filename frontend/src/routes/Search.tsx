import { useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { EmptyState, ErrorState, Skeleton } from '../components/common'
import { OriginChip } from '../components/chips'
import type { SearchHit } from '../lib/types'

const GROUPS: { key: SearchHit['entity_type']; label: string; path: string }[] = [
  { key: 'software', label: 'Software', path: '/software' },
  { key: 'instance', label: 'Instances', path: '/instances' },
  { key: 'artifact', label: 'Artifacts', path: '/artifacts' },
  { key: 'run', label: 'Runs', path: '/runs' },
]

export default function Search() {
  const [params, setParams] = useSearchParams()
  const q = params.get('q') ?? ''
  const [federated, setFederated] = useState(params.get('federated') === 'true')
  const { data, error, loading, reload } = useAsync(
    () => (q ? api.search(q, undefined, federated) : Promise.resolve(undefined)),
    [q, federated],
  )

  return (
    <section>
      <div className="page-header">
        <h1>Search</h1>
        <form
          className="inline"
          role="search"
          onSubmit={(e) => {
            e.preventDefault()
            const value = new FormData(e.currentTarget as HTMLFormElement).get('q') as string
            setParams({ q: value, ...(federated ? { federated: 'true' } : {}) })
          }}
        >
          <label className="sr-only" htmlFor="q">Query</label>
          <input id="q" name="q" defaultValue={q} style={{ maxWidth: 380 }} placeholder="shacl, patients.ttl, gh-actions/…" />
          <button type="submit" className="primary">Search</button>
          <label style={{ fontWeight: 400, display: 'flex', gap: 6, alignItems: 'center', margin: 0 }}>
            <input
              type="checkbox"
              style={{ width: 'auto' }}
              checked={federated}
              onChange={(e) => {
                setFederated(e.target.checked)
                const next = new URLSearchParams(params)
                if (e.target.checked) next.set('federated', 'true')
                else next.delete('federated')
                setParams(next)
              }}
            />
            Search peer registries
          </label>
        </form>
      </div>

      {loading && <Skeleton rows={5} />}
      {error && <ErrorState error={error} onRetry={reload} />}

      {/* Partial results say which peers did not answer, rather than quietly showing fewer
          results (handoff §5.10). */}
      {data?.partial && (
        <div className="banner warn" role="status">
          <h3>Some peers did not answer</h3>
          <ul style={{ margin: '4px 0 0', paddingLeft: 18 }}>
            {data.peers.filter((p) => p.status !== 'ok').map((p) => (
              <li key={p.peer_id}>{p.title ?? p.base_iri} — {p.status}{p.error ? `: ${p.error}` : ''}</li>
            ))}
          </ul>
        </div>
      )}

      {data && data.hits.length === 0 && (
        <EmptyState
          title={`Nothing matches “${q}”`}
          body={federated ? 'Neither here nor at any peer that answered.' : 'Try turning on peer registries.'}
        />
      )}

      {data && GROUPS.map((g) => {
        const hits = data.hits.filter((h) => h.entity_type === g.key)
        if (hits.length === 0) return null
        return (
          <section key={g.key} style={{ marginBottom: 22 }}>
            <h2 style={{ fontSize: 14, textTransform: 'uppercase', letterSpacing: '0.06em', color: 'var(--text-faint)' }}>
              {g.label} <span className="muted">({hits.length})</span>
            </h2>
            {hits.map((h) => (
              <HitRow key={h.iri} hit={h} basePath={g.path} />
            ))}
          </section>
        )
      })}
    </section>
  )
}

function HitRow({ hit, basePath }: { hit: SearchHit; basePath: string }) {
  const local = hit.origin.kind === 'local'
  const id = hit.iri.split('/').pop() ?? ''
  const body = (
    <>
      <div className="spread">
        <h3>{hit.title}</h3>
        <OriginChip origin={hit.origin} interactive={false} />
      </div>
      {hit.snippet && <p>{hit.snippet}</p>}
    </>
  )
  // A federated hit lives at its home registry; it is never presented as one of ours.
  return local ? (
    <Link className="row-card" to={`${basePath}/${id}`}>{body}</Link>
  ) : (
    <a className="row-card" href={hit.iri} target="_blank" rel="noreferrer">{body}</a>
  )
}
