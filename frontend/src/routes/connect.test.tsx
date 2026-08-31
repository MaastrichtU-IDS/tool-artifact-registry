import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import ConnectAgent from './ConnectAgent'
import { SessionProvider } from '../lib/session'

const BASE = 'https://reg.ids.example'

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
            issuer: 'https://kc.example/realms/tar',
            human_signin: true,
            workload_issuers: [],
            client_claim: 'azp',
            scopes: [],
          }
        : { enabled: false, human_signin: false, workload_issuers: [], client_claim: 'azp', scopes: [] },
    },
    peers: [],
  }
}

function stub(oidc: boolean) {
  vi.spyOn(globalThis, 'fetch').mockImplementation(async (input: RequestInfo | URL) => {
    if (String(input).includes('/.well-known')) {
      return new Response(JSON.stringify(wellKnown(oidc)), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      })
    }
    return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } })
  })
}

function renderPage() {
  return render(
    <MemoryRouter>
      <SessionProvider>
        <ConnectAgent />
      </SessionProvider>
    </MemoryRouter>,
  )
}

beforeEach(() => vi.restoreAllMocks())
afterEach(() => vi.restoreAllMocks())

describe('ConnectAgent', () => {
  it('builds every snippet from this registry, never a placeholder', async () => {
    stub(false)
    renderPage()
    // The commonest way this page fails a reader is by handing them someone else's hostname.
    await waitFor(() => expect(screen.getByText(`${BASE}/mcp`)).toBeInTheDocument())
    // Both the OAuth line and the bearer-token variant carry this registry's own URL.
    expect(screen.getAllByText(new RegExp(`claude mcp add .*${BASE}/mcp`)).length).toBeGreaterThan(0)
    expect(screen.queryByText(/your-registry\.example/)).not.toBeInTheDocument()
  })

  it('offers a snippet per client, and switching tabs switches the snippet', async () => {
    stub(false)
    renderPage()
    await waitFor(() => expect(screen.getAllByText(/claude mcp add/).length).toBeGreaterThan(0))

    await userEvent.click(screen.getByRole('tab', { name: /cursor/i }))
    expect(screen.getAllByText(/mcpServers/).length).toBeGreaterThan(0)
    expect(screen.queryAllByText(/claude mcp add/)).toHaveLength(0)

    await userEvent.click(screen.getByRole('tab', { name: /curl/i }))
    expect(screen.getByText(/tools\/list/)).toBeInTheDocument()

    await userEvent.click(screen.getByRole('tab', { name: /your own agent/i }))
    expect(screen.getByText(/streamablehttp_client/)).toBeInTheDocument()
  })

  it('gives every snippet a copy button that copies the snippet itself', async () => {
    stub(false)
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })
    renderPage()
    await waitFor(() => expect(screen.getAllByText(/claude mcp add/).length).toBeGreaterThan(0))

    await userEvent.click(screen.getByRole('button', { name: /copy the claude code command$/i }))
    expect(writeText).toHaveBeenCalledWith(`claude mcp add --transport http tar ${BASE}/mcp`)

    // Each tab's snippet is copyable, not only the first.
    await userEvent.click(screen.getByRole('tab', { name: /cursor/i }))
    await userEvent.click(screen.getByRole('button', { name: /copy the mcp.json config/i }))
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining(`${BASE}/mcp`))
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining('mcpServers'))
  })

  it('tells you to sign in through the identity provider when there is one', async () => {
    stub(true)
    renderPage()
    await waitFor(() =>
      expect(screen.getByText(/https:\/\/kc\.example\/realms\/tar/)).toBeInTheDocument(),
    )
    expect(screen.getByText(/nothing long-lived is stored/i)).toBeInTheDocument()
  })

  it('says to use a token when the registry has no identity provider', async () => {
    stub(false)
    renderPage()
    await waitFor(() =>
      expect(screen.getByText(/no identity provider configured/i)).toBeInTheDocument(),
    )
    // And does not imply an OAuth flow that would never start.
    expect(screen.queryByText(/browser window opens/i)).not.toBeInTheDocument()
  })
})
