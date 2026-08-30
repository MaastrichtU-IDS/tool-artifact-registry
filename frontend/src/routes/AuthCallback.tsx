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
    if (!code || !registry?.auth?.oidc?.issuer || !registry.auth.oidc.client_id) return
    completeOidcSignIn(registry.auth.oidc.issuer, registry.auth.oidc.client_id, code)
      .then((token) => {
        storeToken(token)
        navigate(consumeReturnTo(), { replace: true })
        // The session provider reads the stored token on mount.
        window.location.reload()
      })
      .catch((e) => setError(String(e)))
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
