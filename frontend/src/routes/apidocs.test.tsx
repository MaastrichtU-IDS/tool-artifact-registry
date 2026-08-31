import { describe, expect, it, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { ApiDocs } from '../components/ApiDocs'

const OPENAPI = {
  openapi: '3.1.0',
  info: { title: 'OntoExplorer API', version: '2.4.0' },
  servers: [{ url: 'https://onto.example.org/api' }],
  paths: {
    '/ontologies': {
      // Declared on the path, so it applies to every operation under it.
      parameters: [{ name: 'page', in: 'query', description: 'Zero-based page number' }],
      get: {
        summary: 'List ontologies',
        tags: ['Ontologies'],
        responses: { '200': { description: 'A page of ontologies' } },
      },
      post: { summary: 'Ingest an ontology', tags: ['Ontologies'], responses: { '201': {} } },
    },
    '/ontologies/{id}': {
      get: {
        summary: 'One ontology',
        tags: ['Ontologies'],
        parameters: [{ name: 'id', in: 'path', required: true }],
        responses: { '200': {}, '404': { description: 'No such ontology' } },
      },
      delete: { summary: 'Remove it', tags: ['Admin'], deprecated: true, responses: { '204': {} } },
    },
  },
}

beforeEach(() => {
  vi.restoreAllMocks()
})

describe('ApiDocs', () => {
  it('links out without fetching anything until asked', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch')
    render(
      <ApiDocs
        softwareId="01a"
        docs={[{ url: 'https://onto.example.org/openapi.json', format: 'openapi', title: 'REST API' }]}
      />,
    )
    expect(screen.getByRole('link', { name: /open document/i })).toHaveAttribute(
      'href',
      'https://onto.example.org/openapi.json',
    )
    // A software page can carry several of these; fetching them all on render would be a
    // burst of requests for documents nobody has asked to read.
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it('renders the operations, grouped by tag, through the registry rather than direct', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify(OPENAPI), { status: 200 }),
    )
    render(
      <ApiDocs softwareId="01a" docs={[{ url: 'https://onto.example.org/openapi.json', format: 'openapi' }]} />,
    )
    await userEvent.click(screen.getByRole('button', { name: /show operations/i }))

    await waitFor(() => expect(screen.getByText(/4 operations/)).toBeInTheDocument())
    // Fetched via the registry: most openapi.json are served without CORS headers, so a direct
    // browser fetch would fail on the majority of records.
    expect(fetchMock.mock.calls[0][0]).toBe('/api/v1/software/01a/api-doc?n=0')

    expect(screen.getByRole('heading', { name: 'Ontologies' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Admin' })).toBeInTheDocument()
    expect(screen.getAllByText('GET')).toHaveLength(2)
    expect(screen.getAllByText('/ontologies/{id}')).toHaveLength(2)
    expect(screen.getByText('deprecated')).toBeInTheDocument()
  })

  it('shows a path-level parameter on the operations that inherit it', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify(OPENAPI), { status: 200 }),
    )
    render(
      <ApiDocs softwareId="01a" docs={[{ url: 'https://onto.example.org/openapi.json', format: 'openapi' }]} />,
    )
    await userEvent.click(screen.getByRole('button', { name: /show operations/i }))
    await waitFor(() => expect(screen.getByText('List ontologies')).toBeInTheDocument())
    await userEvent.click(screen.getByRole('button', { name: /list ontologies/i }))
    expect(screen.getByText('page')).toBeInTheDocument()
    expect(screen.getByText('Zero-based page number')).toBeInTheDocument()
  })

  it('says plainly when the document is not JSON instead of showing an empty API', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response('openapi: 3.1.0\ninfo:\n  title: YAML API\n', { status: 200 }),
    )
    render(
      <ApiDocs softwareId="01a" docs={[{ url: 'https://onto.example.org/openapi.yaml', format: 'openapi' }]} />,
    )
    await userEvent.click(screen.getByRole('button', { name: /show operations/i }))
    await waitFor(() => expect(screen.getByText(/not JSON/i)).toBeInTheDocument())
    // The link survives the failure — the reader can still go and read it.
    expect(screen.getAllByRole('link').some((a) => a.getAttribute('href')?.endsWith('.yaml'))).toBe(true)
  })

  it('offers no operation list for a format it cannot interpret', () => {
    render(
      <ApiDocs
        softwareId="01a"
        docs={[{ url: 'https://onto.example.org/sparql', format: 'sparql-service-description' }]}
      />,
    )
    expect(screen.getByText('SPARQL service description')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /show operations/i })).not.toBeInTheDocument()
    expect(screen.getByRole('link', { name: /open document/i })).toBeInTheDocument()
  })
})
