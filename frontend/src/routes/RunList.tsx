import { Link, useSearchParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { EmptyState, ErrorState, KeysetPager, Skeleton, formatDuration } from '../components/common'
import { OriginChip, RelativeTime, RunStatus, shortId } from '../components/chips'

export default function RunList() {
  const [params, setParams] = useSearchParams()
  const filters = {
    q: params.get('q') ?? undefined,
    software: params.get('software') ?? undefined,
    instance: params.get('instance') ?? undefined,
    status: params.get('status') ?? undefined,
    cursor: params.get('cursor') ?? undefined,
  }
  const { data, error, loading, reload } = useAsync(() => api.listRuns(filters), [params.toString()])
  const filtered = Object.entries(filters).some(([k, v]) => k !== 'cursor' && v)

  return (
    <section>
      <div className="page-header">
        <h1>Runs</h1>
        <p className="tagline">Every execution a deployment advertised, and what it moved.</p>
      </div>

      <div className="inline" style={{ marginBottom: 14 }}>
        <label className="sr-only" htmlFor="status">Status</label>
        <select
          id="status"
          style={{ width: 180 }}
          value={filters.status ?? ''}
          onChange={(e) => {
            const next = new URLSearchParams(params)
            if (e.target.value) next.set('status', e.target.value)
            else next.delete('status')
            next.delete('cursor')
            setParams(next)
          }}
        >
          <option value="">Any status</option>
          <option value="success">success</option>
          <option value="failed">failed</option>
          <option value="running">running</option>
          <option value="aborted">aborted</option>
        </select>
        {filtered && <button type="button" className="link" onClick={() => setParams(new URLSearchParams())}>Clear filters</button>}
      </div>

      {loading && <Skeleton rows={6} />}
      {error && <ErrorState error={error} onRetry={reload} />}

      {data && data.items.length === 0 && (
        <EmptyState
          title={filtered ? 'No run matches these filters' : 'No runs advertised yet'}
          body={filtered ? undefined : 'A deployment advertises a run when it produces or consumes an artifact.'}
          action={filtered ? <button type="button" className="primary" onClick={() => setParams(new URLSearchParams())}>Clear filters</button> : undefined}
        />
      )}

      {data && data.items.length > 0 && (
        <div className="card flush">
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">Run</th>
                  <th scope="col">Deployment</th>
                  <th scope="col">Software</th>
                  <th scope="col">Started</th>
                  <th scope="col">Status</th>
                  <th scope="col">Duration</th>
                  <th scope="col">In → out</th>
                  <th scope="col">Origin</th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((r) => (
                  <tr key={r.iri}>
                    <td><Link to={`/runs/${r.id}`} className="mono">{shortId(r.id)}</Link></td>
                    <td>
                      {r.instance && r.instance_label ? (
                        <Link to={`/instances/${r.instance.split('/').pop()}`}>{r.instance_label}</Link>
                      ) : (
                        <span className="muted">—</span>
                      )}
                    </td>
                    <td>
                      {r.software && r.software_name ? (
                        <Link to={`/software/${r.software.split('/').pop()}`}>{r.software_name}</Link>
                      ) : (
                        <span className="muted">—</span>
                      )}
                    </td>
                    <td><RelativeTime iso={r.started_at} /></td>
                    <td><RunStatus status={r.status} /></td>
                    <td>{formatDuration(r.duration_seconds)}</td>
                    <td className="nowrap">{r.used_count} → {r.generated_count}</td>
                    <td><OriginChip origin={r.origin} cachedNote={false} /></td>
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
          onCursor={(c) => {
            const next = new URLSearchParams(params)
            if (c) next.set('cursor', c)
            else next.delete('cursor')
            setParams(next)
          }}
        />
      )}
    </section>
  )
}
