import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import Sparql, { routeFor } from './Sparql'
import { SessionProvider } from '../lib/session'

const BASE = 'https://reg.test'

const wellKnown = {
  title: 'Test registry',
  base_iri: BASE,
  sparql_url: `${BASE}/sparql`,
  auth: {
    anonymous_read: true,
    api_tokens: true,
    oidc: { enabled: false, human_signin: false, workload_issuers: [], client_claim: 'azp', scopes: [] },
  },
  peers: [],
}

type Answer =
  | { kind: 'select'; vars: string[]; rows: Record<string, unknown>[] }
  | { kind: 'ask'; boolean: boolean }
  | { kind: 'turtle'; body: string }
  | { kind: 'problem'; status: number; problem: Record<string, unknown> }

/** Stubs `/sparql` and the well-known document the session provider fetches on mount. */
function stub(answer: Answer) {
  const calls: string[] = []
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.includes('/.well-known')) {
      return new Response(JSON.stringify(wellKnown), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      })
    }
    if (url.includes('/sparql')) {
      calls.push(String(init?.body ?? ''))
      switch (answer.kind) {
        case 'select':
          return new Response(
            JSON.stringify({ head: { vars: answer.vars }, results: { bindings: answer.rows } }),
            { status: 200, headers: { 'content-type': 'application/sparql-results+json' } },
          )
        case 'ask':
          return new Response(JSON.stringify({ head: {}, boolean: answer.boolean }), {
            status: 200,
            headers: { 'content-type': 'application/sparql-results+json' },
          })
        case 'turtle':
          return new Response(answer.body, {
            status: 200,
            headers: { 'content-type': 'text/turtle; charset=utf-8' },
          })
        case 'problem':
          return new Response(JSON.stringify(answer.problem), {
            status: answer.status,
            headers: { 'content-type': 'application/problem+json' },
          })
      }
    }
    return new Response('{}', { status: 404, headers: { 'content-type': 'application/json' } })
  })
  vi.stubGlobal('fetch', fetchMock)
  return calls
}

function renderSparql() {
  return render(
    <MemoryRouter initialEntries={['/sparql']}>
      <SessionProvider>
        <Routes>
          <Route path="/sparql" element={<Sparql />} />
        </Routes>
      </SessionProvider>
    </MemoryRouter>,
  )
}

const uri = (value: string) => ({ type: 'uri', value })
const lit = (value: string, extra: Record<string, string> = {}) => ({ type: 'literal', value, ...extra })

async function runQuery() {
  await userEvent.click(screen.getByRole('button', { name: /run query/i }))
}

beforeEach(() => {
  stub({ kind: 'select', vars: [], rows: [] })
})
afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('Sparql', () => {
  it('renders a SELECT as a table with the columns the result set declares', async () => {
    stub({
      kind: 'select',
      vars: ['software', 'name', 'license', 'tagline'],
      rows: [
        {
          software: uri(`${BASE}/software/01a`),
          name: lit('shacl-manager'),
          license: uri('https://spdx.org/licenses/Apache-2.0'),
          // `tagline` is unbound in this row: an OPTIONAL that did not match.
        },
        {
          software: uri(`${BASE}/software/01b`),
          name: lit('RDFCraft'),
          license: uri('https://spdx.org/licenses/MIT'),
          tagline: lit('Mapping editor', { 'xml:lang': 'en' }),
        },
      ],
    })
    renderSparql()
    await runQuery()

    const table = await screen.findByRole('table')
    expect(within(table).getByRole('columnheader', { name: '?software' })).toBeInTheDocument()
    expect(within(table).getByRole('columnheader', { name: '?license' })).toBeInTheDocument()
    expect(within(table).getByText('shacl-manager')).toBeInTheDocument()
    expect(screen.getByText('2 rows')).toBeInTheDocument()

    // A local record links into the UI at its own route; a foreign IRI opens where it lives.
    expect(within(table).getByRole('link', { name: '/software/01a' })).toHaveAttribute(
      'href',
      '/software/01a',
    )
    expect(within(table).getByRole('link', { name: 'spdx.org/licenses/Apache-2.0' })).toHaveAttribute(
      'href',
      'https://spdx.org/licenses/Apache-2.0',
    )

    // A language-tagged literal says so; an unbound cell is `—`, never blank.
    expect(within(table).getByText('@en')).toBeInTheDocument()
    expect(within(table).getAllByText('—').length).toBeGreaterThan(0)
  })

  it('does not link a local IRI the UI has no screen for', async () => {
    stub({
      kind: 'select',
      vars: ['release'],
      rows: [{ release: uri(`${BASE}/release/01a`) }],
    })
    renderSparql()
    await runQuery()
    const cell = await screen.findByText('/release/01a')
    // Linking it would land the reader on "No such record here".
    expect(cell.closest('a')).toBeNull()
    expect(cell).toHaveAttribute('title', expect.stringContaining('no screen for this record'))
  })

  it('shows the datatype of a typed literal', async () => {
    stub({
      kind: 'select',
      vars: ['triples'],
      rows: [
        { triples: lit('558', { datatype: 'http://www.w3.org/2001/XMLSchema#integer' }) },
      ],
    })
    renderSparql()
    await runQuery()
    expect(await screen.findByText('558')).toBeInTheDocument()
    expect(screen.getByText('xsd:integer')).toBeInTheDocument()
  })

  it('renders an ASK as a yes/no, not as an empty table', async () => {
    stub({ kind: 'ask', boolean: true })
    renderSparql()
    await runQuery()
    expect(await screen.findByText('Yes')).toBeInTheDocument()
    expect(screen.queryByRole('table')).not.toBeInTheDocument()
    expect(screen.getByText(/returns a boolean, not bindings/)).toBeInTheDocument()
  })

  it('renders a false ASK as No, and does not confuse it with an error', async () => {
    stub({ kind: 'ask', boolean: false })
    renderSparql()
    await runQuery()
    expect(await screen.findByText('No')).toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('shows CONSTRUCT output as Turtle text', async () => {
    stub({ kind: 'turtle', body: '@prefix tar: <https://w3id.org/tar/ns#> .\n<urn:a> a tar:Software .\n' })
    renderSparql()
    await runQuery()
    expect(await screen.findByText(/<urn:a> a tar:Software \./)).toBeInTheDocument()
    expect(screen.queryByRole('table')).not.toBeInTheDocument()
  })

  it('reads an empty result set as empty rather than broken', async () => {
    stub({ kind: 'select', vars: ['s'], rows: [] })
    renderSparql()
    await runQuery()
    expect(await screen.findByText('0 rows')).toBeInTheDocument()
    expect(screen.getByText('The query ran and matched nothing')).toBeInTheDocument()
    // Empty is an answer, not a failure: nothing on screen may claim otherwise.
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    expect(screen.queryByText(/went wrong/i)).not.toBeInTheDocument()
  })

  it('names the empty default graph when a query that matched nothing never said GRAPH', async () => {
    stub({ kind: 'select', vars: ['s'], rows: [] })
    renderSparql()
    const editor = screen.getByLabelText('Query')
    await userEvent.clear(editor)
    await userEvent.type(editor, 'SELECT ?s WHERE {{ ?s ?p ?o }}')
    await runQuery()
    expect(await screen.findByText(/default graph here is empty/)).toBeInTheDocument()
  })

  it('surfaces the server’s own message when a write is refused', async () => {
    stub({
      kind: 'problem',
      status: 403,
      problem: {
        type: 'https://w3id.org/tar/problem/forbidden',
        title: 'Forbidden',
        status: 403,
        detail:
          'this endpoint is read-only; writes go through the REST API so they are validated and audited',
      },
    })
    renderSparql()
    await runQuery()
    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('Refused: this endpoint is read-only')
    expect(alert).toHaveTextContent(/writes go through the REST API so they are validated/)
  })

  it('shows the parser’s message for a syntax error, not a generic failure', async () => {
    stub({
      kind: 'problem',
      status: 400,
      problem: {
        type: 'https://w3id.org/tar/problem/bad-request',
        title: 'Bad request',
        status: 400,
        detail: 'SPARQL syntax error: unexpected end of file at line 1 column 23',
      },
    })
    renderSparql()
    await runQuery()
    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('The server could not parse this query')
    expect(alert).toHaveTextContent('unexpected end of file at line 1 column 23')
    expect(alert).not.toHaveTextContent('Something went wrong')
  })

  it('caps what it renders and says so, instead of locking the browser', async () => {
    stub({
      kind: 'select',
      vars: ['n'],
      rows: Array.from({ length: 1200 }, (_, i) => ({ n: lit(String(i)) })),
    })
    renderSparql()
    await runQuery()
    expect(await screen.findByText('1200 rows')).toBeInTheDocument()
    expect(screen.getByText(/Showing the first 500/)).toBeInTheDocument()
    expect(screen.getAllByRole('row')).toHaveLength(501) // 500 body rows + the header row
  })

  it('loads an example into the editor and runs it', async () => {
    const calls = stub({ kind: 'select', vars: ['graph'], rows: [{ graph: uri('urn:tar:local') }] })
    renderSparql()
    await userEvent.click(screen.getByRole('button', { name: /Named graphs and how big each one is/ }))
    await waitFor(() => expect(calls.length).toBe(1))
    expect(calls[0]).toContain('GRAPH ?graph')
    expect((screen.getByLabelText('Query') as HTMLTextAreaElement).value).toContain('GROUP BY ?graph')
  })

  it('runs on Ctrl+Enter', async () => {
    const calls = stub({ kind: 'ask', boolean: true })
    renderSparql()
    screen.getByLabelText('Query').focus()
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(calls.length).toBe(1))
  })

  it('puts the named graphs and the tar: prefix on screen for someone who has never seen the model', async () => {
    renderSparql()
    expect(await screen.findByText('urn:tar:local')).toBeInTheDocument()
    expect(screen.getByText('urn:tar:vocab')).toBeInTheDocument()
    expect(screen.getByText('https://w3id.org/tar/ns#')).toBeInTheDocument()
    expect(screen.getByText(/default graph is empty/)).toBeInTheDocument()
  })
})

describe('routeFor', () => {
  it('routes only the IRI kinds the SPA actually has a screen for', () => {
    expect(routeFor(`${BASE}/software/01a`, BASE)).toBe('/software/01a')
    expect(routeFor(`${BASE}/instance/01a`, BASE)).toBe('/instances/01a')
    expect(routeFor(`${BASE}/artifact/01a`, BASE)).toBe('/artifacts/01a')
    expect(routeFor(`${BASE}/run/01a`, BASE)).toBe('/runs/01a')
    // An artifact type has no page; the useful destination is what conforms to it.
    expect(routeFor(`${BASE}/type/rdf-graph`, BASE)).toBe(
      `/artifacts?conforms_to=${encodeURIComponent(`${BASE}/type/rdf-graph`)}`,
    )
    // No screen exists for these, so they must not become links that land on a 404.
    expect(routeFor(`${BASE}/release/01a`, BASE)).toBeUndefined()
    expect(routeFor(`${BASE}/distribution/01a`, BASE)).toBeUndefined()
    // A peer's record lives at its home registry, never at ours.
    expect(routeFor('https://reg.mumc.nl/software/09z', BASE)).toBeUndefined()
  })
})
