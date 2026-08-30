import { Link, useParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { CopyField, ErrorState, SignalBar, Skeleton, formatDuration } from '../components/common'
import { ArtifactTypeChip, AvailabilityBadge, OriginChip, RelativeTime, RunStatus, shortId } from '../components/chips'
import type { ArtifactRef } from '../lib/types'

export default function RunDetail() {
  const { id = '' } = useParams()
  const { data: run, error, loading, reload } = useAsync(() => api.getRun(id), [id])

  if (loading) return <Skeleton rows={6} />
  if (error) return <ErrorState error={error} onRetry={reload} />
  if (!run) return null

  return (
    <>
      <div className="page-header">
        <div className="spread">
          <h1 className="mono" style={{ fontSize: 22 }}>{run.label ?? `run ${shortId(run.id)}`}</h1>
          <div className="inline">
            <RunStatus status={run.status} />
            <OriginChip origin={run.origin} />
          </div>
        </div>
        <p className="tagline">
          {run.instance && run.instance_label ? (
            <>At <Link to={`/instances/${run.instance.split('/').pop()}`}>{run.instance_label}</Link></>
          ) : (
            'At an unknown deployment'
          )}
          {run.software && run.software_name && (
            <> running <Link to={`/software/${run.software.split('/').pop()}`}>{run.software_name}</Link></>
          )}
          {run.release_version && <> {run.release_version}</>}
        </p>
      </div>

      <SignalBar
        signals={[
          { label: 'Started', value: <RelativeTime iso={run.started_at} />, unknown: !run.started_at },
          { label: 'Ended', value: <RelativeTime iso={run.ended_at} />, unknown: !run.ended_at },
          { label: 'Duration', value: formatDuration(run.duration_seconds), unknown: run.duration_seconds === undefined },
          { label: 'Consumed', value: run.used.length },
          { label: 'Produced', value: run.generated.length },
        ]}
      />

      <div className="two-col">
        <ArtifactColumn title="Consumed" refs={run.used} empty="This run advertised no inputs." />
        <ArtifactColumn title="Produced" refs={run.generated} empty="This run advertised no outputs." />
      </div>

      <section className="card">
        <h2>Identity</h2>
        <dl className="rail-section">
          <dt>Persistent IRI</dt>
          <dd><CopyField value={run.iri} label="run IRI" /></dd>
          {run.external_key && (
            <>
              <dt>External key</dt>
              <dd><CopyField value={run.external_key} label="external run key" /></dd>
            </>
          )}
        </dl>
      </section>

      {run.openlineage_payload != null && (
        <section className="card">
          <h2>OpenLineage payload</h2>
          <p className="hint" style={{ marginTop: 0 }}>
            The event as it arrived, kept verbatim so nothing the mapping does not name is lost.
          </p>
          <details className="disclosure">
            <summary>Show raw payload</summary>
            <pre className="report">{JSON.stringify(run.openlineage_payload, null, 2)}</pre>
          </details>
        </section>
      )}
    </>
  )
}

function ArtifactColumn({ title, refs, empty }: { title: string; refs: ArtifactRef[]; empty: string }) {
  return (
    <section className="card">
      <h2>{title}</h2>
      {refs.length === 0 ? (
        <p className="muted" style={{ margin: 0 }}>{empty}</p>
      ) : (
        <ul style={{ listStyle: 'none', padding: 0, margin: 0 }} className="stack">
          {refs.map((r) => (
            <li key={r.iri}>
              <div className="spread">
                {r.unresolved ? (
                  <a href={r.iri} target="_blank" rel="noreferrer" className="mono" style={{ fontSize: 12.5 }}>{r.iri}</a>
                ) : (
                  <Link to={`/artifacts/${r.iri.split('/').pop()}`}>{r.title ?? r.iri.split('/').pop()}</Link>
                )}
                <OriginChip origin={r.origin} cachedNote={false} />
              </div>
              <div className="row-meta" style={{ marginTop: 4 }}>
                {r.conforms_to && <ArtifactTypeChip type={r.conforms_to} />}
                <AvailabilityBadge availability={r.availability} />
                {r.unresolved && (
                  <span className="chip" title="Stored verbatim; a background worker will fetch a stub. Advertisement never waits on the network.">
                    not resolved yet
                  </span>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
