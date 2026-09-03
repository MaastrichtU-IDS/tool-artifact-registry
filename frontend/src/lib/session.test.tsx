import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  completeOidcSignIn,
  forgetToken,
  renewAccessToken,
  secondsUntilExpiry,
  storeToken,
} from './session'

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

/**
 * Renewal. The property that matters most here is a negative one — the refresh token must not
 * reach any persistent store — so it is asserted directly rather than inferred.
 */
describe('silent renewal', () => {
  const jwtWithExp = (exp: number) =>
    `header.${btoa(JSON.stringify({ exp })).replace(/=+$/, '')}.signature`

  it('reads the expiry from the access token rather than trusting a relative number', () => {
    const now = Math.floor(Date.now() / 1000)
    expect(secondsUntilExpiry(jwtWithExp(now + 300))).toBeGreaterThan(290)
    expect(secondsUntilExpiry(jwtWithExp(now + 300))).toBeLessThanOrEqual(300)
    // An already-expired token reports a negative number rather than nothing, so the caller
    // can tell "expired" from "cannot tell" and renew immediately.
    expect(secondsUntilExpiry(jwtWithExp(now - 60))).toBeLessThan(0)
  })

  it('says nothing rather than guessing when the token carries no readable expiry', () => {
    // A registry API token is not a JWT at all, and must not throw on the way past.
    expect(secondsUntilExpiry('tar_abcdef0123456789')).toBeUndefined()
    expect(secondsUntilExpiry('header.!!!not-base64!!!.sig')).toBeUndefined()
    expect(secondsUntilExpiry(`header.${btoa('{"sub":"u"}')}.sig`)).toBeUndefined()
  })

  it('keeps the refresh token out of every persistent store', () => {
    storeToken({ accessToken: 'at-9', idToken: 'it-9', refreshToken: 'rt-secret', expiresIn: 300 })
    // The access token and id token are kept, as before.
    expect(sessionStorage.getItem('tar.token')).toBe('at-9')
    expect(sessionStorage.getItem('tar.id_token')).toBe('it-9')
    // The refresh token is the long-lived half, and it is nowhere a later tab could find it.
    const everywhere = [
      ...Object.values(sessionStorage),
      ...Object.values(localStorage),
      document.cookie,
    ].join(' ')
    expect(everywhere).not.toContain('rt-secret')
  })

  it('renews with the refresh token and keeps the rotated one', async () => {
    storeToken({ accessToken: 'at-1', refreshToken: 'rt-1' })
    tokenResponse = { ok: true, status: 200, body: { access_token: 'at-2', refresh_token: 'rt-2' } }

    expect(await renewAccessToken(ISSUER, 'tar-ui')).toBe('at-2')
    expect(tokenCalls).toHaveLength(1)
    expect(tokenCalls[0].get('grant_type')).toBe('refresh_token')
    expect(tokenCalls[0].get('refresh_token')).toBe('rt-1')
    expect(sessionStorage.getItem('tar.token')).toBe('at-2')

    // The next renewal must present the *rotated* token, or a provider with rotation on
    // refuses it and the session dies one renewal early.
    tokenResponse = { ok: true, status: 200, body: { access_token: 'at-3' } }
    expect(await renewAccessToken(ISSUER, 'tar-ui')).toBe('at-3')
    expect(tokenCalls[1].get('refresh_token')).toBe('rt-2')
  })

  it('gives up and forgets a refresh token the provider has refused', async () => {
    storeToken({ accessToken: 'at-1', refreshToken: 'rt-dead' })
    tokenResponse = { ok: false, status: 400, body: { error: 'invalid_grant' } }

    expect(await renewAccessToken(ISSUER, 'tar-ui')).toBeNull()
    expect(tokenCalls).toHaveLength(1)
    // And does not ask again with the token it already knows is spent.
    expect(await renewAccessToken(ISSUER, 'tar-ui')).toBeNull()
    expect(tokenCalls).toHaveLength(1)
  })

  it('cannot renew a pasted API token, and does not try', async () => {
    forgetToken()
    expect(await renewAccessToken(ISSUER, 'tar-ui')).toBeNull()
    expect(tokenCalls).toHaveLength(0)
  })
})
