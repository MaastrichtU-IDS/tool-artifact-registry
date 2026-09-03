import { afterEach, describe, expect, it, vi } from 'vitest'
import { api, ApiError, qs, setAuthToken, setTokenRenewer } from './api'

describe('qs', () => {
  it('drops empty and false values so the URL stays readable', () => {
    expect(qs({ q: 'shacl', license: undefined, federated: false, limit: 25 })).toBe('?q=shacl&limit=25')
  })
  it('returns nothing when every parameter is empty', () => {
    expect(qs({ q: '', cursor: undefined })).toBe('')
  })
})

describe('ApiError.fieldErrors', () => {
  it('maps a SHACL validation report back onto form fields', () => {
    const report = `
[] a sh:ValidationReport ;
    sh:conforms false ;
    sh:result [
        a sh:ValidationResult ;
        sh:resultSeverity sh:Violation ;
        sh:focusNode "urn:new" ;
        sh:resultPath <https://schema.org/name> ;
        sh:sourceConstraintComponent sh:MinCountConstraintComponent ;
        tar:jsonField "name" ;
        sh:resultMessage "Software needs a name"
    ] ;
    sh:result [
        a sh:ValidationResult ;
        sh:resultSeverity sh:Violation ;
        sh:focusNode "urn:new" ;
        sh:resultPath <https://w3id.org/tar/ns#kind> ;
        sh:sourceConstraintComponent sh:InConstraintComponent ;
        tar:jsonField "kind" ;
        sh:resultMessage "kind must be one of service, library, cli, workflow"
    ] .`
    const err = new ApiError(
      { type: 'x', title: 'Write rejected by SHACL validation', status: 422, report },
      422,
    )
    const fields = err.fieldErrors()
    expect(fields.name).toBe('Software needs a name')
    expect(fields.kind).toMatch(/must be one of service/)
  })

  it('returns nothing when the problem carries no report', () => {
    const err = new ApiError({ type: 'x', title: 'Forbidden', status: 403 }, 403)
    expect(err.fieldErrors()).toEqual({})
  })
})

describe('token renewal', () => {
  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

  afterEach(() => {
    setTokenRenewer(null)
    setAuthToken(null)
    vi.unstubAllGlobals()
  })

  it('renews once on a 401 and retries the request with the new token', async () => {
    setAuthToken('expired')
    const sent: (string | null)[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_path: string, init: RequestInit) => {
        const auth = new Headers(init.headers).get('authorization')
        sent.push(auth)
        return auth === 'Bearer fresh' ? json({ authenticated: true }) : json({ title: 'nope' }, 401)
      }),
    )
    setTokenRenewer(async () => {
      setAuthToken('fresh')
      return 'fresh'
    })

    await expect(api.whoami()).resolves.toEqual({ authenticated: true })
    expect(sent).toEqual(['Bearer expired', 'Bearer fresh'])
  })

  it('gives up after one retry rather than looping on a 401 renewal cannot fix', async () => {
    setAuthToken('a-token')
    const fetchMock = vi.fn(async () => json({ title: 'still no' }, 401))
    vi.stubGlobal('fetch', fetchMock)
    // Renewal "succeeds" — the token is simply not the problem.
    setTokenRenewer(async () => 'another')

    await expect(api.whoami()).rejects.toBeInstanceOf(ApiError)
    expect(fetchMock).toHaveBeenCalledTimes(2)
  })

  it('renews once for a burst of expired requests, not once each', async () => {
    // The rotation trap: six panels expire together, and a provider that rotates refresh
    // tokens accepts the first renewal and refuses the rest. They must share one.
    setAuthToken('expired')
    let renewals = 0
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_path: string, init: RequestInit) => {
        const auth = new Headers(init.headers).get('authorization')
        return auth === 'Bearer fresh' ? json({ authenticated: true }) : json({ title: 'nope' }, 401)
      }),
    )
    setTokenRenewer(async () => {
      renewals += 1
      await new Promise((r) => setTimeout(r, 5))
      setAuthToken('fresh')
      return 'fresh'
    })

    const all = await Promise.all([api.whoami(), api.whoami(), api.whoami(), api.whoami()])
    expect(all).toHaveLength(4)
    expect(renewals).toBe(1)
  })

  it('lets the 401 stand when there is nothing to renew with', async () => {
    setAuthToken('a-pasted-api-token')
    const fetchMock = vi.fn(async () => json({ title: 'no' }, 401))
    vi.stubGlobal('fetch', fetchMock)
    setTokenRenewer(async () => null)

    await expect(api.whoami()).rejects.toBeInstanceOf(ApiError)
    // One renewal attempt, and no retry, because it had nothing to offer.
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('does not try to renew a request that was never authenticated', async () => {
    setAuthToken(null)
    const fetchMock = vi.fn(async () => json({ title: 'no' }, 401))
    vi.stubGlobal('fetch', fetchMock)
    let asked = false
    setTokenRenewer(async () => {
      asked = true
      return 'x'
    })

    await expect(api.whoami()).rejects.toBeInstanceOf(ApiError)
    expect(asked).toBe(false)
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })
})
