import type {
  Artifact, Lineage, Page, Peer, Run, RunSummary, SearchResults, Software, Instance,
  Release, TokenRecord, WellKnown, WhoAmI, ProblemJson, KeywordTerm,
  Subscription, SubscriptionCreated, SubscriptionDelivery, SubscriptionDetail,
  SparqlAnswer, SparqlTerm, ArtifactType,
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

/**
 * How to get a fresh access token when the current one has expired.
 *
 * Registered by the session layer, which owns the credential; this module only knows that
 * something can be asked for a new token and that a `401` is when to ask. Returning `null`
 * means renewal is not possible — no refresh token, a pasted API token, or the provider
 * refused — and the `401` then stands, exactly as it did before.
 */
type TokenRenewer = () => Promise<string | null>

let renewer: TokenRenewer | null = null
let renewing: Promise<string | null> | null = null

export function setTokenRenewer(fn: TokenRenewer | null) {
  renewer = fn
  renewing = null
}

/**
 * At most one renewal in flight, whatever else is happening.
 *
 * A page that loads six panels at once expires all six requests at once. Without this they
 * would each present the same refresh token, and with refresh-token rotation — which is what a
 * provider should be configured for — the first use invalidates it and the other five are
 * refused, taking a recoverable expiry and turning it into a forced sign-out. They share one
 * renewal and one answer instead.
 */
function renewOnce(): Promise<string | null> {
  if (!renewer) return Promise.resolve(null)
  if (!renewing) {
    renewing = renewer()
      .catch(() => null)
      .finally(() => {
        renewing = null
      })
  }
  return renewing
}

/**
 * Send a request, and if it comes back `401`, renew once and send it again.
 *
 * The header is rebuilt per attempt rather than captured, so the retry carries the new token
 * rather than the expired one it was rejected for. Only one retry: a second `401` after a
 * successful renewal is the server saying no for a reason renewal cannot fix.
 */
async function sendWithRenewal(path: string, init: RequestInit, headers: Headers): Promise<Response> {
  const attempt = () => {
    const h = new Headers(headers)
    if (authToken) h.set('authorization', `Bearer ${authToken}`)
    else h.delete('authorization')
    return fetch(path, { ...init, headers: h })
  }
  const resp = await attempt()
  if (resp.status !== 401 || !authToken || !renewer) return resp
  const fresh = await renewOnce()
  return fresh ? attempt() : resp
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('accept', 'application/json')
  if (init.body) headers.set('content-type', 'application/json')
  const resp = await sendWithRenewal(path, init, headers)
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

/** A response that is not JSON — an API description document, fetched through the registry. */
async function requestText(path: string): Promise<string> {
  const resp = await sendWithRenewal(`/api/v1${path}`, {}, new Headers())
  const text = await resp.text()
  if (!resp.ok) {
    // The error body *is* JSON even when the success body is not.
    let problem: ProblemJson | null = null
    try {
      problem = JSON.parse(text)
    } catch {
      problem = null
    }
    throw new ApiError(
      problem ?? { type: 'about:blank', title: resp.statusText, status: resp.status },
      resp.status,
    )
  }
  return text
}

export const api = {
  raw: requestText,
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
  // Auto-registration keys: bound to the software, so any deployment of it can register itself.
  listSoftwareTokens: (id: string) =>
    request<{ items: TokenRecord[] }>(`/api/v1/software/${id}/tokens`),
  mintSoftwareToken: (id: string, body: unknown) =>
    request<{ token: string; record: TokenRecord; usage: string }>(`/api/v1/software/${id}/tokens`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  revokeSoftwareToken: (id: string, tokenId: string) =>
    request<void>(`/api/v1/software/${id}/tokens/${tokenId}`, { method: 'DELETE' }),
  mintToken: (id: string, body: unknown) =>
    request<{ token: string; record: TokenRecord }>(`/api/v1/instances/${id}/tokens`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  revokeToken: (id: string, tokenId: string) =>
    request<void>(`/api/v1/instances/${id}/tokens/${tokenId}`, { method: 'DELETE' }),

  keywords: () =>
    request<{ scheme: string; items: KeywordTerm[]; total: number }>('/api/v1/keywords'),
  /// Make a term nameable. Send `iri` to adopt one that already has an identifier elsewhere;
  /// omit it and the registry mints one. Curator only.
  createArtifactType: (body: {
    label: string
    definition?: string
    default_media_type?: string
    slug?: string
    iri?: string
    scheme?: string
    aliases?: string[]
  }) => request<ArtifactType>('/api/v1/types', { method: 'POST', body: JSON.stringify(body) }),
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
  const resp = await sendWithRenewal('/sparql', { method: 'POST', body: query, signal }, headers)
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
