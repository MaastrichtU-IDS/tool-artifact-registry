import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'
import { api, setAuthToken } from './api'
import type { WellKnown, WhoAmI } from './types'

/**
 * Session handling.
 *
 * Two ways in, both ending in a bearer token:
 *
 *  - **OIDC authorisation code + PKCE** against the issuer the registry advertises. This is
 *    the same identity provider (Keycloak on `ids3`) that deployments authenticate to with
 *    `client_credentials`; humans get roles, workloads get a client id. Sign-in is hidden
 *    entirely when `/.well-known/tar-registry` reports no issuer (handoff §7).
 *  - **A registry API token**, pasted. The fallback for a registry running without any
 *    identity provider, which requirement 6 says must be possible.
 *
 * The token lives in `sessionStorage`: it disappears when the tab closes, and it is never put
 * in a cookie, so nothing here is exposed to CSRF.
 */

const TOKEN_KEY = 'tar.token'
/** Kept only so sign-out can tell the provider *which* session to end (`id_token_hint`). */
const ID_TOKEN_KEY = 'tar.id_token'
const VERIFIER_KEY = 'tar.pkce_verifier'
const STATE_KEY = 'tar.oidc_state'
const RETURN_KEY = 'tar.return_to'

/** The provider's OpenID configuration, fetched once per issuer per page load. */
const discoveries = new Map<string, Promise<OpenIdConfiguration>>()

interface OpenIdConfiguration {
  authorization_endpoint: string
  token_endpoint: string
  end_session_endpoint?: string
}

function discover(issuer: string): Promise<OpenIdConfiguration> {
  let p = discoveries.get(issuer)
  if (!p) {
    p = fetch(`${issuer}/.well-known/openid-configuration`).then((r) => {
      if (!r.ok) throw new Error(`${issuer} served no OpenID configuration (HTTP ${r.status})`)
      return r.json()
    })
    // A failed discovery must not be cached, or sign-in stays broken until a reload.
    p.catch(() => discoveries.delete(issuer))
    discoveries.set(issuer, p)
  }
  return p
}

interface SessionValue {
  registry?: WellKnown
  who?: WhoAmI
  loading: boolean
  signInWithOidc: () => Promise<void>
  signInWithToken: (token: string) => Promise<WhoAmI>
  signOut: () => void
  isCurator: boolean
  isAdmin: boolean
  /** True when this registry has an OIDC issuer to sign in against at all. */
  oidcAvailable: boolean
}

const SessionContext = createContext<SessionValue | null>(null)

export function SessionProvider({ children }: { children: ReactNode }) {
  const [registry, setRegistry] = useState<WellKnown>()
  const [who, setWho] = useState<WhoAmI>()
  const [loading, setLoading] = useState(true)

  const refreshWho = useCallback(async () => {
    try {
      const w = await api.whoami()
      setWho(w.authenticated ? w : undefined)
      return w
    } catch {
      setWho(undefined)
      return undefined
    }
  }, [])

  useEffect(() => {
    let cancelled = false
    ;(async () => {
      try {
        const wk = await api.wellKnown()
        if (!cancelled) setRegistry(wk)
      } catch {
        /* the registry description is optional for read-only browsing */
      }
      const stored = sessionStorage.getItem(TOKEN_KEY)
      if (stored) {
        setAuthToken(stored)
        await refreshWho()
      }
      if (!cancelled) setLoading(false)
    })()
    return () => {
      cancelled = true
    }
  }, [refreshWho])

  const signInWithToken = useCallback(
    async (token: string) => {
      setAuthToken(token)
      const w = await api.whoami()
      if (!w.authenticated) {
        setAuthToken(null)
        throw new Error('That credential was not accepted.')
      }
      sessionStorage.setItem(TOKEN_KEY, token)
      setWho(w)
      return w
    },
    [],
  )

  const signInWithOidc = useCallback(async () => {
    const oidc = registry?.auth?.oidc
    if (!oidc?.enabled || !oidc.issuer || !oidc.client_id) {
      throw new Error('This registry has no OIDC issuer configured.')
    }
    const conf = await discover(oidc.issuer)
    const verifier = randomString(64)
    const state = randomString(32)
    sessionStorage.setItem(VERIFIER_KEY, verifier)
    sessionStorage.setItem(STATE_KEY, state)
    // Coming back to /auth/callback would re-run the exchange with a spent code, so a
    // sign-in started *from* the callback returns to the front page instead.
    const here = window.location.pathname + window.location.search
    sessionStorage.setItem(RETURN_KEY, here.startsWith('/auth/callback') ? '/' : here)
    const challenge = await pkceChallenge(verifier)
    const params = new URLSearchParams({
      response_type: 'code',
      client_id: oidc.client_id,
      redirect_uri: `${window.location.origin}/auth/callback`,
      scope: 'openid profile email',
      code_challenge: challenge,
      code_challenge_method: 'S256',
      state,
    })
    if (oidc.audience) params.set('audience', oidc.audience)
    window.location.href = `${conf.authorization_endpoint}?${params}`
  }, [registry])

  const signOut = useCallback(() => {
    const oidc = registry?.auth?.oidc
    const idToken = sessionStorage.getItem(ID_TOKEN_KEY)
    sessionStorage.removeItem(TOKEN_KEY)
    sessionStorage.removeItem(ID_TOKEN_KEY)
    setAuthToken(null)
    setWho(undefined)
    // Dropping our copy of the token is not signing out. The provider still holds an SSO
    // session, so the next "Sign in" would come straight back — as the *previous* person,
    // without ever asking for a password. On a shared machine that hands one user's
    // curator rights to the next. RP-initiated logout (OIDC RP-Initiated Logout 1.0) ends
    // the session at the provider too.
    if (oidc?.issuer && oidc.client_id && idToken) {
      discover(oidc.issuer)
        .then((conf) => {
          if (!conf.end_session_endpoint) return
          const p = new URLSearchParams({
            id_token_hint: idToken,
            client_id: oidc.client_id!,
            post_logout_redirect_uri: `${window.location.origin}/`,
          })
          window.location.href = `${conf.end_session_endpoint}?${p}`
        })
        .catch(() => {
          /* Local sign-out already happened; a provider we cannot reach is not fatal. */
        })
    }
  }, [registry])

  const value = useMemo<SessionValue>(
    () => ({
      registry,
      who,
      loading,
      signInWithOidc,
      signInWithToken,
      signOut,
      isCurator: who?.is_curator ?? false,
      isAdmin: who?.is_admin ?? false,
      oidcAvailable: registry?.auth?.oidc?.human_signin ?? false,
    }),
    [registry, who, loading, signInWithOidc, signInWithToken, signOut],
  )

  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>
}

export function useSession(): SessionValue {
  const ctx = useContext(SessionContext)
  if (!ctx) throw new Error('useSession outside SessionProvider')
  return ctx
}

export interface OidcTokens {
  accessToken: string
  /** Only used as `id_token_hint` when signing out. */
  idToken?: string
}

/**
 * Exchange the authorisation code for a token; called by the /auth/callback route.
 *
 * An authorisation code is single-use: a second exchange of the same code makes the provider
 * reject it *and* (per RFC 6749 §4.1.2) revoke the tokens already issued for it. React can
 * easily run an effect twice — StrictMode does it deliberately in development — so the
 * exchange is memoised per code rather than merely guarded inside a component.
 */
const exchanges = new Map<string, Promise<OidcTokens>>()

export function completeOidcSignIn(
  issuer: string,
  clientId: string,
  code: string,
  state: string | null,
): Promise<OidcTokens> {
  let p = exchanges.get(code)
  if (!p) {
    p = exchangeCode(issuer, clientId, code, state)
    exchanges.set(code, p)
  }
  return p
}

async function exchangeCode(
  issuer: string,
  clientId: string,
  code: string,
  state: string | null,
): Promise<OidcTokens> {
  const expectedState = sessionStorage.getItem(STATE_KEY)
  const verifier = sessionStorage.getItem(VERIFIER_KEY)
  // Both are cleared whatever happens: a spent or rejected code must not be retried with
  // the same verifier, and a stale one left behind breaks the next attempt.
  sessionStorage.removeItem(STATE_KEY)
  sessionStorage.removeItem(VERIFIER_KEY)

  // Fail closed. `state` and the PKCE verifier live in this tab's sessionStorage; if they
  // are absent this redirect did not come from a sign-in *we* started, and an unsolicited
  // code must not be exchanged (OAuth 2.0 Security BCP §4.7, authorization-code injection).
  if (!verifier || !expectedState) {
    throw new Error(
      'This sign-in did not start in this browser tab, so it cannot be completed. Press Sign in again.',
    )
  }
  if (state !== expectedState) {
    throw new Error('The sign-in state did not match. Press Sign in again.')
  }

  const conf = await discover(issuer)
  const body = new URLSearchParams({
    grant_type: 'authorization_code',
    client_id: clientId,
    code,
    redirect_uri: `${window.location.origin}/auth/callback`,
    code_verifier: verifier,
  })
  const resp = await fetch(conf.token_endpoint, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body,
  })
  const json = await resp.json().catch(() => ({}) as Record<string, string>)
  if (!resp.ok) {
    // Keycloak explains itself in the body; `HTTP 400` on its own tells nobody anything.
    throw new Error(json.error_description || json.error || `token endpoint returned ${resp.status}`)
  }
  if (!json.access_token) throw new Error('The token response carried no access_token.')
  return { accessToken: json.access_token as string, idToken: json.id_token as string | undefined }
}

export function consumeReturnTo(): string {
  const to = sessionStorage.getItem(RETURN_KEY) || '/'
  sessionStorage.removeItem(RETURN_KEY)
  return to
}

export function storeToken(tokens: OidcTokens) {
  sessionStorage.setItem(TOKEN_KEY, tokens.accessToken)
  if (tokens.idToken) sessionStorage.setItem(ID_TOKEN_KEY, tokens.idToken)
  setAuthToken(tokens.accessToken)
}

function randomString(len: number): string {
  const bytes = new Uint8Array(len)
  crypto.getRandomValues(bytes)
  return Array.from(bytes, (b) => 'abcdefghijklmnopqrstuvwxyz0123456789'[b % 36]).join('')
}

async function pkceChallenge(verifier: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier))
  return btoa(String.fromCharCode(...new Uint8Array(digest)))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '')
}
