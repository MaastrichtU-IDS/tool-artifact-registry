import type {
  Artifact, Lineage, Page, Peer, Run, RunSummary, SearchResults, Software, Instance,
  Release, TokenRecord, WellKnown, WhoAmI, ProblemJson,
  Subscription, SubscriptionCreated, SubscriptionDelivery, SubscriptionDetail,
  SparqlAnswer, SparqlTerm,
} from './types'

/// An error carrying the RFC 9457 problem document, so a form can map a SHACL report back to
/// its fields instead of dumping Turtle at the user (handoff §5.7).
export class ApiError extends Error {
  problem: ProblemJson
  status: number
  constructor(problem: ProblemJson, status: number) {
    super(problem.detail || problem.title || `HTTP ${status}`)
    this.problem = problem
    this.status = status
  }
  /// Field errors parsed out of the SHACL validation report.
  fieldErrors(): Record<string, string> {
    const out: Record<string, string> = {}
    if (!this.problem.report) return out
    // Entries are emitted as `tar:jsonField "x" ; sh:resultMessage "y"` per result node.
    const re = /tar:jsonField\s+"([^"]+)"\s*;\s*\n?\s*sh:resultMessage\s+"((?:[^"\\]|\\.)*)"/g
    let m: RegExpExecArray | null
    while ((m = re.exec(this.problem.report)) !== null) {
      out[m[1]] = m[2].replace(/\\"/g, '"').replace(/\\n/g, '\n')
    }
    return out
  }
}

let authToken: string | null = null

export function setAuthToken(token: string | null) {
  authToken = token
}

export function getAuthToken() {
  return authToken
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('accept', 'application/json')
  if (init.body) headers.set('content-type', 'application/json')
  if (authToken) headers.set('authorization', `Bearer ${authToken}`)
  const resp = await fetch(path, { ...init, headers })
  if (resp.status === 204) return undefined as T
  const text = await resp.text()
  const body = text ? JSON.parse(text) : null
  if (!resp.ok) {
    throw new ApiError(
      body ?? { type: 'about:blank', title: resp.statusText, status: resp.status },
      resp.status,
    )
  }
  return body as T
}

export function qs(params: Record<string, string | number | boolean | undefined | null>): string {
  const sp = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== null && v !== '' && v !== false) sp.set(k, String(v))
  }
  const s = sp.toString()
  return s ? `?${s}` : ''
}

export const api = {
  wellKnown: () => request<WellKnown>('/.well-known/tar-registry'),
  registry: () => request<Record<string, unknown>>('/api/v1/registry'),
  whoami: () => request<WhoAmI>('/api/v1/whoami'),

  listSoftware: (p: Record<string, string | undefined>) =>
    request<Page<Software>>(`/api/v1/software${qs(p)}`),
  getSoftware: (id: string) => request<Software>(`/api/v1/software/${id}`),
  createSoftware: (body: unknown) =>
    request<Software>('/api/v1/software', { method: 'POST', body: JSON.stringify(body) }),
  updateSoftware: (id: string, body: unknown) =>
    request<Software>(`/api/v1/software/${id}`, { method: 'PATCH', body: JSON.stringify(body) }),
  listReleases: (id: string) => request<Page<Release>>(`/api/v1/software/${id}/releases`),
  syncSoftware: (id: string) =>
    request<{ changed: string[]; releases_added: string[]; skipped: string[] }>(
      `/api/v1/software/${id}/sync`,
      { method: 'POST' },
    ),
  createRelease: (id: string, body: unknown) =>
    request<Release>(`/api/v1/software/${id}/releases`, { method: 'POST', body: JSON.stringify(body) }),

  listInstances: (p: Record<string, string | undefined>) =>
    request<Page<Instance>>(`/api/v1/instances${qs(p)}`),
  getInstance: (id: string) => request<Instance>(`/api/v1/instances/${id}`),
  createInstance: (body: unknown) =>
    request<Instance>('/api/v1/instances', { method: 'POST', body: JSON.stringify(body) }),
  updateInstance: (id: string, body: unknown) =>
    request<Instance>(`/api/v1/instances/${id}`, { method: 'PATCH', body: JSON.stringify(body) }),
  instanceRuns: (id: string, p: Record<string, string | undefined> = {}) =>
    request<Page<RunSummary>>(`/api/v1/instances/${id}/runs${qs(p)}`),
  instanceArtifacts: (id: string, p: Record<string, string | undefined> = {}) =>
    request<Page<Artifact>>(`/api/v1/instances/${id}/artifacts${qs(p)}`),

  listTokens: (id: string) => request<{ items: TokenRecord[] }>(`/api/v1/instances/${id}/tokens`),
  mintToken: (id: string, body: unknown) =>
    request<{ token: string; record: TokenRecord }>(`/api/v1/instances/${id}/tokens`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  revokeToken: (id: string, tokenId: string) =>
    request<void>(`/api/v1/instances/${id}/tokens/${tokenId}`, { method: 'DELETE' }),

  listArtifacts: (p: Record<string, string | undefined>) =>
    request<Page<Artifact>>(`/api/v1/artifacts${qs(p)}`),
  getArtifact: (id: string) => request<Artifact>(`/api/v1/artifacts/${id}`),
  lineage: (id: string, depth = 1) =>
    request<Lineage>(`/api/v1/artifacts/${id}/lineage${qs({ depth, direction: 'both' })}`),

  listRuns: (p: Record<string, string | undefined>) => request<Page<RunSummary>>(`/api/v1/runs${qs(p)}`),
  getRun: (id: string) => request<Run>(`/api/v1/runs/${id}`),

  search: (q: string, type?: string, federated?: boolean) =>
    request<SearchResults>(`/api/v1/search${qs({ q, type, federated })}`),
  capabilities: (p: { produces?: string; consumes?: string }) =>
    request<{ items: { iri: string; entity_type: string; name?: string }[]; total: number }>(
      `/api/v1/capabilities${qs(p)}`,
    ),

  listPeers: () => request<{ items: Peer[]; total: number }>('/api/v1/peers'),
  suggestedPeers: () => request<{ items: Peer[]; total: number }>('/api/v1/peers/suggested'),
  previewPeer: (base_url: string) =>
    request<{ base_iri: string; title?: string; operator?: string; peers_of_peer: string[] }>(
      '/api/v1/peers',
      { method: 'POST', body: JSON.stringify({ base_url, preview: true }) },
    ),
  addPeer: (base_url: string) =>
    request<Peer>('/api/v1/peers', { method: 'POST', body: JSON.stringify({ base_url }) }),
  removePeer: (id: string) => request<void>(`/api/v1/peers/${id}`, { method: 'DELETE' }),

  // Subscriptions: "tell me when an artifact like this appears". Owned by an Instance and
  // managed with that Instance's credential, exactly like its tokens.
  listSubscriptions: (id: string) =>
    request<{ items: Subscription[]; total: number; max_per_instance: number }>(
      `/api/v1/instances/${id}/subscriptions`,
    ),
  createSubscription: (id: string, body: unknown) =>
    request<SubscriptionCreated>(`/api/v1/instances/${id}/subscriptions`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  getSubscription: (sid: string) => request<SubscriptionDetail>(`/api/v1/subscriptions/${sid}`),
  updateSubscription: (sid: string, body: unknown) =>
    request<SubscriptionCreated>(`/api/v1/subscriptions/${sid}`, {
      method: 'PATCH',
      body: JSON.stringify(body),
    }),
  deleteSubscription: (sid: string) =>
    request<void>(`/api/v1/subscriptions/${sid}`, { method: 'DELETE' }),
  /// The pull path — what a tool behind a firewall uses instead of a webhook.
  subscriptionDeliveries: (sid: string, p: Record<string, string | undefined> = {}) =>
    request<{
      items: SubscriptionDelivery[]
      cursor: number
      next_cursor: number
      remaining: number
      acknowledged?: number
    }>(`/api/v1/subscriptions/${sid}/deliveries${qs(p)}`),

  /// Read-only SPARQL 1.1 (spec §7.7). Not routed through `request()`: CONSTRUCT and DESCRIBE
  /// answer in Turtle, so the response is dispatched on its content type, not assumed to be
  /// JSON.
  sparql: (query: string, signal?: AbortSignal) => sparql(query, signal),
}

async function sparql(query: string, signal?: AbortSignal): Promise<SparqlAnswer> {
  const headers = new Headers({
    'content-type': 'application/sparql-query',
    // Both answers are acceptable; the server picks according to the query form.
    accept: 'application/sparql-results+json, text/turtle;q=0.9',
  })
  if (authToken) headers.set('authorization', `Bearer ${authToken}`)
  const resp = await fetch('/sparql', { method: 'POST', headers, body: query, signal })
  const text = await resp.text()
  const contentType = resp.headers.get('content-type') ?? ''

  if (!resp.ok) {
    // Every error path is RFC 9457 (spec §7.9) — a refused write, a syntax error, a 401.
    let problem: ProblemJson
    try {
      problem = JSON.parse(text) as ProblemJson
    } catch {
      problem = {
        type: 'about:blank',
        title: resp.statusText || `HTTP ${resp.status}`,
        status: resp.status,
        detail: text.slice(0, 2000) || undefined,
      }
    }
    throw new ApiError(problem, resp.status)
  }

  if (contentType.includes('turtle') || contentType.includes('n-triples')) {
    return { form: 'graph', turtle: text }
  }
  const doc = JSON.parse(text) as {
    head?: { vars?: string[] }
    boolean?: boolean
    results?: { bindings?: Record<string, SparqlTerm>[] }
  }
  if (typeof doc.boolean === 'boolean') return { form: 'ask', boolean: doc.boolean }
  return {
    form: 'select',
    vars: doc.head?.vars ?? [],
    rows: doc.results?.bindings ?? [],
  }
}

/// The registry's IRI for a record is also its UI route (handoff §3), so links are just paths.
export function pathFor(iri: string, base: string): string {
  if (base && iri.startsWith(base)) return iri.slice(base.length) || '/'
  return iri
}
