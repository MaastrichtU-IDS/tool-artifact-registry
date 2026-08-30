import { useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { EmptyState, ErrorState, Skeleton } from '../components/common'
import { OriginChip } from '../components/chips'
import type { PeerSearchStatus, SearchHit } from '../lib/types'

/** Propagation adds fields the shared types do not carry yet (they are owned elsewhere).
 *  A federated search now travels past this registry's own peers, so a result has to say
 *  how far it came: a hit from a peer the operator chose to trust is not the same evidence
 *  as one relayed to us by that peer from a registry we have never heard of. */
type Reach = 'local' | 'direct' | 'indirect'
type FedHit = SearchHit & { reach?: Reach; hops?: number; via?: string }
type FedPeer = Omit<PeerSearchStatus, 'status'> & {
  status: 'ok' | 'timeout' | 'error' | 'already_handled' | 'skipped'
  reach?: Reach
  hops?: number
  via?: string
  note?: string
}
type FedTrace = {
  query_id: string
  origin?: string
  registry: string
  max_hops: number
  hops_granted: number
  hops_forwarded: number
  budget_exhausted?: boolean
  path?: string[]
}

const FAILED = new Set(['timeout', 'error'])

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
          results (handoff §5.10). A peer that refused a repeat of this query, or that we
          deliberately did not ask, is *not* a failure and is reported separately below. */}
      {data?.partial && (
        <div className="banner warn" role="status">
          <h3>Some peers did not answer</h3>
          <ul style={{ margin: '4px 0 0', paddingLeft: 18 }}>
            {(data.peers as FedPeer[]).filter((p) => FAILED.has(p.status)).map((p) => (
              <li key={p.peer_id}>
                {p.title ?? p.base_iri} — {p.status}
                {p.reach === 'indirect' && p.via ? ` (reached through ${p.via})` : ''}
                {p.error ? `: ${p.error}` : ''}
              </li>
            ))}
          </ul>
        </div>
      )}

      {data && <Topology peers={data.peers as FedPeer[]} trace={(data as { federation?: FedTrace }).federation} />}

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

/** What the query actually swept. Federated search propagates now, so "which peers answered"
 *  is no longer the same question as "which peers do we trust": the answer can include
 *  registries this operator has never configured, and edges that were deliberately cut to
 *  stop the query looping. Both are stated rather than folded into a hit count. */
function Topology({ peers, trace }: { peers: FedPeer[]; trace?: FedTrace }) {
  if (!trace || peers.length === 0) return null
  const answered = peers.filter((p) => p.status === 'ok')
  const direct = answered.filter((p) => p.reach !== 'indirect').length
  const indirect = answered.length - direct
  const cut = peers.filter((p) => p.status === 'already_handled' || p.status === 'skipped')
  if (answered.length === 0 && cut.length === 0) return null
  return (
    <details className="card" style={{ marginBottom: 16 }}>
      <summary style={{ cursor: 'pointer', fontSize: 13 }}>
        {direct} direct {direct === 1 ? 'peer' : 'peers'}
        {indirect > 0 && `, ${indirect} reached through them`}
        {cut.length > 0 && `, ${cut.length} not asked twice`}
        {trace.budget_exhausted && ` · stopped at the ${trace.max_hops}-hop limit`}
      </summary>
      <ul style={{ margin: '8px 0 0', paddingLeft: 18, fontSize: 13 }}>
        {peers.map((p) => (
          <li key={p.peer_id}>
            {p.title ?? p.base_iri} — <span className="mono">{p.status}</span>
            {p.reach === 'indirect' && p.via ? ` · via ${p.via}` : ''}
            {p.status === 'ok' && ` · ${p.hits} ${p.hits === 1 ? 'hit' : 'hits'}`}
            {p.note && <span className="muted"> — {p.note}</span>}
          </li>
        ))}
      </ul>
      <p className="hint">Query {trace.query_id}, up to {trace.max_hops} hops from {trace.origin ?? trace.registry}.</p>
    </details>
  )
}

/** How far a hit travelled. A directly-configured peer is a trust decision the operator
 *  made; a registry two hops out is not, and the row says so. */
function ReachChip({ hit }: { hit: FedHit }) {
  if (!hit.reach || hit.reach === 'local' || hit.reach === 'direct') return null
  return (
    <span className="chip warn" title={`Relayed to us through ${hit.via ?? 'a peer'} — we do not peer with its home registry`}>
      {hit.hops ?? 2} hops · via {hit.via ?? 'a peer'}
    </span>
  )
}

function HitRow({ hit, basePath }: { hit: FedHit; basePath: string }) {
  const local = hit.origin.kind === 'local'
  const id = hit.iri.split('/').pop() ?? ''
  const body = (
    <>
      <div className="spread">
        <h3>{hit.title}</h3>
        <span className="row-meta">
          <ReachChip hit={hit} />
          <OriginChip origin={hit.origin} interactive={false} />
        </span>
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
