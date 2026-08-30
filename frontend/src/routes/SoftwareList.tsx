import { Link, useSearchParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { useSession } from '../lib/session'
import { EmptyState, ErrorState, KeysetPager, Skeleton } from '../components/common'
import { ArtifactTypeChip, KindChip, LicenseChip, OriginChip } from '../components/chips'

const FACET_LABELS: Record<string, string> = {
  license: 'Licence',
  kind: 'Kind',
  edam_topic: 'EDAM topic',
}

export default function SoftwareList() {
  const [params, setParams] = useSearchParams()
  const { isCurator, isAdmin } = useSession()
  const filters = {
    q: params.get('q') ?? undefined,
    license: params.get('license') ?? undefined,
    kind: params.get('kind') ?? undefined,
    edam_topic: params.get('edam_topic') ?? undefined,
    consumes: params.get('consumes') ?? undefined,
    produces: params.get('produces') ?? undefined,
    cursor: params.get('cursor') ?? undefined,
  }
  const { data, error, loading, reload } = useAsync(() => api.listSoftware(filters), [params.toString()])
  const filtered = Boolean(filters.q || filters.license || filters.kind || filters.edam_topic || filters.consumes || filters.produces)

  const setParam = (key: string, value?: string) => {
    const next = new URLSearchParams(params)
    if (value) next.set(key, value)
    else next.delete(key)
    next.delete('cursor')
    setParams(next)
  }

  return (
    <div className="list-layout">
      <aside aria-label="Filters">
        <div className="spread">
          <strong style={{ fontSize: 14 }}>Filters</strong>
          {filtered && <button type="button" className="link" onClick={() => setParams(new URLSearchParams())}>Clear</button>}
        </div>
        {(data?.facets ?? []).map((facet) => (
          <div className="facet" key={facet.name}>
            <h3>{FACET_LABELS[facet.name] ?? facet.name}</h3>
            <ul>
              {facet.values.map((v) => {
                const selected = params.get(facet.name) === v.value
                return (
                  <li key={v.value}>
                    <button
                      type="button"
                      className={selected ? 'link selected' : 'link'}
                      aria-pressed={selected}
                      onClick={() => setParam(facet.name, selected ? undefined : v.value)}
                    >
                      {v.label ?? v.value}
                    </button>
                    <span className="count">{v.count}</span>
                  </li>
                )
              })}
            </ul>
          </div>
        ))}
      </aside>

      <section>
        <div className="page-header spread">
          <div>
            <h1>Software</h1>
            <p className="tagline">
              What each tool is, who is responsible for it, and what it can consume and produce.
            </p>
          </div>
          {isCurator && <Link className="chip accent" to="/software/new">Register software</Link>}
        </div>

        {(filters.produces || filters.consumes) && (
          <div className="banner info">
            <p>
              Showing software that {filters.produces ? 'produces' : 'consumes'}{' '}
              <code>{filters.produces ?? filters.consumes}</code>.{' '}
              <button type="button" className="link" onClick={() => { setParam('produces', undefined); setParam('consumes', undefined) }}>
                Clear
              </button>
            </p>
          </div>
        )}

        {loading && <Skeleton rows={6} />}
        {error && <ErrorState error={error} onRetry={reload} />}

        {data && data.items.length === 0 && (
          filtered ? (
            <EmptyState
              title="No software matches these filters"
              body="Try removing one, or search across every entity type."
              action={<button type="button" className="primary" onClick={() => setParams(new URLSearchParams())}>Clear filters</button>}
            />
          ) : (
            <EmptyState
              title="No software registered yet"
              body={
                isAdmin
                  ? 'Register the first tool, or run `tar seed --from ids-examples` to load the IDS estate.'
                  : 'Nothing has been registered in this registry yet.'
              }
              action={isCurator ? <Link className="chip accent" to="/software/new">Register software</Link> : undefined}
            />
          )
        )}

        {data?.items.map((sw) => (
          <Link className={sw.image ? 'row-card with-thumb' : 'row-card'} to={`/software/${sw.id}`} key={sw.iri}>
            {sw.image && (
              <img
                className="thumb"
                src={sw.image}
                alt=""
                loading="lazy"
                /* A dead image URL must not leave a broken-image glyph in the row. */
                onError={(e) => { (e.currentTarget as HTMLImageElement).style.display = 'none' }}
              />
            )}
            <div className="row-body">
            <div className="spread">
              <h3>{sw.name}</h3>
              <OriginChip origin={sw.origin} interactive={false} />
            </div>
            {sw.tagline && <p>{sw.tagline}</p>}
            <div className="row-meta">
              <KindChip kind={sw.kind} interactive={false} />
              <LicenseChip license={sw.license} interactive={false} />
              <span className="chip plain">{sw.instance_count} {sw.instance_count === 1 ? 'instance' : 'instances'}</span>
              {sw.runs_30d > 0 && <span className="chip plain">{sw.runs_30d} runs/30d</span>}
              {sw.edam_topics.slice(0, 2).map((t) => <ArtifactTypeChip key={t.iri} type={t} interactive={false} />)}
              {sw.tombstoned && <span className="chip warn">withdrawn</span>}
            </div>
            </div>
          </Link>
        ))}

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
    </div>
  )
}
