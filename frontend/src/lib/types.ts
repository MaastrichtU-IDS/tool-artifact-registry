// Mirrors src/model.rs. Hand-written rather than generated: the surface is small, and a code
// generator would be one more build step in a repo that promises minimal ops.

export interface Origin {
  kind: 'local' | 'peer'
  peer_id?: string
  peer_title?: string
  peer_base_iri?: string
  cached_at?: string
  resolve_status?: string
}

export interface TypeRef {
  iri: string
  label?: string
  definition?: string
  source: 'edam' | 'local' | 'external'
}

export interface AgentRef {
  iri: string
  name?: string
  kind?: string
  identifier?: string
  email?: string
  homepage?: string
}

export interface Capability {
  iri: string
  produces: TypeRef[]
  consumes: TypeRef[]
  declared_at: 'software' | 'release' | 'instance'
}

export interface Download {
  url: string
  label?: string
  platform?: string
  byte_size?: number
  availability?: string
}

export interface Release {
  iri: string
  id: string
  version: string
  date_published?: string
  container_image?: string
  image_digest?: string
  changelog?: string
  install_command?: string
  downloads?: Download[]
  software?: string
  capability?: Capability
  origin: Origin
}

export interface Software {
  iri: string
  id: string
  name: string
  tagline?: string
  description?: string
  homepage?: string
  code_repository?: string
  documentation?: string
  download_url?: string
  image?: string
  screenshots: string[]
  readme?: string
  readme_base_url?: string
  license?: string
  kind?: string
  maturity?: string
  deployable: boolean
  edam_topics: TypeRef[]
  keywords: string[]
  publisher?: AgentRef
  contact?: AgentRef
  publications: string[]
  capability?: Capability
  latest_release?: Release
  instance_count: number
  release_count: number
  runs_30d: number
  created?: string
  modified?: string
  origin: Origin
  tombstoned?: boolean
}

export interface Instance {
  iri: string
  id: string
  label: string
  description?: string
  software?: string
  software_name?: string
  release?: string
  release_version?: string
  outdated?: boolean
  latest_version?: string
  endpoint_url?: string
  endpoint_description?: string
  operator?: AgentRef
  availability?: string
  jurisdiction?: string
  health: 'up' | 'down' | 'unknown'
  home_registry?: string
  capability?: Capability
  last_run_at?: string
  runs_30d: number
  failures_30d: number
  artifact_count: number
  oidc_client_id?: string
  oidc_issuer?: string
  allowed_scopes: string[]
  token_count: number
  origin: Origin
  tombstoned?: boolean
}

export type Availability = 'public' | 'restricted' | 'embargoed' | 'metadata-only'

export interface Distribution {
  iri: string
  title?: string
  access_url?: string
  download_url?: string
  media_type?: string
  byte_size?: number
  checksum?: { algorithm: string; value: string }
  conforms_to?: string
  license?: string
  access_service?: string
  access_protocol?: string
  auth_method?: string
  availability: Availability
  access_request_url?: string
}

export interface RunSummary {
  iri: string
  id: string
  label?: string
  status: string
  started_at?: string
  ended_at?: string
  duration_seconds?: number
  external_key?: string
  instance?: string
  instance_label?: string
  release?: string
  release_version?: string
  software?: string
  software_name?: string
  used_count: number
  generated_count: number
  origin: Origin
}

export interface ArtifactRef {
  iri: string
  title?: string
  conforms_to?: TypeRef
  availability?: Availability
  origin: Origin
  unresolved?: boolean
}

export interface Run extends RunSummary {
  used: ArtifactRef[]
  generated: ArtifactRef[]
  openlineage_payload?: unknown
}

export interface Artifact {
  iri: string
  id: string
  title?: string
  description?: string
  conforms_to?: TypeRef
  license?: string
  keywords: string[]
  issued?: string
  publisher?: AgentRef
  distributions: Distribution[]
  availability: Availability
  was_derived_from: string[]
  was_revision_of?: string
  is_version_of?: string
  was_generated_by?: string
  generated_by_run?: RunSummary
  external_key?: string
  origin: Origin
  tombstoned?: boolean
}

export interface FacetValue {
  value: string
  label?: string
  count: number
}
export interface Facet {
  name: string
  values: FacetValue[]
}

export interface Page<T> {
  items: T[]
  total: number
  next_cursor?: string
  facets?: Facet[]
}

export interface SearchHit {
  iri: string
  entity_type: 'software' | 'instance' | 'artifact' | 'run'
  title: string
  snippet?: string
  origin: Origin
  score: number
}

export interface PeerSearchStatus {
  peer_id: string
  base_iri: string
  title?: string
  status: 'ok' | 'timeout' | 'error'
  hits: number
  error?: string
}

export interface SearchResults {
  query: string
  hits: SearchHit[]
  total: number
  partial: boolean
  peers: PeerSearchStatus[]
}

export interface Peer {
  id: string
  base_iri: string
  title?: string
  operator?: string
  added_at: string
  last_seen_at?: string
  resolve_status: string
  last_error?: string
  record_count: number
  state: 'active' | 'suggested' | 'dismissed'
  suggested_by?: string
}

export interface TokenRecord {
  id: string
  prefix: string
  instance_iri?: string
  scopes: string[]
  label?: string
  created_at: string
  created_by?: string
  expires_at?: string
  last_used_at?: string
  revoked_at?: string
}

export interface LineageNode {
  iri: string
  entity_type: string
  title?: string
  origin: Origin
  depth: number
  unresolved?: boolean
}
export interface LineageEdge {
  from: string
  to: string
  predicate: string
}
export interface Lineage {
  root: string
  nodes: LineageNode[]
  edges: LineageEdge[]
  truncated: boolean
}

export interface WellKnown {
  title: string
  operator?: string
  base_iri: string
  software_version: string
  public_read: boolean
  sparql_url: string
  auth: {
    anonymous_read: boolean
    api_tokens: boolean
    oidc: {
      enabled: boolean
      issuer?: string
      client_id?: string
      human_signin: boolean
      workload_issuers: string[]
      audience?: string
      client_claim: string
      scopes: string[]
    }
  }
  peers: { base_iri: string; title?: string }[]
}

export interface WhoAmI {
  authenticated: boolean
  credential: string
  subject: string
  display_name?: string
  instance?: string
  issuer?: string
  scopes: string[]
  roles: string[]
  is_curator: boolean
  is_admin: boolean
}

/** RFC 9457, with the SHACL report of spec §7.9 attached. */
export interface ProblemJson {
  type: string
  title: string
  status: number
  detail?: string
  report?: string
  report_media_type?: string
}
