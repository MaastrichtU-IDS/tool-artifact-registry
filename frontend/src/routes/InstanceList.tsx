import { Link, useSearchParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { useSession } from '../lib/session'
import { EmptyState, ErrorState, KeysetPager, Skeleton } from '../components/common'
import { HealthDot, OriginChip, RelativeTime } from '../components/chips'

export default function InstanceList() {
  const [params, setParams] = useSearchParams()
  const { isCurator } = useSession()
  const filters = {
    q: params.get('q') ?? undefined,
    software: params.get('software') ?? undefined,
    status: params.get('status') ?? undefined,
    cursor: params.get('cursor') ?? undefined,
  }
  const { data, error, loading, reload } = useAsync(() => api.listInstances(filters), [params.toString()])
  const filtered = Boolean(filters.q || filters.software || filters.status)

  return (
    <section>
      <div className="page-header spread">
        <div>
          <h1>Instances</h1>
          <p className="tagline">
            Deployments. A deployment is where software actually ran — it has an operator, sometimes
            an endpoint, and the runs are attributed to it.
          </p>
        </div>
        {isCurator && <Link className="chip accent" to="/instances/new">Register deployment</Link>}
      </div>

      {loading && <Skeleton rows={6} />}
      {error && <ErrorState error={error} onRetry={reload} />}

      {data && data.items.length === 0 && (
        filtered ? (
          <EmptyState
            title="No deployment matches these filters"
            action={<button type="button" className="primary" onClick={() => setParams(new URLSearchParams())}>Clear filters</button>}
          />
        ) : (
          <EmptyState
            title="No deployments registered yet"
            body="Register one for each place a tool runs: a cluster, a partner site, or a laptop."
            action={isCurator ? <Link className="chip accent" to="/instances/new">Register deployment</Link> : undefined}
          />
        )
      )}

      {data && data.items.length > 0 && (
        <div className="card flush">
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">Deployment</th>
                  <th scope="col">Software</th>
                  <th scope="col">Health</th>
                  <th scope="col">Operator</th>
                  <th scope="col">Endpoint</th>
                  <th scope="col">Last run</th>
                  <th scope="col">Origin</th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((i) => (
                  <tr key={i.iri}>
                    <td>
                      <Link to={`/instances/${i.id}`}>{i.label}</Link>
                      {i.tombstoned && <span className="chip warn" style={{ marginLeft: 6 }}>withdrawn</span>}
                    </td>
                    <td>
                      {i.software && i.software_name ? (
                        <Link to={`/software/${i.software.split('/').pop()}`}>{i.software_name}</Link>
                      ) : (
                        <span className="muted">—</span>
                      )}
                      {i.release_version && <span className="muted"> {i.release_version}</span>}
                      {i.outdated && <span className="chip warn" style={{ marginLeft: 6 }}>outdated</span>}
                    </td>
                    <td><HealthDot health={i.health} /></td>
                    <td>{i.operator?.name ?? <span className="muted">—</span>}</td>
                    <td>
                      {i.endpoint_url ? (
                        <a href={i.endpoint_url} target="_blank" rel="noreferrer">
                          {i.endpoint_url.replace(/^https?:\/\//, '')}
                        </a>
                      ) : (
                        /* No endpoint is normal — a laptop or a batch job — not broken. */
                        <span className="muted">no endpoint · CLI or batch</span>
                      )}
                    </td>
                    <td><RelativeTime iso={i.last_run_at} /></td>
                    <td><OriginChip origin={i.origin} cachedNote={false} /></td>
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
