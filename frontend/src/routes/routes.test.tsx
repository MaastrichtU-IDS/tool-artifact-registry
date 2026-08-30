import { render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import ArtifactDetail from './ArtifactDetail'
import SoftwareDetail from './SoftwareDetail'
import SoftwareList from './SoftwareList'
import { SessionProvider } from '../lib/session'
import type { Artifact, Page, Software } from '../lib/types'

const software: Software = {
  iri: 'https://reg.test/software/01a',
  id: '01a',
  name: 'shacl-manager',
  tagline: 'SHACL shape management and validation',
  license: 'https://spdx.org/licenses/Apache-2.0',
  kind: 'service',
  kinds: ['service'],
  edam_topics: [{ iri: 'http://edamontology.org/topic_3071', label: 'Data management', source: 'edam' }],
  keywords: ['shacl'],
  screenshots: [],
  deployable: true,
  publications: [],
  capability: {
    iri: 'https://reg.test/capability/1',
    declared_at: 'software',
    consumes: [{ iri: 'http://edamontology.org/data_2600', label: 'RDF graph', source: 'edam' }],
    produces: [{ iri: 'https://reg.test/type/shacl-validation-report', label: 'SHACL validation report', source: 'local' }],
  },
  instance_count: 2,
  release_count: 1,
  runs_30d: 143,
  origin: { kind: 'local' },
}

/** A record cached from a peer: the UI must never render it identically to a local one. */
const foreignSoftware: Software = {
  ...software,
  iri: 'https://reg.mumc.nl/software/09z',
  id: '09z',
  name: 'shacl-manager (MUMC)',
  origin: { kind: 'peer', peer_title: 'MUMC', peer_base_iri: 'https://reg.mumc.nl', cached_at: new Date().toISOString() },
}

const metadataOnly: Artifact = {
  iri: 'https://reg.test/artifact/01b',
  id: '01b',
  title: 'Masked replica of the MUMC cohort',
  conforms_to: { iri: 'http://edamontology.org/data_2600', label: 'RDF graph', source: 'edam' },
  keywords: [],
  distributions: [{
    iri: 'https://reg.test/distribution/1',
    media_type: 'text/turtle',
    availability: 'metadata-only',
    access_request_url: 'https://ids.unimaas.nl/data-access',
  }],
  availability: 'metadata-only',
  was_derived_from: [],
  origin: { kind: 'local' },
}

const publicArtifact: Artifact = {
  ...metadataOnly,
  iri: 'https://reg.test/artifact/01c',
  id: '01c',
  title: 'Validation report',
  availability: 'public',
  distributions: [{
    iri: 'https://reg.test/distribution/2',
    media_type: 'text/turtle',
    availability: 'public',
    download_url: 'https://shacl.example/r.ttl',
    access_url: 'https://shacl.example/r',
  }],
}

function stubFetch(routes: Record<string, unknown>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input)
    const key = Object.keys(routes).find((k) => url.includes(k))
    if (!key) return new Response('{}', { status: 404 })
    return new Response(JSON.stringify(routes[key]), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    })
  })
}

const emptyPage = <T,>(): Page<T> => ({ items: [], total: 0 })

beforeEach(() => {
  vi.stubGlobal('fetch', stubFetch({}))
})
afterEach(() => {
  vi.unstubAllGlobals()
})

function renderAt(path: string, element: React.ReactNode, pattern: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <SessionProvider>
        <Routes>
          <Route path={pattern} element={element} />
        </Routes>
      </SessionProvider>
    </MemoryRouter>,
  )
}

describe('SoftwareList', () => {
  it('leads with the tool, its licence and how many deployments exist', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/api/v1/software': { items: [software], total: 1, facets: [] } satisfies Page<Software>,
      '/.well-known': { title: 'Test', auth: { anonymous_read: true, api_tokens: true, oidc: { enabled: false, human_signin: false, workload_issuers: [], client_claim: 'azp', scopes: [] } } },
    }))
    renderAt('/software', <SoftwareList />, '/software')
    expect(await screen.findByText('shacl-manager')).toBeInTheDocument()
    expect(screen.getByText('SHACL shape management and validation')).toBeInTheDocument()
    expect(screen.getByText('Apache-2.0')).toBeInTheDocument()
    expect(screen.getByText('2 instances')).toBeInTheDocument()
    expect(screen.getByText('143 runs/30d')).toBeInTheDocument()
  })

  it('never nests a link inside the clickable row', async () => {
    // Nested anchors are invalid HTML and trap keyboard users; chips inside a row-card link
    // must render as spans.
    vi.stubGlobal('fetch', stubFetch({
      '/api/v1/software': { items: [software], total: 1, facets: [] } satisfies Page<Software>,
    }))
    const { container } = renderAt('/software', <SoftwareList />, '/software')
    await screen.findByText('shacl-manager')
    expect(container.querySelectorAll('a a')).toHaveLength(0)
  })

  it('distinguishes empty from filtered-empty', async () => {
    vi.stubGlobal('fetch', stubFetch({ '/api/v1/software': emptyPage<Software>() }))
    const { unmount } = renderAt('/software', <SoftwareList />, '/software')
    expect(await screen.findByText('No software registered yet')).toBeInTheDocument()
    unmount()

    renderAt('/software?q=nothing', <SoftwareList />, '/software')
    expect(await screen.findByText('No software matches these filters')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /clear filters/i })).toBeInTheDocument()
  })
})

describe('SoftwareDetail', () => {
  it('puts consumes and produces above the fold and offers the matchmaking link', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/api/v1/software/01a/releases': emptyPage(),
      '/api/v1/instances': emptyPage(),
      '/api/v1/software/01a': software,
    }))
    renderAt('/software/01a', <SoftwareDetail />, '/software/:id')
    expect(await screen.findByRole('heading', { name: 'shacl-manager' })).toBeInTheDocument()
    expect(screen.getByText('RDF graph')).toBeInTheDocument()
    expect(screen.getByText('SHACL validation report')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /What consumes SHACL validation report/ })).toBeInTheDocument()
  })

  it('offers no edit affordance at all on a record cached from a peer', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/api/v1/software/09z/releases': emptyPage(),
      '/api/v1/instances': emptyPage(),
      '/api/v1/software/09z': foreignSoftware,
    }))
    renderAt('/software/09z', <SoftwareDetail />, '/software/:id')
    await screen.findByRole('heading', { name: 'shacl-manager (MUMC)' })
    expect(screen.getByText(/Cached from another registry/)).toBeInTheDocument()
    expect(screen.queryByRole('link', { name: 'Edit' })).not.toBeInTheDocument()
  })

  it('calls them installations, not deployments, when the software cannot be hosted', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/api/v1/software/01a/releases': emptyPage(),
      '/api/v1/instances': emptyPage(),
      '/api/v1/software/01a': { ...software, deployable: false },
    }))
    renderAt('/software/01a', <SoftwareDetail />, '/software/:id')
    expect(await screen.findByText('Installations')).toBeInTheDocument()
    expect(screen.getByText(/runs on a machine rather than being hosted/)).toBeInTheDocument()
    expect(screen.queryByText('Deployments')).not.toBeInTheDocument()
  })

  it('shows the absence of a capability as information, not as a hidden block', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/api/v1/software/01a/releases': emptyPage(),
      '/api/v1/instances': emptyPage(),
      '/api/v1/software/01a': { ...software, capability: undefined },
    }))
    renderAt('/software/01a', <SoftwareDetail />, '/software/:id')
    expect(await screen.findByText('No capability declared')).toBeInTheDocument()
  })
})

describe('ArtifactDetail', () => {
  it('renders no download affordance for a metadata-only artifact, only a request-access link', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/lineage': { root: metadataOnly.iri, nodes: [], edges: [], truncated: false },
      '/api/v1/artifacts/01b': metadataOnly,
    }))
    renderAt('/artifacts/01b', <ArtifactDetail />, '/artifacts/:id')
    await screen.findByRole('heading', { name: 'Masked replica of the MUMC cohort' })
    // Never a disabled download button: that would miscommunicate (handoff §5.5).
    expect(screen.queryByRole('link', { name: /Download/ })).not.toBeInTheDocument()
    expect(screen.getByRole('link', { name: /Request access/ })).toHaveAttribute(
      'href',
      'https://ids.unimaas.nl/data-access',
    )
    expect(screen.getByText(/bytes are not published here/)).toBeInTheDocument()
  })

  it('does offer a download when the bytes exist', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/lineage': { root: publicArtifact.iri, nodes: [], edges: [], truncated: false },
      '/api/v1/artifacts/01c': publicArtifact,
    }))
    renderAt('/artifacts/01c', <ArtifactDetail />, '/artifacts/:id')
    await waitFor(() =>
      expect(screen.getByRole('link', { name: /Download/ })).toHaveAttribute('href', 'https://shacl.example/r.ttl'),
    )
  })
})
