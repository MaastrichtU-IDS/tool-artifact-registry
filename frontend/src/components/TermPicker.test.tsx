import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { TermPicker } from './TermPicker'

/** One fetch stub for both routes the picker uses: the search, and the type registration. */
function stubFetch(hits: unknown[], onCreate?: (body: Record<string, unknown>) => unknown) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.startsWith('/api/v1/vocab/search')) {
      return new Response(JSON.stringify({ items: hits, total: hits.length }), { status: 200 })
    }
    if (url.startsWith('/api/v1/vocab/resolve')) {
      return new Response('[]', { status: 200 })
    }
    if (url === '/api/v1/types') {
      const body = JSON.parse(String(init?.body ?? '{}'))
      const created = onCreate?.(body)
      if (!created) return new Response(JSON.stringify({ detail: 'curator role required' }), { status: 403 })
      return new Response(JSON.stringify(created), { status: 201 })
    }
    throw new Error(`unexpected fetch ${url}`)
  })
}

describe('TermPicker', () => {
  beforeEach(() => vi.stubGlobal('fetch', stubFetch([])))
  afterEach(() => vi.unstubAllGlobals())

  it('offers to register a type when nothing matches, rather than leaving the curator stuck', async () => {
    const created = vi.fn(() => ({ iri: 'https://reg.test/type/patch-log', label: 'patch log' }))
    vi.stubGlobal('fetch', stubFetch([], created))
    const onChange = vi.fn()
    render(<TermPicker id="produces" label="Produces" branch="data" value={[]} onChange={onChange} />)

    await userEvent.type(screen.getByLabelText('Produces'), 'patch log')
    const offer = await screen.findByText('Add “patch log” as a type')
    await userEvent.click(offer)

    await waitFor(() => expect(onChange).toHaveBeenCalledWith(['https://reg.test/type/patch-log']))
    expect(created).toHaveBeenCalledWith({ label: 'patch log' })
  })

  it('adopts a pasted IRI under its own identifier instead of minting a second name for it', async () => {
    const created = vi.fn((body: Record<string, unknown>) => ({ iri: body.iri, label: body.label }))
    vi.stubGlobal('fetch', stubFetch([], created))
    const onChange = vi.fn()
    render(<TermPicker id="produces" label="Produces" branch="data" value={[]} onChange={onChange} />)

    await userEvent.type(screen.getByLabelText('Produces'), 'http://purl.obolibrary.org/obo/SWO_0000001')
    await userEvent.click(await screen.findByText('Adopt this IRI as a type'))

    await waitFor(() =>
      expect(onChange).toHaveBeenCalledWith(['http://purl.obolibrary.org/obo/SWO_0000001']),
    )
    expect(created).toHaveBeenCalledWith({
      iri: 'http://purl.obolibrary.org/obo/SWO_0000001',
      label: 'SWO 0000001',
    })
  })

  it('does not offer to register beside a real match — reusing the right term must be the easy path', async () => {
    vi.stubGlobal(
      'fetch',
      stubFetch([{ iri: 'http://edamontology.org/data_2048', label: 'Report', source: 'edam' }]),
    )
    render(<TermPicker id="produces" label="Produces" branch="data" value={[]} onChange={vi.fn()} />)
    await userEvent.type(screen.getByLabelText('Produces'), 'report')
    expect(await screen.findByText('Report')).toBeInTheDocument()
    expect(screen.queryByText(/as a type$/)).not.toBeInTheDocument()
  })

  it('does not offer to register a topic: this registry does not own that vocabulary', async () => {
    render(<TermPicker id="topics" label="Topics" branch="topic" value={[]} onChange={vi.fn()} />)
    await userEvent.type(screen.getByLabelText('Topics'), 'semantic web')
    await waitFor(() => expect(screen.queryByText(/as a type/)).not.toBeInTheDocument())
  })

  it('says why a registration failed rather than silently adding nothing', async () => {
    const onChange = vi.fn()
    render(<TermPicker id="produces" label="Produces" branch="data" value={[]} onChange={onChange} />)
    await userEvent.type(screen.getByLabelText('Produces'), 'patch log')
    await userEvent.click(await screen.findByText('Add “patch log” as a type'))
    expect(await screen.findByRole('alert')).toHaveTextContent('curator role required')
    expect(onChange).not.toHaveBeenCalled()
  })
})
