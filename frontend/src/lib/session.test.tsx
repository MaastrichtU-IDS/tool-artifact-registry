import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { completeOidcSignIn } from './session'

/**
 * The authorisation-code half of sign-in, unit-tested against a stub token endpoint.
 *
 * Every case here is a bug that a real Keycloak found, or would have: a code exchanged twice
 * (React runs effects twice in development), an unsolicited code accepted because `state` was
 * generated and then never checked, and a provider error reduced to "HTTP 400".
 */

const ISSUER = 'http://127.0.0.1:8090/realms/tar'
const DISCOVERY = {
  authorization_endpoint: `${ISSUER}/protocol/openid-connect/auth`,
  token_endpoint: `${ISSUER}/protocol/openid-connect/token`,
  end_session_endpoint: `${ISSUER}/protocol/openid-connect/logout`,
}

let tokenCalls: URLSearchParams[]
let tokenResponse: { ok: boolean; status: number; body: unknown }

function stubFetch() {
  return vi.fn(async (url: string, init?: RequestInit) => {
    if (String(url).includes('.well-known/openid-configuration')) {
      return { ok: true, status: 200, json: async () => DISCOVERY } as unknown as Response
    }
    if (String(url) === DISCOVERY.token_endpoint) {
      tokenCalls.push(new URLSearchParams(String(init?.body)))
      return {
        ok: tokenResponse.ok,
        status: tokenResponse.status,
        json: async () => tokenResponse.body,
      } as unknown as Response
    }
    throw new Error(`unexpected fetch ${url}`)
  })
}

beforeEach(() => {
  tokenCalls = []
  tokenResponse = { ok: true, status: 200, body: { access_token: 'at-1', id_token: 'it-1' } }
  sessionStorage.clear()
  sessionStorage.setItem('tar.pkce_verifier', 'verifier-abc')
  sessionStorage.setItem('tar.oidc_state', 'state-abc')
  vi.stubGlobal('fetch', stubFetch())
})
afterEach(() => vi.unstubAllGlobals())

describe('completeOidcSignIn', () => {
  it('exchanges the code with the PKCE verifier and returns both tokens', async () => {
    const tokens = await completeOidcSignIn(ISSUER, 'tar-ui', 'code-1', 'state-abc')
    expect(tokens).toEqual({ accessToken: 'at-1', idToken: 'it-1' })
    expect(tokenCalls).toHaveLength(1)
    expect(tokenCalls[0].get('grant_type')).toBe('authorization_code')
    expect(tokenCalls[0].get('client_id')).toBe('tar-ui')
    expect(tokenCalls[0].get('code_verifier')).toBe('verifier-abc')
    // A public client sends no secret; the verifier is what proves it started this flow.
    expect(tokenCalls[0].get('client_secret')).toBeNull()
  })

  it('exchanges a given code exactly once, however often the effect runs', async () => {
    const [a, b] = await Promise.all([
      completeOidcSignIn(ISSUER, 'tar-ui', 'code-once', 'state-abc'),
      completeOidcSignIn(ISSUER, 'tar-ui', 'code-once', 'state-abc'),
    ])
    expect(a).toEqual(b)
    expect(tokenCalls).toHaveLength(1)
  })

  it('refuses a code whose state does not match the one it issued', async () => {
    await expect(completeOidcSignIn(ISSUER, 'tar-ui', 'code-2', 'not-the-state')).rejects.toThrow(
      /state did not match/i,
    )
    expect(tokenCalls).toHaveLength(0)
  })

  it('refuses an unsolicited code when this tab started no sign-in', async () => {
    sessionStorage.clear()
    await expect(completeOidcSignIn(ISSUER, 'tar-ui', 'code-3', 'state-abc')).rejects.toThrow(
      /did not start in this browser tab/i,
    )
    expect(tokenCalls).toHaveLength(0)
  })

  it('clears the verifier and state so a spent code cannot be replayed', async () => {
    await completeOidcSignIn(ISSUER, 'tar-ui', 'code-4', 'state-abc')
    expect(sessionStorage.getItem('tar.pkce_verifier')).toBeNull()
    expect(sessionStorage.getItem('tar.oidc_state')).toBeNull()
  })

  it("surfaces the provider's own explanation rather than a bare status code", async () => {
    tokenResponse = {
      ok: false,
      status: 400,
      body: { error: 'invalid_grant', error_description: 'Code not valid' },
    }
    await expect(completeOidcSignIn(ISSUER, 'tar-ui', 'code-5', 'state-abc')).rejects.toThrow(
      'Code not valid',
    )
  })
})
