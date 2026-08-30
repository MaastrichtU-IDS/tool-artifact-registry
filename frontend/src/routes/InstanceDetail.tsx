import { Link, useParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { useSession } from '../lib/session'
import {
  CiteBlock, CopyField, ErrorState, FairDownloads, ForeignNotice, SignalBar, Skeleton, Tombstone,
  formatDuration,
} from '../components/common'
import {
  ArtifactTypeChip, AvailabilityBadge, HealthDot, OriginChip, RelativeTime, RunStatus,
} from '../components/chips'

/** Same two-column shell as Software, so the two read as one system — but the content makes
 *  the difference obvious: an Instance is concrete, has an endpoint, and has runs. */
export default function InstanceDetail() {
  const { id = '' } = useParams()
  const { isCurator, who } = useSession()
  const inst = useAsync(() => api.getInstance(id), [id])
  const runs = useAsync(() => api.instanceRuns(id, { limit: '15' }), [id])
  const artifacts = useAsync(() => api.instanceArtifacts(id, { limit: '15' }), [id])

  if (inst.loading) return <Skeleton rows={8} />
  if (inst.error) return <ErrorState error={inst.error} onRetry={inst.reload} />
  if (!inst.data) return null
  const i = inst.data
  const foreign = i.origin.kind === 'peer'
  const mayManage = isCurator || who?.instance === i.iri

  return (
    <>
      {i.tombstoned && <Tombstone what="deployment record" />}
      {foreign && <ForeignNotice homeIri={i.iri} />}
      {i.outdated && (
        <div className="banner warn">
          <h3>Running an older release</h3>
          <p>This deployment runs {i.release_version}; the latest registered release is {i.latest_version}.</p>
        </div>
      )}

      <div className="detail">
        <div>
          <div className="page-header">
            <div className="spread">
              <h1>{i.label}</h1>
              <div className="inline">
                <HealthDot health={i.health} />
                <OriginChip origin={i.origin} />
                {mayManage && !foreign && <Link className="chip" to={`/instances/${i.id}/edit`}>Edit</Link>}
                {mayManage && !foreign && <Link className="chip" to={`/instances/${i.id}/tokens`}>Credentials</Link>}
              </div>
            </div>
            <p className="tagline">
              A deployment of{' '}
              {i.software && i.software_name ? (
                <Link to={`/software/${i.software.split('/').pop()}`}>{i.software_name}</Link>
              ) : (
                'unregistered software'
              )}
              {i.release_version && <> {i.release_version}</>}
              {i.operator?.name && <> · operated by {i.operator.name}</>}
            </p>
            <ul className="chips">
              {i.endpoint_url && <li><a className="chip accent" href={i.endpoint_url} target="_blank" rel="noreferrer">Open endpoint ↗</a></li>}
              {i.endpoint_description && <li><a className="chip" href={i.endpoint_description} target="_blank" rel="noreferrer">OpenAPI ↗</a></li>}
            </ul>
          </div>

          <SignalBar
            signals={[
              { label: 'Last run', value: <RelativeTime iso={i.last_run_at} />, unknown: !i.last_run_at },
              { label: 'Runs / 30d', value: i.runs_30d },
              { label: 'Failures / 30d', value: i.failures_30d },
              { label: 'Artifacts', value: i.artifact_count },
            ]}
          />

          <section className="card flush">
            <h2>Runs</h2>
            {runs.loading && <div style={{ padding: 16 }}><Skeleton rows={3} /></div>}
            {runs.data && runs.data.items.length === 0 && (
              <p className="muted" style={{ padding: '0 16px 16px' }}>
                No run has been advertised by this deployment yet.
              </p>
            )}
            {runs.data && runs.data.items.length > 0 && (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Run</th>
                      <th scope="col">Started</th>
                      <th scope="col">Status</th>
                      <th scope="col">Duration</th>
                      <th scope="col">In → out</th>
                      <th scope="col">External key</th>
                    </tr>
                  </thead>
                  <tbody>
                    {runs.data.items.map((r) => (
                      <tr key={r.iri}>
                        <td><Link to={`/runs/${r.id}`} className="mono">{r.id.slice(0, 8)}</Link></td>
                        <td><RelativeTime iso={r.started_at} /></td>
                        <td><RunStatus status={r.status} /></td>
                        <td>{formatDuration(r.duration_seconds)}</td>
                        <td className="nowrap">{r.used_count} → {r.generated_count}</td>
                        <td className="mono muted">{r.external_key ?? '—'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          <section className="card flush">
            <h2>Artifacts produced here</h2>
            {artifacts.loading && <div style={{ padding: 16 }}><Skeleton rows={3} /></div>}
            {artifacts.data && artifacts.data.items.length === 0 && (
              <p className="muted" style={{ padding: '0 16px 16px' }}>Nothing advertised yet.</p>
            )}
            {artifacts.data && artifacts.data.items.length > 0 && (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Artifact</th>
                      <th scope="col">Type</th>
                      <th scope="col">Availability</th>
                      <th scope="col">Issued</th>
                    </tr>
                  </thead>
                  <tbody>
                    {artifacts.data.items.map((a) => (
                      <tr key={a.iri}>
                        <td><Link to={`/artifacts/${a.id}`}>{a.title ?? a.id.slice(0, 8)}</Link></td>
                        <td>{a.conforms_to ? <ArtifactTypeChip type={a.conforms_to} /> : <span className="muted">—</span>}</td>
                        <td><AvailabilityBadge availability={a.availability} /></td>
                        <td><RelativeTime iso={a.issued} /></td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          {i.capability && (
            <section className="card">
              <h2>Capability narrowed at this deployment</h2>
              <p className="hint" style={{ marginTop: 0 }}>
                This deployment declares a narrower capability than the software as a whole.
              </p>
              <div className="two-col">
                <div>
                  <h3 style={{ fontSize: 13, margin: '0 0 8px' }}>Consumes</h3>
                  <ul className="chips">{i.capability.consumes.map((t) => <li key={t.iri}><ArtifactTypeChip type={t} /></li>)}</ul>
                </div>
                <div>
                  <h3 style={{ fontSize: 13, margin: '0 0 8px' }}>Produces</h3>
                  <ul className="chips">{i.capability.produces.map((t) => <li key={t.iri}><ArtifactTypeChip type={t} /></li>)}</ul>
                </div>
              </div>
            </section>
          )}
        </div>

        <aside className="rail">
          <section className="card rail-section">
            <h2>Deployment</h2>
            <dl>
              <dt>Endpoint</dt>
              <dd>
                {i.endpoint_url ? (
                  <a href={i.endpoint_url} target="_blank" rel="noreferrer">{i.endpoint_url}</a>
                ) : (
                  <span className="muted">none — runs from a CLI or batch job</span>
                )}
              </dd>
              <dt>Availability</dt>
              <dd>{i.availability ?? <span className="muted">—</span>}</dd>
              <dt>Jurisdiction</dt>
              <dd>{i.jurisdiction ?? <span className="muted">—</span>}</dd>
              <dt>Operator</dt>
              <dd>{i.operator?.name ?? <span className="muted">—</span>}</dd>
              <dt>Home registry</dt>
              <dd className="mono" style={{ fontSize: 12 }}>{i.home_registry ?? '—'}</dd>
            </dl>
          </section>

          <section className="card rail-section">
            <h2>How it authenticates</h2>
            {i.oidc_client_id ? (
              <>
                <p className="hint" style={{ marginTop: 0 }}>
                  This deployment advertises with a token from its own identity provider. No
                  secret for it is stored here.
                </p>
                <dl>
                  <dt>OIDC client</dt>
                  <dd><code className="mono">{i.oidc_client_id}</code></dd>
                  {i.oidc_issuer && (
                    <>
                      <dt>Issuer</dt>
                      <dd className="mono" style={{ fontSize: 12 }}>{i.oidc_issuer}</dd>
                    </>
                  )}
                  {i.allowed_scopes.length > 0 && (
                    <>
                      <dt>Scopes</dt>
                      <dd><ul className="chips">{i.allowed_scopes.map((s) => <li key={s}><span className="chip">{s}</span></li>)}</ul></dd>
                    </>
                  )}
                </dl>
              </>
            ) : (
              <p className="hint" style={{ marginTop: 0 }}>
                No workload identity is bound. This deployment can only advertise with a
                registry API token.{' '}
                {mayManage && <Link to={`/instances/${i.id}/edit`}>Bind an OIDC client →</Link>}
              </p>
            )}
            {mayManage && (
              <p className="hint">
                <Link to={`/instances/${i.id}/tokens`}>
                  {i.token_count} API {i.token_count === 1 ? 'token' : 'tokens'} →
                </Link>
              </p>
            )}
          </section>

          <CiteBlock iri={i.iri} title={i.label} version={i.release_version} />

          <section className="card rail-section">
            <h2>Persistent IRI</h2>
            <CopyField value={i.iri} label="persistent IRI" />
          </section>

          <FairDownloads iri={i.iri} />
        </aside>
      </div>
    </>
  )
}
