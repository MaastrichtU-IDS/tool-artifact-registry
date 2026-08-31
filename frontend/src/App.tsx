import { useState } from 'react'
import { NavLink, Navigate, Route, Routes, useNavigate } from 'react-router-dom'
import { Modal } from './components/common'
import { useSession } from './lib/session'
import SoftwareList from './routes/SoftwareList'
import SoftwareDetail from './routes/SoftwareDetail'
import SoftwareForm from './routes/SoftwareForm'
import InstanceList from './routes/InstanceList'
import InstanceDetail from './routes/InstanceDetail'
import InstanceForm from './routes/InstanceForm'
import Tokens from './routes/Tokens'
import SoftwareTokens from './routes/SoftwareTokens'
import Subscriptions from './routes/Subscriptions'
import ArtifactList from './routes/ArtifactList'
import ArtifactDetail from './routes/ArtifactDetail'
import RunList from './routes/RunList'
import RunDetail from './routes/RunDetail'
import Peers from './routes/Peers'
import Search from './routes/Search'
import Sparql from './routes/Sparql'
import AuthCallback from './routes/AuthCallback'
import NotFound from './routes/NotFound'

export default function App() {
  return (
    <>
      <a className="skip-link" href="#main">Skip to content</a>
      <Header />
      <main id="main">
        <Routes>
          <Route path="/" element={<Navigate to="/software" replace />} />
          <Route path="/software" element={<SoftwareList />} />
          <Route path="/software/new" element={<SoftwareForm />} />
          <Route path="/software/:id" element={<SoftwareDetail />} />
          <Route path="/software/:id/edit" element={<SoftwareForm />} />
          <Route path="/instances" element={<InstanceList />} />
          <Route path="/instances/new" element={<InstanceForm />} />
          <Route path="/instances/:id" element={<InstanceDetail />} />
          <Route path="/instance/:id" element={<InstanceDetail />} />
          <Route path="/instances/:id/edit" element={<InstanceForm />} />
          <Route path="/instances/:id/tokens" element={<Tokens />} />
          <Route path="/software/:id/tokens" element={<SoftwareTokens />} />
          <Route path="/instances/:id/subscriptions" element={<Subscriptions />} />
          <Route path="/artifacts" element={<ArtifactList />} />
          <Route path="/artifacts/:id" element={<ArtifactDetail />} />
          <Route path="/artifact/:id" element={<ArtifactDetail />} />
          <Route path="/runs" element={<RunList />} />
          <Route path="/runs/:id" element={<RunDetail />} />
          <Route path="/run/:id" element={<RunDetail />} />
          <Route path="/peers" element={<Peers />} />
          <Route path="/search" element={<Search />} />
          <Route path="/sparql" element={<Sparql />} />
          <Route path="/auth/callback" element={<AuthCallback />} />
          <Route path="*" element={<NotFound />} />
        </Routes>
      </main>
    </>
  )
}

function Header() {
  const { registry, who, isAdmin, signOut } = useSession()
  const navigate = useNavigate()
  const [q, setQ] = useState('')

  return (
    <header className="app-header">
      <div className="app-header-inner">
        <NavLink to="/software" className="brand">
          {registry?.title ?? 'Tool Artifact Registry'}
          {registry?.base_iri && <small>{registry.base_iri.replace(/^https?:\/\//, '')}</small>}
        </NavLink>
        <nav className="tabs" aria-label="Sections">
          <NavLink to="/software">Software</NavLink>
          <NavLink to="/instances">Instances</NavLink>
          <NavLink to="/artifacts">Artifacts</NavLink>
          <NavLink to="/runs">Runs</NavLink>
          <NavLink to="/sparql">SPARQL</NavLink>
          {/* Peers is admin-only, and absent rather than disabled for everyone else. */}
          {isAdmin && <NavLink to="/peers">Peers</NavLink>}
        </nav>
        <form
          className="header-search"
          role="search"
          onSubmit={(e) => {
            e.preventDefault()
            if (q.trim()) navigate(`/search?q=${encodeURIComponent(q.trim())}`)
          }}
        >
          <label className="sr-only" htmlFor="global-search">Search the registry</label>
          <input
            id="global-search"
            type="search"
            placeholder="Search…"
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
        </form>
        {who ? (
          <div className="inline">
            <span className="chip" title={`${who.credential} · ${who.subject}`}>
              {who.display_name || who.subject}
              {who.is_admin ? ' · admin' : who.is_curator ? ' · curator' : ''}
            </span>
            <button type="button" className="link" onClick={signOut}>Sign out</button>
          </div>
        ) : (
          <SignIn />
        )}
      </div>
    </header>
  )
}

function SignIn() {
  const { registry, oidcAvailable, signInWithOidc, signInWithToken } = useSession()
  const [open, setOpen] = useState(false)
  const [token, setToken] = useState('')
  const [error, setError] = useState<string>()

  // No identity provider and no token path at all: nothing to sign in with, so show nothing.
  if (!registry) return null
  if (!oidcAvailable && !registry.auth?.api_tokens) return null

  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>Sign in</button>
      {open && (
        <Modal title="Sign in" onClose={() => setOpen(false)}>
          {oidcAvailable ? (
            <>
              <p>
                {/* host, not hostname: a dev Keycloak is 127.0.0.1:8090, and "127.0.0.1"
                    alone names nothing. */}
                Sign in with {new URL(registry.auth.oidc.issuer!).host}. Your roles there
                (<code>curator</code>, <code>admin</code>) decide what you can change here.
              </p>
              <div className="actions">
                <button
                  type="button"
                  className="primary"
                  onClick={() => signInWithOidc().catch((e) => setError(String(e)))}
                >
                  Continue with single sign-on
                </button>
              </div>
              <hr style={{ border: 0, borderTop: '1px solid var(--border)', margin: '18px 0' }} />
            </>
          ) : (
            <p className="hint" style={{ marginTop: 0 }}>
              This registry has no identity provider configured, so sign-in uses a registry API
              token. Deployments should authenticate with OIDC instead — see the workload
              identity documentation.
            </p>
          )}
          <form
            onSubmit={async (e) => {
              e.preventDefault()
              setError(undefined)
              try {
                await signInWithToken(token.trim())
                setOpen(false)
              } catch (err) {
                setError(err instanceof Error ? err.message : String(err))
              }
            }}
          >
            <div className="field">
              <label htmlFor="token">API token</label>
              <input
                id="token"
                type="password"
                autoComplete="off"
                value={token}
                onChange={(e) => setToken(e.target.value)}
                placeholder="tar_…"
              />
              <p className="hint">Stored for this browser tab only, never in a cookie.</p>
            </div>
            {error && <p className="field-error" role="alert">{error}</p>}
            <div className="actions">
              <button type="submit" className="primary" disabled={!token.trim()}>Sign in</button>
              <button type="button" onClick={() => setOpen(false)}>Cancel</button>
            </div>
          </form>
        </Modal>
      )}
    </>
  )
}
