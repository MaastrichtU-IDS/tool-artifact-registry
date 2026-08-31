import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import SoftwareDeploy from './SoftwareDeploy'
import SoftwareDetail from './SoftwareDetail'
import { SessionProvider } from '../lib/session'
import type { Software, TokenRecord, WhoAmI } from '../lib/types'

const BASE = 'https://reg.ids.example'
const ISSUER = 'https://kc.example/realms/tar'

const software: Software = {
  iri: `${BASE}/software/01a`,
  id: '01a',
  name: 'shacl-manager',
  tagline: 'SHACL shape management and validation',
  screenshots: [],
  api_docs: [],
  registration_clients: [],
  kinds: ['service'],
  kind: 'service',
  deployable: true,
  topics: [],
  keywords: [],
  publications: [],
  instance_count: 0,
  release_count: 0,
  runs_30d: 0,
  origin: { kind: 'local' },
}

const curator: WhoAmI = {
  authenticated: true,
  credential: 'oidc-human',
  subject: 'curator',
  scopes: [],
  roles: ['curator'],
  is_curator: true,
  is_admin: false,
}

const reader: WhoAmI = { ...curator, roles: ['reader'], is_curator: false }

const key: TokenRecord = {
  id: 'tok-1',
  prefix: 'a1b2c3',
  scopes: ['register:instance', 'advertise:produce'],
  label: 'helm chart',
  created_at: new Date().toISOString(),
}

function wellKnown(oidc: boolean) {
  return {
    title: 'Test registry',
    base_iri: BASE,
    sparql_url: `${BASE}/sparql`,
    auth: {
      anonymous_read: true,
      api_tokens: true,
      oidc: oidc
        ? {
            enabled: true,
            issuer: ISSUER,
            human_signin: true,
            workload_issuers: [],
            audience: BASE,
            client_claim: 'azp',
            scopes: [],
          }
        : { enabled: false, human_signin: false, workload_issuers: [], client_claim: 'azp', scopes: [] },
    },
    peers: [],
  }
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

/** The more specific route first: the stub matches on substring. */
function stub({
  oidc = false, who = curator, tokens = [] as TokenRecord[], record = software,
} = {}) {
  vi.stubGlobal('fetch', stubFetch({
    '/api/v1/software/01a/tokens': { items: tokens, total: tokens.length },
    '/api/v1/software/01a/releases': { items: [], total: 0 },
    '/api/v1/software/01a': record,
    '/api/v1/instances': { items: [], total: 0 },
    '/api/v1/whoami': who,
    '/.well-known': wellKnown(oidc),
  }))
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={['/software/01a/deploy']}>
      <SessionProvider>
        <Routes>
          <Route path="/software/:id/deploy" element={<SoftwareDeploy />} />
        </Routes>
      </SessionProvider>
    </MemoryRouter>,
  )
}

/** Signed in as whoever `stub` was given: the session only asks who it is when it holds one. */
beforeEach(() => sessionStorage.setItem('tar.token', 'tar_test'))
afterEach(() => {
  sessionStorage.clear()
  vi.unstubAllGlobals()
})

describe('SoftwareDeploy', () => {
  it('names every way in at once, so the reader can see what they are not choosing', async () => {
    stub()
    renderPage()
    // A reader who has already decided one axis — usually the credential their site runs on —
    // must be able to find the other without reading three sets of instructions.
    expect(await screen.findByRole('radio', { name: /Manual registration/ })).toBeInTheDocument()
    expect(screen.getByRole('radio', { name: /Self-registration/ })).toBeInTheDocument()
    expect(screen.getByRole('radio', { name: /API key/ })).toBeInTheDocument()
    expect(screen.getByRole('radio', { name: /Identity provider/ })).toBeInTheDocument()
  })

  it('starts on the credential this registry is actually configured for', async () => {
    stub({ oidc: true })
    const { unmount } = renderPage()
    // Offering a provider that is not there, or a key when single sign-on is set up, sends the
    // reader down a path their registry cannot walk.
    await waitFor(() => expect(screen.getByRole('radio', { name: /Identity provider/ })).toBeChecked())
    unmount()

    stub({ oidc: false })
    renderPage()
    await waitFor(() => expect(screen.getByRole('radio', { name: /API key/ })).toBeChecked())
  })

  it('builds every command from this registry, never a placeholder', async () => {
    stub({ oidc: true })
    renderPage()
    await screen.findByRole('radio', { name: /Manual registration/ })

    await userEvent.click(screen.getByRole('radio', { name: /Self-registration/ }))
    expect(screen.getByText(new RegExp(`curl -X PUT ${BASE}/api/v1/instances/self`))).toBeInTheDocument()

    await userEvent.click(screen.getByRole('radio', { name: /API key/ }))
    expect(screen.getByText(new RegExp(`curl -X PUT ${BASE}/api/v1/instances/self`))).toBeInTheDocument()

    await userEvent.click(screen.getByRole('radio', { name: /Manual registration/ }))
    expect(screen.getByText(new RegExp(`curl -X POST ${BASE}/api/v1/instances`))).toBeInTheDocument()

    expect(screen.queryByText(/localhost:8080|your-registry\.example/)).not.toBeInTheDocument()
  })

  it('sends a curator to the form for manual registration, and offers no key there', async () => {
    stub()
    renderPage()
    const link = await screen.findByRole('link', { name: /Open the deployment form/ })
    expect(link).toHaveAttribute('href', '/instances/new?software=01a')
    // The mint form belongs to self-registration; showing it here would suggest the manual
    // path needs a credential of its own.
    expect(screen.queryByRole('button', { name: 'Mint key' })).not.toBeInTheDocument()
    expect(screen.queryByText(/What the announcement contains/)).not.toBeInTheDocument()
  })

  it('switching the credential switches the self-registration instructions', async () => {
    stub({ oidc: true })
    renderPage()
    await screen.findByRole('radio', { name: /Self-registration/ })
    await userEvent.click(screen.getByRole('radio', { name: /Self-registration/ }))

    // Through the provider: fetch a short-lived token, and no key is minted anywhere.
    expect(screen.getByText(/grant_type=client_credentials/)).toBeInTheDocument()
    expect(screen.getByText(new RegExp(`ISSUER=${ISSUER}`))).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Mint key' })).not.toBeInTheDocument()

    await userEvent.click(screen.getByRole('radio', { name: /API key/ }))
    expect(screen.getByRole('button', { name: 'Mint key' })).toBeInTheDocument()
    expect(screen.queryByText(/grant_type=client_credentials/)).not.toBeInTheDocument()
  })

  it('states what the request must contain for either credential', async () => {
    stub({ oidc: true })
    renderPage()
    await screen.findByRole('radio', { name: /Self-registration/ })
    await userEvent.click(screen.getByRole('radio', { name: /Self-registration/ }))

    for (const field of ['instance_key', 'label', 'software', 'version', 'endpoint_url', 'health_endpoint', 'capability']) {
      expect(screen.getByText(field)).toBeInTheDocument()
    }
    // The two rules that cost the most time when they are learned from a response code.
    expect(screen.getAllByText(/403/).length).toBeGreaterThan(0)
    expect(screen.getByText(/keeps whatever is stored/)).toBeInTheDocument()

    // Same reference under the other credential — the body does not depend on who signed it.
    await userEvent.click(screen.getByRole('radio', { name: /API key/ }))
    expect(screen.getByText('instance_key')).toBeInTheDocument()
  })

  it('gives every snippet a copy button that copies the snippet it sits on', async () => {
    stub({ oidc: true })
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })
    renderPage()
    await screen.findByRole('radio', { name: /Self-registration/ })
    await userEvent.click(screen.getByRole('radio', { name: /Self-registration/ }))

    await userEvent.click(screen.getByRole('button', { name: /copy the service account announcement/i }))
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining('grant_type=client_credentials'))
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining(`${BASE}/api/v1/instances/self`))

    await userEvent.click(screen.getByRole('radio', { name: /API key/ }))
    await userEvent.click(screen.getByRole('button', { name: /copy the announcement/i }))
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining('$TAR_KEY'))
  })

  it('says so when no issuer is trusted, rather than printing a recipe that cannot work', async () => {
    stub({ oidc: false })
    renderPage()
    await screen.findByRole('radio', { name: /Self-registration/ })
    await userEvent.click(screen.getByRole('radio', { name: /Self-registration/ }))
    await userEvent.click(screen.getByRole('radio', { name: /Identity provider/ }))

    expect(screen.getByText(/trusts no issuer yet/)).toBeInTheDocument()
    expect(screen.queryByText(/grant_type=client_credentials/)).not.toBeInTheDocument()

    // And the way out is offered where the problem is stated.
    await userEvent.click(screen.getByRole('button', { name: /Show me that instead/ }))
    expect(screen.getByRole('button', { name: 'Mint key' })).toBeInTheDocument()
  })

  it('lists the keys already issued, since each one can add records on its own', async () => {
    stub({ tokens: [key] })
    renderPage()
    await screen.findByRole('radio', { name: /Self-registration/ })
    await userEvent.click(screen.getByRole('radio', { name: /Self-registration/ }))
    expect(screen.getByText('helm chart')).toBeInTheDocument()
    expect(screen.getByText('tar_a1b2c3…')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Revoke' })).toBeInTheDocument()
  })

  it('refuses the whole page to someone who is not a curator', async () => {
    stub({ who: reader })
    renderPage()
    expect(await screen.findByText('Curator role required')).toBeInTheDocument()
    expect(screen.queryByRole('radio')).not.toBeInTheDocument()
  })
})

describe('SoftwareDetail', () => {
  function renderDetail() {
    return render(
      <MemoryRouter initialEntries={['/software/01a']}>
        <SessionProvider>
          <Routes>
            <Route path="/software/:id" element={<SoftwareDetail />} />
          </Routes>
        </SessionProvider>
      </MemoryRouter>,
    )
  }

  it('leads with creating a deployment, not with the mechanism that might do it', async () => {
    stub()
    renderDetail()
    const link = await screen.findByRole('link', { name: 'Create deployment' })
    expect(link).toHaveAttribute('href', '/software/01a/deploy')
  })

  it('offers nothing of the sort for software that cannot be hosted', async () => {
    stub({ record: { ...software, deployable: false } })
    renderDetail()
    await screen.findByRole('heading', { name: 'shacl-manager' })
    // There would be no deployment to register, so the entry point is absent, not disabled.
    expect(screen.queryByRole('link', { name: 'Create deployment' })).not.toBeInTheDocument()
  })
})
