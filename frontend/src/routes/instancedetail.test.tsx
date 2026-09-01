import { render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import InstanceDetail from './InstanceDetail'
import { SessionProvider } from '../lib/session'

const BASE = 'https://reg.test'

const wellKnown = {
  title: 'Test registry', base_iri: BASE, sparql_url: `${BASE}/sparql`,
  auth: { anonymous_read: true, api_tokens: true,
          oidc: { enabled: false, human_signin: false, workload_issuers: [], client_claim: 'azp', scopes: [] } },
  peers: [],
}

function stub(instance: Record<string, unknown>) {
  vi.spyOn(globalThis, 'fetch').mockImplementation(async (input: RequestInfo | URL) => {
    const url = String(input)
    const json = (v: unknown) =>
      new Response(JSON.stringify(v), { status: 200, headers: { 'content-type': 'application/json' } })
    if (url.includes('/.well-known')) return json(wellKnown)
    if (url.includes('/whoami')) return json({ authenticated: true, roles: ['curator'], is_curator: true })
    if (url.includes('/instances/01a/runs')) return json({ items: [], total: 0 })
    if (url.includes('/instances/01a/artifacts')) return json({ items: [], total: 0 })
    if (url.includes('/instances/01a')) return json(instance)
    return json({ items: [], total: 0 })
  })
}

const base = {
  iri: `${BASE}/instance/01a`, id: '01a', label: 'a deployment',
  health: 'unknown', runs_30d: 0, failures_30d: 0, artifact_count: 0,
  allowed_scopes: [], token_count: 0, origin: { kind: 'local' },
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={['/instances/01a']}>
      <SessionProvider>
        <Routes><Route path="/instances/:id" element={<InstanceDetail />} /></Routes>
      </SessionProvider>
    </MemoryRouter>,
  )
}

beforeEach(() => {
  vi.restoreAllMocks()
  // The Edit affordance is curator-only, so without a session the assertions below would pass
  // for the wrong reason — an absent button proves nothing if nobody could ever see it.
  sessionStorage.setItem('tar.token', 'test-token')
})

describe('a self-registered deployment', () => {
  it('offers no edit, and says why instead of just omitting it', async () => {
    stub({ ...base, self_registered_by: 'urn:tar:token:01a', instance_key: 'prod' })
    renderPage()
    await waitFor(() => expect(screen.getByText(/maintains its own record/i)).toBeInTheDocument())
    // An absent button with no explanation reads as a broken page.
    expect(screen.queryByRole('link', { name: 'Edit' })).not.toBeInTheDocument()
    expect(screen.getByText(/overwritten without warning/i)).toBeInTheDocument()
    // And it names what a curator can still do.
    expect(screen.getByText(/withdraw the record or revoke the credential/i)).toBeInTheDocument()
  })

  it('leaves a curator-created record editable', async () => {
    stub({ ...base })
    renderPage()
    await waitFor(() => expect(screen.getByRole('link', { name: 'Edit' })).toBeInTheDocument())
    expect(screen.queryByText(/maintains its own record/i)).not.toBeInTheDocument()
  })
})
