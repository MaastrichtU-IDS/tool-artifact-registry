import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { completeOidcSignIn, consumeReturnTo, storeToken, useSession } from '../lib/session'
import { Skeleton } from '../components/common'

/** OIDC redirect landing. The code is exchanged in the browser with PKCE, so the registry
 *  never needs a client secret for the UI. */
export default function AuthCallback() {
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const { registry } = useSession()
  const [error, setError] = useState<string>()

  useEffect(() => {
    const code = params.get('code')
    const oidcError = params.get('error')
    if (oidcError) {
      setError(params.get('error_description') || oidcError)
      return
    }
    if (!code) {
      setError('This page is the sign-in landing point; it was reached without an authorisation code.')
      return
    }
    if (!registry?.auth?.oidc?.issuer || !registry.auth.oidc.client_id) return
    completeOidcSignIn(
      registry.auth.oidc.issuer,
      registry.auth.oidc.client_id,
      code,
      params.get('state'),
    )
      .then((tokens) => {
        storeToken(tokens)
        // A full load, not a client-side navigate: it drops the spent code out of the URL
        // (and out of history) and remounts the session provider, which picks the stored
        // token up. Navigating and *then* reloading raced, and could reload the callback
        // URL — replaying a code the provider had already burned.
        window.location.replace(consumeReturnTo())
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
  }, [params, registry, navigate])

  if (error) {
    return (
      <div className="banner danger" role="alert">
        <h3>Sign-in failed</h3>
        <p>{error}</p>
        <div className="actions">
          <button type="button" onClick={() => navigate('/software')}>Continue without signing in</button>
        </div>
      </div>
    )
  }
  return <Skeleton rows={3} />
}
