import { Link, useParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { useSession } from '../lib/session'
import {
  CiteBlock, CommandBlock, EmptyState, ErrorState, FairDownloads, ForeignNotice,
  SignalBar, Skeleton, Tombstone, formatBytes,
} from '../components/common'
import {
  ArtifactTypeChip, HealthDot, KindChip, LicenseChip, OriginChip, RelativeTime,
} from '../components/chips'
import { Markdown } from '../components/Markdown'
import { ApiDocs } from '../components/ApiDocs'
import { useState } from 'react'
import { ApiError } from '../lib/api'
import type { Software as SoftwareRecord } from '../lib/types'

/**
 * Lead with what only this registry can say: what the tool consumes and produces, and which
 * deployments exist (handoff §2.1, §4.1). Description and metadata sit below and to the side.
 *
 * There is deliberately no run list here. Runs belong to Instances (spec D5); the signal bar
 * shows the roll-up and links out.
 */
export default function SoftwareDetail() {
  const { id = '' } = useParams()
  const { isCurator } = useSession()
  const sw = useAsync(() => api.getSoftware(id), [id])
  const releases = useAsync(() => api.listReleases(id), [id])
  const instances = useAsync(() => api.listInstances({ software: id }), [id])

  if (sw.loading) return <Skeleton rows={8} />
  if (sw.error) return <ErrorState error={sw.error} onRetry={sw.reload} />
  if (!sw.data) return null
  const s = sw.data
  const foreign = s.origin.kind === 'peer'
  const cap = s.capability

  return (
    <>
      {s.tombstoned && <Tombstone what="software record" />}
      {foreign && <ForeignNotice homeIri={s.iri} />}

      <div className="detail">
        <div>
          <div className="page-header">
            <div className="spread">
              <h1>{s.name}</h1>
              <div className="inline">
                <OriginChip origin={s.origin} />
                {/* Edit affordances are absent for foreign records, never disabled. */}
                {isCurator && !foreign && !s.tombstoned && (
                  <Link className="chip" to={`/software/${s.id}/edit`}>Edit</Link>
                )}
                {/* Only offered where a deployment could exist: an auto-registration key for
                    software that cannot be hosted would create records nothing can fill in. */}
                {isCurator && !foreign && !s.tombstoned && s.deployable && (
                  <Link className="chip" to={`/software/${s.id}/tokens`}>Auto-registration</Link>
                )}
              </div>
            </div>
            {s.tagline && <p className="tagline">{s.tagline}</p>}
            <ul className="chips">
              {s.code_repository && <li><a className="chip" href={s.code_repository} target="_blank" rel="noreferrer">Repository ↗</a></li>}
              {s.documentation && <li><a className="chip" href={s.documentation} target="_blank" rel="noreferrer">Docs ↗</a></li>}
              {s.homepage && s.homepage !== s.code_repository && <li><a className="chip" href={s.homepage} target="_blank" rel="noreferrer">Website ↗</a></li>}
              {s.download_url && <li><a className="chip accent" href={s.download_url} target="_blank" rel="noreferrer">⬇ Download ↗</a></li>}
            </ul>
          </div>

          <SignalBar
            signals={[
              { label: 'Instances', value: <Link to={`/instances?software=${s.id}`}>{s.instance_count}</Link> },
              { label: 'Runs / 30d', value: <Link to={`/runs?software=${s.id}`}>{s.runs_30d}</Link> },
              { label: 'Releases', value: s.release_count },
              { label: 'Latest', value: s.latest_release?.version ?? '—', unknown: !s.latest_release },
            ]}
          />

          {(s.latest_release?.install_command || (s.latest_release?.downloads?.length ?? 0) > 0 || s.download_url) && (
            <section className="card">
              <h2>Get it</h2>
              {s.latest_release?.install_command && (
                <CommandBlock command={s.latest_release.install_command} />
              )}
              {s.latest_release?.image_digest && (
                <p className="hint">Digest {s.latest_release.image_digest}</p>
              )}
              {(s.latest_release?.downloads?.length ?? 0) > 0 && (
                <>
                  <p className="hint" style={{ marginTop: s.latest_release?.install_command ? 12 : 0 }}>
                    {s.latest_release!.version} builds
                  </p>
                  <ul className="chips">
                    {s.latest_release!.downloads!.map((d) => (
                      <li key={d.url}>
                        <a className="chip accent" href={d.url}>
                          ⬇ {d.platform ?? d.label ?? d.url.split('/').pop()}
                          {d.byte_size ? <span className="muted"> {formatBytes(d.byte_size)}</span> : null}
                        </a>
                      </li>
                    ))}
                  </ul>
                </>
              )}
              {!s.latest_release?.install_command &&
                (s.latest_release?.downloads?.length ?? 0) === 0 &&
                s.download_url && (
                  <p style={{ margin: 0 }}>
                    <a href={s.download_url} target="_blank" rel="noreferrer">Downloads and releases ↗</a>
                  </p>
                )}
            </section>
          )}

          <section className="card">
            <h2>Consumes and produces</h2>
            {cap && (cap.consumes.length > 0 || cap.produces.length > 0) ? (
              <div className="two-col">
                <div>
                  <h3 style={{ fontSize: 13, margin: '0 0 8px' }}>Consumes</h3>
                  {cap.consumes.length ? (
                    <ul className="chips">
                      {cap.consumes.map((t) => <li key={t.iri}><ArtifactTypeChip type={t} /></li>)}
                    </ul>
                  ) : (
                    <p className="muted">Nothing declared.</p>
                  )}
                </div>
                <div>
                  <h3 style={{ fontSize: 13, margin: '0 0 8px' }}>Produces</h3>
                  {cap.produces.length ? (
                    <>
                      <ul className="chips">
                        {cap.produces.map((t) => <li key={t.iri}><ArtifactTypeChip type={t} /></li>)}
                      </ul>
                      <p className="hint">
                        {cap.produces.map((t) => (
                          <Link key={t.iri} to={`/software?consumes=${encodeURIComponent(t.iri)}`} style={{ marginRight: 12 }}>
                            What consumes {t.label}? →
                          </Link>
                        ))}
                      </p>
                    </>
                  ) : (
                    <p className="muted">Nothing declared.</p>
                  )}
                </div>
              </div>
            ) : (
              /* The absence of a capability is information; the block is never hidden. */
              <EmptyState
                title="No capability declared"
                body="Until this tool says what it consumes and produces, it cannot be matched to others, and discovery has to wait for something to actually run."
                action={isCurator && !foreign ? <Link className="chip accent" to={`/software/${s.id}/edit`}>Declare capability</Link> : undefined}
              />
            )}
          </section>

          <section className="card flush">
            <h2>{s.deployable ? 'Deployments' : 'Installations'}</h2>
            {!s.deployable && (
              <p className="hint" style={{ padding: '0 16px' }}>
                This software runs on a machine rather than being hosted, so it has installations
                rather than deployments and none of them has an endpoint. Runs are still
                attributed to the installation that performed them.
              </p>
            )}
            {instances.loading && <div style={{ padding: 16 }}><Skeleton rows={3} /></div>}
            {instances.data && instances.data.items.length === 0 && (
              <div style={{ padding: '0 16px 16px' }}>
                <p className="muted">
                  No {s.deployable ? 'deployment' : 'installation'} of this software is registered.
                </p>
              </div>
            )}
            {instances.data && instances.data.items.length > 0 && (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">{s.deployable ? 'Deployment' : 'Installation'}</th>
                      <th scope="col">Release</th>
                      <th scope="col">Health</th>
                      <th scope="col">Operator</th>
                      <th scope="col">Last run</th>
                      <th scope="col">Origin</th>
                    </tr>
                  </thead>
                  <tbody>
                    {instances.data.items.map((i) => (
                      <tr key={i.iri}>
                        <td><Link to={`/instances/${i.id}`}>{i.label}</Link></td>
                        <td>
                          {i.release_version ?? <span className="muted">—</span>}
                          {i.outdated && (
                            <span className="chip warn" style={{ marginLeft: 6 }} title={`Latest is ${i.latest_version}`}>
                              outdated
                            </span>
                          )}
                        </td>
                        <td><HealthDot health={i.health} /></td>
                        <td>{i.operator?.name ?? <span className="muted">—</span>}</td>
                        <td><RelativeTime iso={i.last_run_at} /></td>
                        <td><OriginChip origin={i.origin} cachedNote={false} /></td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          {(s.image || s.screenshots.length > 0) && (
            <section className="card">
              <h2>Screenshots</h2>
              <div className="shots">
                {[s.image, ...s.screenshots].filter(Boolean).map((src) => (
                  <a key={src} href={src} target="_blank" rel="noreferrer">
                    <img
                      src={src}
                      alt={`${s.name} screenshot`}
                      loading="lazy"
                      onError={(e) => {
                        const fig = (e.currentTarget as HTMLImageElement).closest('a')
                        if (fig) (fig as HTMLElement).style.display = 'none'
                      }}
                    />
                  </a>
                ))}
              </div>
              <p className="hint">
                Images are referenced, never stored here — the registry holds pointers, not bytes.
              </p>
            </section>
          )}

          {(s.readme || s.description) && (
            <section className="card">
              <h2>{s.readme ? 'Readme' : 'Description'}</h2>
              {s.description && s.readme && (
                <p style={{ marginTop: 0 }}>{s.description}</p>
              )}
              {s.readme ? (
                <Markdown source={s.readme} baseUrl={s.readme_base_url} />
              ) : (
                <p style={{ margin: 0 }}>{s.description}</p>
              )}
            </section>
          )}

          {s.api_docs?.length > 0 && (
            <section className="card">
              <h2>API</h2>
              <p className="hint" style={{ marginTop: 0 }}>
                Machine-readable descriptions of this software's API. The document is fetched
                through this registry, because most are served without CORS headers and a
                browser cannot read them directly.
              </p>
              <ApiDocs softwareId={s.id} docs={s.api_docs} />
            </section>
          )}

          {s.publications.length > 0 && (
            <section className="card">
              <h2>Publications</h2>
              <ul>
                {s.publications.map((p) => (
                  <li key={p}><a href={p} target="_blank" rel="noreferrer">{p}</a></li>
                ))}
              </ul>
            </section>
          )}
        </div>

        <aside className="rail">
          <section className="card rail-section">
            <h2>Metadata</h2>
            <ul className="chips" style={{ marginBottom: 10 }}>
              <li><LicenseChip license={s.license} /></li>
              <li><KindChip kind={s.kind} /></li>
              {s.maturity && <li><span className="chip">{s.maturity}</span></li>}
            </ul>
            <dl>
              {s.topics.length > 0 && (
                <>
                  <dt>Topics</dt>
                  <dd>
                    <ul className="chips">
                      {s.topics.map((t) => (
                        <li key={t.iri}><ArtifactTypeChip type={t} to={`/software?topic=${encodeURIComponent(t.iri)}`} /></li>
                      ))}
                    </ul>
                  </dd>
                </>
              )}
              {s.keywords.length > 0 && (
                <>
                  <dt>Keywords</dt>
                  <dd>
                    <ul className="chips">
                      {s.keywords.map((k) => (
                        <li key={k}><Link className="chip plain" to={`/software?keyword=${encodeURIComponent(k)}`}>{k}</Link></li>
                      ))}
                    </ul>
                  </dd>
                </>
              )}
              <dt>Added</dt>
              <dd><RelativeTime iso={s.created} /></dd>
              <dt>Updated</dt>
              <dd><RelativeTime iso={s.modified} /></dd>
            </dl>
          </section>

          <CiteBlock iri={s.iri} title={s.name} version={s.latest_release?.version} />

          {releases.data && releases.data.items.length > 0 && (
            <section className="card rail-section">
              <h2>Releases</h2>
              <dl>
                {releases.data.items.map((r) => (
                  <div key={r.iri} className="spread" style={{ padding: '3px 0' }}>
                    <span>{r.version}</span>
                    <span className="muted"><RelativeTime iso={r.date_published} /></span>
                  </div>
                ))}
              </dl>
            </section>
          )}

          {(s.publisher || s.contact) && (
            <section className="card rail-section">
              <h2>People</h2>
              <dl>
                {s.publisher && (
                  <>
                    <dt>Publisher</dt>
                    <dd>
                      {s.publisher.homepage ? (
                        <a href={s.publisher.homepage} target="_blank" rel="noreferrer">{s.publisher.name}</a>
                      ) : (
                        s.publisher.name
                      )}
                      {s.publisher.identifier && (
                        <> · <a href={s.publisher.identifier} target="_blank" rel="noreferrer">
                          {s.publisher.identifier.includes('ror.org') ? 'ROR' : 'ORCID'}
                        </a></>
                      )}
                    </dd>
                  </>
                )}
                {s.contact && (
                  <>
                    <dt>Contact</dt>
                    <dd>{s.contact.name}{s.contact.email && <> · <a href={`mailto:${s.contact.email}`}>email</a></>}</dd>
                  </>
                )}
              </dl>
            </section>
          )}

          {s.sync && <SyncPanel software={s} onDone={sw.reload} canSync={isCurator && !foreign} />}

          <FairDownloads iri={s.iri} biotools={`/api/v1/software/${s.id}/export/biotools`} />
        </aside>
      </div>
    </>
  )
}


/** What the repository controls, when it last ran, and a way to run it now. */
function SyncPanel({
  software, onDone, canSync,
}: { software: SoftwareRecord; onDone: () => void; canSync: boolean }) {
  const sync = software.sync!
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<{ changed: string[]; releases_added: string[]; skipped: string[] }>()
  const [error, setError] = useState<string>()

  return (
    <section className="card rail-section">
      <h2>Repository sync</h2>
      <p className="hint" style={{ marginTop: 0 }}>
        These fields are kept in step with{' '}
        <a href={`https://github.com/${sync.repo}`} target="_blank" rel="noreferrer">{sync.repo}</a>{' '}
        and will be overwritten on the next sync. Everything else on this page is edited here and
        is never touched.
      </p>
      <ul className="chips" style={{ marginBottom: 10 }}>
        {sync.fields.map((f) => <li key={f}><span className="chip accent">{f}</span></li>)}
      </ul>
      <dl>
        <dt>Last run</dt>
        <dd>
          {sync.last_status === 'never' ? (
            <span className="muted">never</span>
          ) : (
            <>
              <RelativeTime iso={sync.last_synced_at} />{' '}
              <span className={sync.last_status === 'ok' ? 'chip ok' : 'chip danger'}>
                <span className={sync.last_status === 'ok' ? 'dot' : 'dot square'} aria-hidden="true" />
                {sync.last_status}
              </span>
            </>
          )}
        </dd>
        {sync.last_error && (
          <>
            <dt>Error</dt>
            <dd className="muted">{sync.last_error}</dd>
          </>
        )}
        {!sync.enabled && (
          <>
            <dt>Status</dt>
            <dd><span className="chip warn">disabled</span></dd>
          </>
        )}
      </dl>

      {canSync && (
        <div className="actions">
          <button
            type="button"
            disabled={busy || !sync.enabled}
            onClick={async () => {
              setBusy(true)
              setError(undefined)
              setResult(undefined)
              try {
                setResult(await api.syncSoftware(software.id))
                onDone()
              } catch (e) {
                setError(e instanceof ApiError ? (e.problem.detail ?? e.problem.title) : String(e))
              } finally {
                setBusy(false)
              }
            }}
          >
            {busy ? 'Syncing…' : 'Sync now'}
          </button>
        </div>
      )}
      {result && (
        <p className="hint">
          {result.changed.length === 0 && result.releases_added.length === 0
            ? 'Already up to date — nothing changed.'
            : `Updated ${result.changed.join(', ')}${
                result.releases_added.length ? `; added ${result.releases_added.join(', ')}` : ''
              }.`}
          {result.skipped.length > 0 && ` Skipped: ${result.skipped.join('; ')}.`}
        </p>
      )}
      {error && <p className="field-error" role="alert">{error}</p>}
    </section>
  )
}
