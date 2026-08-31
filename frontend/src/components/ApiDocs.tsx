import { useEffect, useRef, useState } from 'react'
import type { ApiDoc } from '../lib/types'
import { api } from '../lib/api'

/** Human names for the formats. Kept beside the type rather than in each caller. */
export const API_FORMAT_LABEL: Record<string, string> = {
  openapi: 'OpenAPI',
  asyncapi: 'AsyncAPI',
  graphql: 'GraphQL schema',
  'sparql-service-description': 'SPARQL service description',
  ols4: 'OLS4-compatible API',
  postman: 'Postman collection',
  other: 'API description',
}

interface Operation {
  method: string
  path: string
  summary?: string
  description?: string
  tags: string[]
  deprecated?: boolean
  parameters: { name: string; in: string; required?: boolean; description?: string }[]
  responses: { code: string; description?: string }[]
}

const METHODS = ['get', 'post', 'put', 'patch', 'delete', 'head', 'options', 'trace']

/**
 * Read an OpenAPI document into a flat operation list.
 *
 * Deliberately hand-rolled and forgiving rather than a validating parser: the job is to show a
 * reader what the API offers, and a document that is 95% well-formed should render 95% of its
 * operations instead of one error. Anything unrecognised is skipped, never guessed at.
 */
function parseOpenApi(doc: any): { title?: string; version?: string; servers: string[]; ops: Operation[] } {
  const ops: Operation[] = []
  const paths = doc?.paths && typeof doc.paths === 'object' ? doc.paths : {}
  for (const [path, item] of Object.entries<any>(paths)) {
    if (!item || typeof item !== 'object') continue
    // Parameters declared on the path apply to every operation under it.
    const shared = Array.isArray(item.parameters) ? item.parameters : []
    for (const method of METHODS) {
      const op = item[method]
      if (!op || typeof op !== 'object') continue
      const params = [...shared, ...(Array.isArray(op.parameters) ? op.parameters : [])]
      ops.push({
        method: method.toUpperCase(),
        path,
        summary: op.summary,
        description: op.description,
        tags: Array.isArray(op.tags) ? op.tags.filter((t: unknown) => typeof t === 'string') : [],
        deprecated: op.deprecated === true,
        parameters: params
          .filter((p: any) => p && typeof p.name === 'string')
          .map((p: any) => ({ name: p.name, in: p.in ?? 'query', required: p.required, description: p.description })),
        responses: Object.entries<any>(op.responses ?? {}).map(([code, r]) => ({
          code,
          description: r?.description,
        })),
      })
    }
  }
  const servers = Array.isArray(doc?.servers)
    ? doc.servers.map((s: any) => s?.url).filter((u: unknown): u is string => typeof u === 'string')
    : []
  return { title: doc?.info?.title, version: doc?.info?.version, servers, ops }
}

function methodClass(method: string): string {
  if (method === 'GET' || method === 'HEAD') return 'ok'
  if (method === 'DELETE') return 'bad'
  if (method === 'POST' || method === 'PUT' || method === 'PATCH') return 'warn'
  return ''
}

function OperationRow({ op }: { op: Operation }) {
  const [open, setOpen] = useState(false)
  const hasDetail = op.parameters.length > 0 || op.responses.length > 0 || !!op.description
  return (
    <div className="api-op">
      <button
        type="button"
        className="api-op-head"
        onClick={() => hasDetail && setOpen(!open)}
        aria-expanded={hasDetail ? open : undefined}
        disabled={!hasDetail}
      >
        <span className={`chip ${methodClass(op.method)}`}>{op.method}</span>
        <code className="api-op-path">{op.path}</code>
        {op.deprecated && <span className="chip bad">deprecated</span>}
        <span className="muted api-op-summary">{op.summary}</span>
      </button>
      {open && (
        <div className="api-op-body">
          {op.description && <p className="muted">{op.description}</p>}
          {op.parameters.length > 0 && (
            <>
              <h5>Parameters</h5>
              <ul className="api-params">
                {op.parameters.map((p) => (
                  <li key={`${p.in}:${p.name}`}>
                    <code>{p.name}</code> <span className="muted">in {p.in}</span>
                    {p.required && <span className="chip warn">required</span>}
                    {p.description && <div className="muted">{p.description}</div>}
                  </li>
                ))}
              </ul>
            </>
          )}
          {op.responses.length > 0 && (
            <>
              <h5>Responses</h5>
              <ul className="api-params">
                {op.responses.map((r) => (
                  <li key={r.code}>
                    <code>{r.code}</code> <span className="muted">{r.description}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </div>
  )
}

/**
 * One API description, rendered.
 *
 * The document is fetched through the registry rather than directly: almost no `openapi.json`
 * is served with CORS headers, so a browser-side fetch fails on most records — and fails
 * invisibly, which is worse than not offering the feature. Only OpenAPI gets an operation
 * list; every other format is a link, because inventing a renderer for a document we cannot
 * interpret would be pretending to know more than we do.
 */
export function ApiDocView({ softwareId, doc, index }: { softwareId: string; doc: ApiDoc; index: number }) {
  const [state, setState] = useState<{ loading: boolean; error?: string; parsed?: ReturnType<typeof parseOpenApi> }>({
    loading: false,
  })
  const [expanded, setExpanded] = useState(false)
  const label = API_FORMAT_LABEL[doc.format] ?? API_FORMAT_LABEL.other
  const renderable = doc.format === 'openapi'

  // Deps are only the things that identify *which* document to fetch. Putting the fetch's own
  // state in here made the effect re-run the moment it set `loading`, and the cleanup then
  // cancelled the request it had just started — so the result was always discarded and the
  // panel sat on "Fetching…" forever. A ref tracks what has already been requested instead.
  const requested = useRef<string>()
  useEffect(() => {
    if (!expanded || !renderable) return
    const key = `${softwareId}#${index}`
    if (requested.current === key) return
    requested.current = key
    let cancelled = false
    setState({ loading: true })
    api
      .raw(`/software/${softwareId}/api-doc?n=${index}`)
      .then((text) => {
        if (cancelled) return
        try {
          setState({ loading: false, parsed: parseOpenApi(JSON.parse(text)) })
        } catch {
          // YAML is common and we do not carry a YAML parser. Say so plainly and keep the link
          // rather than showing an empty operation list that reads as "this API has nothing".
          setState({
            loading: false,
            error: 'This document is not JSON — probably YAML, which this page cannot read. Open it directly.',
          })
        }
      })
      .catch((e: Error) => !cancelled && setState({ loading: false, error: e.message }))
    return () => {
      cancelled = true
    }
  }, [expanded, renderable, softwareId, index])

  const byTag = new Map<string, Operation[]>()
  for (const op of state.parsed?.ops ?? []) {
    const tag = op.tags[0] ?? 'Operations'
    if (!byTag.has(tag)) byTag.set(tag, [])
    byTag.get(tag)!.push(op)
  }

  return (
    <section className="api-doc card">
      <header className="api-doc-head">
        <div>
          <strong>{doc.title || label}</strong>
          {doc.title && <span className="chip">{label}</span>}
          {doc.description && <p className="muted">{doc.description}</p>}
        </div>
        <div className="api-doc-actions">
          <a className="btn ghost" href={doc.url} target="_blank" rel="noreferrer noopener">
            Open document
          </a>
          {renderable && (
            <button type="button" className="btn" onClick={() => setExpanded(!expanded)}>
              {expanded ? 'Hide operations' : 'Show operations'}
            </button>
          )}
        </div>
      </header>

      {expanded && state.loading && <p className="muted">Fetching {doc.url}…</p>}
      {expanded && state.error && (
        <p className="error">
          {state.error}{' '}
          <a href={doc.url} target="_blank" rel="noreferrer noopener">
            {doc.url}
          </a>
        </p>
      )}
      {expanded && state.parsed && (
        <div className="api-doc-body">
          <p className="muted">
            {state.parsed.title}
            {state.parsed.version && ` v${state.parsed.version}`} — {state.parsed.ops.length} operation
            {state.parsed.ops.length === 1 ? '' : 's'}
          </p>
          {state.parsed.servers.length > 0 && (
            <p className="muted">
              Servers: {state.parsed.servers.map((s) => <code key={s}>{s}</code>)}
            </p>
          )}
          {state.parsed.ops.length === 0 && (
            <p className="muted">The document parsed, but declares no paths.</p>
          )}
          {[...byTag.entries()].map(([tag, ops]) => (
            <div key={tag}>
              <h4>{tag}</h4>
              {ops.map((op) => (
                <OperationRow key={`${op.method} ${op.path}`} op={op} />
              ))}
            </div>
          ))}
        </div>
      )}
    </section>
  )
}

export function ApiDocs({ softwareId, docs }: { softwareId: string; docs: ApiDoc[] }) {
  if (!docs?.length) return null
  return (
    <section>
      <h3>API</h3>
      {docs.map((d, i) => (
        <ApiDocView key={d.url} softwareId={softwareId} doc={d} index={i} />
      ))}
    </section>
  )
}
