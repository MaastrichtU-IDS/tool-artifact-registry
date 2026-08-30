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
const VERIFIER_KEY = 'tar.pkce_verifier'
const RETURN_KEY = 'tar.return_to'

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
    const conf = await fetch(`${oidc.issuer}/.well-known/openid-configuration`).then((r) => r.json())
    const verifier = randomString(64)
    sessionStorage.setItem(VERIFIER_KEY, verifier)
    sessionStorage.setItem(RETURN_KEY, window.location.pathname + window.location.search)
    const challenge = await pkceChallenge(verifier)
    const params = new URLSearchParams({
      response_type: 'code',
      client_id: oidc.client_id,
      redirect_uri: `${window.location.origin}/auth/callback`,
      scope: 'openid profile email',
      code_challenge: challenge,
      code_challenge_method: 'S256',
      state: randomString(16),
    })
    if (oidc.audience) params.set('audience', oidc.audience)
    window.location.href = `${conf.authorization_endpoint}?${params}`
  }, [registry])

  const signOut = useCallback(() => {
    sessionStorage.removeItem(TOKEN_KEY)
    setAuthToken(null)
    setWho(undefined)
  }, [])

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

/** Exchange the authorisation code for a token; called by the /auth/callback route. */
export async function completeOidcSignIn(issuer: string, clientId: string, code: string): Promise<string> {
  const conf = await fetch(`${issuer}/.well-known/openid-configuration`).then((r) => r.json())
  const verifier = sessionStorage.getItem(VERIFIER_KEY) ?? ''
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
  if (!resp.ok) throw new Error(`token endpoint returned ${resp.status}`)
  const json = await resp.json()
  sessionStorage.removeItem(VERIFIER_KEY)
  return json.access_token as string
}

export function consumeReturnTo(): string {
  const to = sessionStorage.getItem(RETURN_KEY) || '/'
  sessionStorage.removeItem(RETURN_KEY)
  return to
}

export function storeToken(token: string) {
  sessionStorage.setItem(TOKEN_KEY, token)
  setAuthToken(token)
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
