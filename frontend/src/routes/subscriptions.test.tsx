import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import Subscriptions from './Subscriptions'
import { SessionProvider } from '../lib/session'
import type { Instance, Subscription } from '../lib/types'

const instance: Instance = {
  iri: 'https://reg.test/instance/01b',
  id: '01b',
  label: 'laptop-eerol',
  health: 'unknown',
  runs_30d: 0,
  failures_30d: 0,
  artifact_count: 0,
  allowed_scopes: [],
  token_count: 1,
  origin: { kind: 'local' },
}

const base: Subscription = {
  id: 'sub-1',
  instance_iri: instance.iri,
  label: 'SHACL reports',
  filter: {
    conforms_to: ['http://edamontology.org/data_2048'],
    software: [],
    instance: [],
    keywords: [],
    license: [],
    availability: ['public'],
    roles: ['produced'],
    exclude_own: true,
  },
  webhook_signed: false,
  enabled: true,
  delivery_state: 'active',
  consecutive_failures: 0,
  cursor_seq: 0,
  created_at: new Date().toISOString(),
  pending_count: 2,
  failed_count: 0,
  dead_count: 0,
  unacked_count: 2,
  delivery_mode: 'pull',
  pull_url: 'https://reg.test/api/v1/subscriptions/sub-1/deliveries',
}

const suspended: Subscription = {
  ...base,
  id: 'sub-2',
  label: 'dead receiver',
  webhook_url: 'https://receiver.example/hook',
  webhook_signed: true,
  delivery_mode: 'webhook',
  delivery_state: 'suspended',
  consecutive_failures: 12,
  last_error: 'could not connect to the receiver',
  dead_count: 3,
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

function renderScreen() {
  return render(
    <MemoryRouter initialEntries={['/instances/01b/subscriptions']}>
      <SessionProvider>
        <Routes>
          <Route path="/instances/:id/subscriptions" element={<Subscriptions />} />
        </Routes>
      </SessionProvider>
    </MemoryRouter>,
  )
}

beforeEach(() => {
  vi.stubGlobal('fetch', stubFetch({}))
})
afterEach(() => {
  vi.unstubAllGlobals()
})

describe('Subscriptions', () => {
  it('says nothing is watching yet rather than showing an empty table', async () => {
    vi.stubGlobal('fetch', stubFetch({
      // The more specific route first: the stub matches on substring.
      '/subscriptions': { items: [], total: 0, max_per_instance: 32 },
      '/api/v1/instances/01b': instance,
    }))
    renderScreen()
    expect(await screen.findByText('No subscriptions yet')).toBeInTheDocument()
    expect(screen.queryByRole('table')).not.toBeInTheDocument()
  })

  it('distinguishes a pull subscriber from a webhook one, since only one is reachable', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/subscriptions': { items: [base, suspended], total: 2, max_per_instance: 32 },
      '/api/v1/instances/01b': instance,
    }))
    renderScreen()
    expect(await screen.findByText('SHACL reports')).toBeInTheDocument()
    expect(screen.getByText('pull')).toBeInTheDocument()
    expect(screen.getByText('webhook · signed')).toBeInTheDocument()
    // The pull path has to be explained, not just offered.
    expect(screen.getByText(/Nothing has to be reachable from the internet/)).toBeInTheDocument()
  })

  it('states a failing webhook in words, never by colour alone', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/subscriptions': { items: [suspended], total: 1, max_per_instance: 32 },
      '/api/v1/instances/01b': instance,
    }))
    renderScreen()
    expect(await screen.findByText(/suspended after 12 failures/)).toBeInTheDocument()
    expect(screen.getByText(/3 undeliverable/)).toBeInTheDocument()
    // And the way out is offered where the problem is shown.
    expect(screen.getByRole('button', { name: 'Resume' })).toBeInTheDocument()
  })

  it('warns before someone creates a subscription that matches everything', async () => {
    vi.stubGlobal('fetch', stubFetch({
      '/subscriptions': { items: [], total: 0, max_per_instance: 32 },
      '/api/v1/instances/01b': instance,
    }))
    renderScreen()
    // The form starts empty, so the warning is there from the outset…
    expect(await screen.findByText('This subscription matches everything')).toBeInTheDocument()
    // …and goes away as soon as the subscription actually asks for something.
    await userEvent.type(screen.getByLabelText(/Title or description contains/), 'patients.ttl')
    expect(screen.queryByText('This subscription matches everything')).not.toBeInTheDocument()
  })
})
