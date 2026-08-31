import { Link, useSearchParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { EmptyState, ErrorState, KeysetPager, Skeleton } from '../components/common'
import { ArtifactTypeChip, AvailabilityBadge, OriginChip, RelativeTime, shortId } from '../components/chips'

export default function ArtifactList() {
  const [params, setParams] = useSearchParams()
  const filters = {
    q: params.get('q') ?? undefined,
    conforms_to: params.get('conforms_to') ?? undefined,
    availability: params.get('availability') ?? undefined,
    keyword: params.get('keyword') ?? undefined,
    software: params.get('software') ?? undefined,
    instance: params.get('instance') ?? undefined,
    cursor: params.get('cursor') ?? undefined,
  }
  const { data, error, loading, reload } = useAsync(() => api.listArtifacts(filters), [params.toString()])
  // The registry's own keyword list. Shown as one row of toggles rather than a dropdown: it is
  // seven items, and seeing them is most of the point — a dropdown would hide the vocabulary
  // from the person who most needs to learn it.
  const keywords = useAsync(() => api.keywords(), [])
  const filtered = Object.entries(filters).some(([k, v]) => k !== 'cursor' && v)

  const setParam = (key: string, value?: string) => {
    const next = new URLSearchParams(params)
    if (value) next.set(key, value)
    else next.delete(key)
    next.delete('cursor')
    setParams(next)
  }

  return (
    <section>
      <div className="page-header">
        <h1>Artifacts</h1>
        <p className="tagline">
          What deployments actually produced and consumed — with the licence, the checksum and
          the way to get at it, or the way to ask.
        </p>
      </div>

      <div className="inline" style={{ marginBottom: 14 }}>
        <label className="sr-only" htmlFor="availability">Availability</label>
        <select
          id="availability"
          style={{ width: 200 }}
          value={filters.availability ?? ''}
          onChange={(e) => setParam('availability', e.target.value || undefined)}
        >
          <option value="">Any availability</option>
          <option value="public">public</option>
          <option value="restricted">restricted</option>
          <option value="embargoed">embargoed</option>
          <option value="metadata-only">metadata-only</option>
        </select>
        {filtered && <button type="button" className="link" onClick={() => setParams(new URLSearchParams())}>Clear filters</button>}
      </div>

      {keywords.data && keywords.data.items.length > 0 && (
        <div className="inline" style={{ marginBottom: 14, flexWrap: 'wrap', gap: 6 }}>
          <span className="muted" style={{ fontSize: 12 }}>Keyword:</span>
          {keywords.data.items.map((k) => {
            const active = filters.keyword === k.slug
            return (
              <button
                key={k.slug}
                type="button"
                className={active ? 'chip accent' : 'chip'}
                title={k.definition}
                aria-pressed={active}
                onClick={() => setParam('keyword', active ? undefined : k.slug)}
              >
                {k.label}
              </button>
            )
          })}
        </div>
      )}

      {filters.conforms_to && (
        <div className="banner info">
          <p>Artifacts conforming to <code>{filters.conforms_to}</code>.</p>
        </div>
      )}

      {loading && <Skeleton rows={6} />}
      {error && <ErrorState error={error} onRetry={reload} />}

      {data && data.items.length === 0 && (
        filtered ? (
          <EmptyState
            title="No artifact matches these filters"
            action={<button type="button" className="primary" onClick={() => setParams(new URLSearchParams())}>Clear filters</button>}
          />
        ) : (
          <EmptyState
            title="No artifacts advertised yet"
            body="Artifacts appear here when a deployment advertises what it produced or consumed."
          />
        )
      )}

      {data && data.items.length > 0 && (
        <div className="card flush">
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">Artifact</th>
                  <th scope="col">Type</th>
                  <th scope="col">Availability</th>
                  <th scope="col">Produced by</th>
                  <th scope="col">Issued</th>
                  <th scope="col">Origin</th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((a) => (
                  <tr key={a.iri}>
                    <td><Link to={`/artifacts/${a.id}`}>{a.title ?? shortId(a.id)}</Link></td>
                    <td>{a.conforms_to ? <ArtifactTypeChip type={a.conforms_to} /> : <span className="muted">—</span>}</td>
                    <td><AvailabilityBadge availability={a.availability} /></td>
                    <td>
                      {a.generated_by_run?.instance_label ? (
                        <Link to={`/instances/${a.generated_by_run.instance?.split('/').pop()}`}>
                          {a.generated_by_run.instance_label}
                        </Link>
                      ) : (
                        <span className="muted">—</span>
                      )}
                    </td>
                    <td><RelativeTime iso={a.issued} /></td>
                    <td><OriginChip origin={a.origin} cachedNote={false} /></td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {data && (
        <KeysetPager
          cursor={filters.cursor}
          nextCursor={data.next_cursor}
          count={data.items.length}
          total={data.total}
          onCursor={(c) => setParam('cursor', c)}
        />
      )}
    </section>
  )
}
