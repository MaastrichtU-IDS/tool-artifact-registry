import { useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { CopyBlock, CopyField, ErrorState, Modal, ProblemJsonError, Skeleton } from '../components/common'
import { RelativeTime } from '../components/chips'
import { useSession } from '../lib/session'
import type { Software, WellKnown } from '../lib/types'

/**
 * How a deployment of one piece of software gets a record here.
 *
 * Two decisions, kept as two controls rather than four items in a list: *who fills the record
 * in* — a curator, or the application itself — and *which credential does it*. A reader
 * arrives having already made one of them, usually the credential their site runs on, and only
 * the combination they chose is spelled out. Every combination is named on screen, so nothing
 * is behind a click whose contents cannot be guessed.
 */

const SCOPES = ['register:instance', 'advertise:produce', 'advertise:consume', 'read:private']

type Mode = 'manual' | 'self'
type Credential = 'key' | 'oidc'

const MODES: { id: Mode; name: string; what: string }[] = [
  {
    id: 'manual',
    name: 'Manual registration',
    what: 'A curator fills the record in and maintains it. Right when deployments are few and long-lived, and someone knows the estate.',
  },
  {
    id: 'self',
    name: 'Self-registration',
    what: 'The deployment announces itself and keeps its own record current. Right when deployments come and go without anyone here hearing about it.',
  },
]

const CREDENTIALS: { id: Credential; name: string; what: string }[] = [
  {
    id: 'key',
    name: 'API key',
    what: 'A key minted here and handed to whatever does the registering. Works with no identity provider at all.',
  },
  {
    id: 'oidc',
    name: 'Identity provider',
    what: 'A service account in Keycloak or another trusted issuer, presenting a short-lived token it fetches itself. Nothing long-lived to leak.',
  },
]

export default function SoftwareDeploy() {
  const { id = '' } = useParams()
  const { isCurator, loading: sessionLoading, registry } = useSession()
  const sw = useAsync(() => api.getSoftware(id), [id])
  const [mode, setMode] = useState<Mode>('manual')
  // Left unset until the reader chooses, so the default can follow how this registry is
  // actually configured rather than what was true when the component first rendered.
  const [chosen, setCredential] = useState<Credential>()

  if (sessionLoading || sw.loading) return <Skeleton rows={6} />
  if (sw.error) return <ErrorState error={sw.error} onRetry={sw.reload} />
  const software = sw.data!

  if (!isCurator) {
    return (
      <div className="banner warn">
        <h3>Curator role required</h3>
        <p>
          Both ways of registering a deployment add records to this registry, and issuing a key
          for one is a standing permission to keep adding them, so only a curator or an
          administrator may set either up.
        </p>
        <Link to={`/software/${id}`}>Back to {software.name}</Link>
      </div>
    )
  }

  // The page is served from the registry, so its own origin is the honest default even before
  // discovery has answered.
  const base = registry?.base_iri ?? window.location.origin
  const credential: Credential = chosen ?? (registry?.auth?.oidc?.enabled ? 'oidc' : 'key')

  return (
    <>
      <div className="page-header">
        <h1>Create deployment — {software.name}</h1>
        <p className="tagline">
          One record per place {software.name} runs. Choose who writes that record and what
          credential they use, and this page shows exactly that combination — nothing else.
        </p>
      </div>

      <section className="card">
        <h2>Which way</h2>
        <div className="axes">
          <Axis name="mode" legend="Who fills it in" options={MODES} value={mode} onChange={setMode} />
          <Axis
            name="credential"
            legend="Which credential"
            options={CREDENTIALS}
            value={credential}
            onChange={setCredential}
          />
        </div>
      </section>

      {mode === 'manual' && credential === 'key' && (
        <ManualWithKey software={software} base={base} />
      )}
      {mode === 'manual' && credential === 'oidc' && (
        <ManualWithOidc software={software} base={base} registry={registry} />
      )}
      {mode === 'self' && credential === 'key' && (
        <SelfWithKey software={software} base={base} />
      )}
      {mode === 'self' && credential === 'oidc' && (
        <SelfWithOidc
          software={software}
          base={base}
          registry={registry}
          onUseKeyInstead={() => setCredential('key')}
        />
      )}

      {mode === 'self' && <AnnouncementFields software={software} />}
    </>
  )
}

/** One axis of the choice: every option's name and its "right when" are on screen at once, so
 *  the reader can see what they are not choosing. */
function Axis<T extends string>({
  name, legend, options, value, onChange,
}: {
  name: string
  legend: string
  options: { id: T; name: string; what: string }[]
  value: T
  onChange: (v: T) => void
}) {
  return (
    <fieldset className="axis">
      <legend>{legend}</legend>
      {options.map((o) => (
        <label key={o.id} className={o.id === value ? 'option selected' : 'option'}>
          <input
            type="radio"
            name={name}
            value={o.id}
            checked={o.id === value}
            onChange={() => onChange(o.id)}
          />
          <span>
            <strong>{o.name}</strong>
            <span className="hint">{o.what}</span>
          </span>
        </label>
      ))}
    </fieldset>
  )
}

function DeploymentFormLink({ software }: { software: Software }) {
  return (
    <p style={{ marginTop: 14, marginBottom: 0 }}>
      <Link className="chip accent" to={`/instances/new?software=${software.id}`}>
        Open the deployment form →
      </Link>
    </p>
  )
}

function ManualWithOidc({
  software, base, registry,
}: { software: Software; base: string; registry?: WellKnown }) {
  const oidc = registry?.auth?.oidc
  return (
    <section className="card">
      <h2>Manual registration, signed in</h2>
      {oidc?.enabled && oidc.issuer ? (
        <p style={{ marginTop: 0 }}>
          You are the credential. Sign in with <code>{oidc.issuer}</code>, and the{' '}
          <code>curator</code> or <code>admin</code> role you hold there is what lets you write
          the record. Nothing is stored in this browser beyond the tab you are in.
        </p>
      ) : (
        <p style={{ marginTop: 0 }}>
          You are the credential. This registry has no identity provider, so you are signed in
          with a registry API token, and its authority is what writes the record. Nothing is
          stored in this browser beyond the tab you are in.
        </p>
      )}
      <p>
        Fill in what you know. Everything except the label may be left empty and added later —
        a deployment with no endpoint, such as a laptop or a batch job, is a normal record
        rather than an incomplete one.
      </p>
      <DeploymentFormLink software={software} />
      <p className="hint" style={{ marginTop: 14 }}>
        The record is then yours to maintain: nothing updates it when {software.name} is
        upgraded or moved. If that is going to be a problem, self-registration is the mode that
        keeps itself current.
      </p>
      <details className="disclosure">
        <summary>Doing it from a script instead</summary>
        <p style={{ marginTop: 10 }}>
          Whatever you signed in with works as a bearer token here, carrying the same authority
          and no more. This is the same record the form writes.
        </p>
        <CopyBlock label="the create-deployment request" code={createCurl(base, software, '$TOKEN')} />
      </details>
    </section>
  )
}

function ManualWithKey({ software, base }: { software: Software; base: string }) {
  return (
    <section className="card">
      <h2>Manual registration, with an API key</h2>
      <p style={{ marginTop: 0 }}>
        The form below writes the record with whatever you are signed in as, which is the
        shortest path. A registry API token carrying <code>register:instance</code> does the
        same thing from a script — useful when the estate is described in configuration rather
        than filled in by hand.
      </p>
      <CopyBlock label="the create-deployment request" code={createCurl(base, software, '$TAR_TOKEN')} />
      <p className="hint">
        A key that may create records this way is not tied to one piece of software, so keep it
        with the person or the pipeline doing the curating — not inside the deployment. A
        credential that lives in the deployment should be a self-registration one, which can
        only ever describe itself.
      </p>
      <DeploymentFormLink software={software} />
    </section>
  )
}

function SelfWithOidc({
  software, base, registry, onUseKeyInstead,
}: {
  software: Software
  base: string
  registry?: WellKnown
  onUseKeyInstead: () => void
}) {
  const oidc = registry?.auth?.oidc
  const trusted = [oidc?.issuer, ...(oidc?.workload_issuers ?? [])].filter(
    (v, i, all): v is string => Boolean(v) && all.indexOf(v) === i,
  )
  const audience = oidc?.audience ?? base
  const clients = software.registration_clients ?? []
  const usable = Boolean(oidc?.enabled) && trusted.length > 0

  const intro = (
    <section className="card">
      <h2>Self-registration, through an identity provider</h2>
      <p style={{ marginTop: 0 }}>
        There is no key to issue, hand over or leak. The deployment authenticates as a client in
        your provider, fetches a token that is good for minutes, and announces itself with that.
        Rotation stops being this registry's problem.
      </p>
      {usable ? (
        <p style={{ marginBottom: 0 }}>
          The client decides which software the deployment belongs to. A body naming a different
          one is refused with a <code>403</code>, not quietly reinterpreted.
        </p>
      ) : (
        <div className="banner warn" style={{ marginBottom: 0 }}>
          <h3>This registry trusts no issuer yet</h3>
          <p>
            A token from an issuer it does not know authenticates nothing here, so this
            combination cannot work until an administrator sets one. Until then, a key does the
            same job.{' '}
            <button type="button" className="link" onClick={onUseKeyInstead}>
              Show me that instead
            </button>
          </p>
        </div>
      )}
    </section>
  )

  if (!oidc?.enabled || trusted.length === 0) return intro

  return (
    <>
      {intro}

      <section className="card">
        <h2>1. Let the client register deployments of {software.name}</h2>
        {clients.length > 0 ? (
          <>
            <p style={{ marginTop: 0 }}>
              These client ids may already do it. The registry reads the client id from the
              token's <code>{oidc.client_claim}</code> claim, so what is listed here has to be
              that value exactly.
            </p>
            <ul className="chips">
              {clients.map((c) => (
                <li key={c}>
                  <span className="chip">{c}</span>
                </li>
              ))}
            </ul>
            <p className="hint">
              <Link to={`/software/${software.id}/edit`}>Edit the list →</Link>
            </p>
          </>
        ) : (
          <p style={{ margin: 0 }}>
            No client may register a deployment of {software.name} yet. Add its client id under{' '}
            <strong>Deployment registration</strong> on the{' '}
            <Link to={`/software/${software.id}/edit`}>software's edit form</Link>. The registry
            reads that id from the token's <code>{oidc.client_claim}</code> claim, so it has to
            match that value exactly.
          </p>
        )}
      </section>

      <section className="card">
        <h2>2. What that client needs on the provider</h2>
        <ul style={{ margin: 0, paddingLeft: 20 }}>
          <li>
            A confidential client with a <strong>service account</strong> — client credentials,
            no user — at an issuer this registry trusts:{' '}
            {trusted.map((iss) => (
              <code key={iss} style={{ marginRight: 6 }}>{iss}</code>
            ))}
          </li>
          <li>
            Access tokens that carry <code>{audience}</code> in <code>aud</code>. This is the
            single most common way the setup fails: the token is issued happily and then
            rejected here as an invalid audience, long after the thing that was misconfigured.
          </li>
        </ul>
        <p className="hint">
          The Keycloak realm shipped with this project does the audience part with a default
          client scope, so every client in it comes out with the right <code>aud</code> —
          including one that registered itself.
        </p>
      </section>

      <section className="card">
        <h2>3. What the deployment runs</h2>
        <CopyBlock
          label="the service account announcement"
          code={`ISSUER=${trusted[0]}
TOKEN_URL=$(curl -s $ISSUER/.well-known/openid-configuration | jq -r .token_endpoint)

TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" \\
  -d grant_type=client_credentials "$TOKEN_URL" | jq -r .access_token)

${announceCurl(base, software, '$TOKEN')}`}
        />
        <p className="hint">
          The first announcement creates the record and answers <code>201</code>; every one
          after it updates the same record and answers <code>200</code>. Run it at start-up and
          again whenever anything it says changes.
        </p>
        <p className="hint">
          <code>{base}/api/v1/whoami</code> with that token answers with the client id the
          registry actually sees, which is the quickest way to find out that{' '}
          <code>{oidc.client_claim}</code> does not hold what you listed above.
        </p>
      </section>
    </>
  )
}

function SelfWithKey({ software, base }: { software: Software; base: string }) {
  const tokens = useAsync(() => api.listSoftwareTokens(software.id), [software.id])
  const [scopes, setScopes] = useState<string[]>([
    'register:instance', 'advertise:produce', 'advertise:consume',
  ])
  const [label, setLabel] = useState('')
  const [expiresIn, setExpiresIn] = useState('')
  const [minted, setMinted] = useState<string>()
  const [error, setError] = useState<Error>()
  const [confirmRevoke, setConfirmRevoke] = useState<{ id: string; prefix: string }>()

  const mint = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(undefined)
    try {
      const res = await api.mintSoftwareToken(software.id, {
        scopes,
        label: label || undefined,
        expires_in: expiresIn || undefined,
      })
      setMinted(res.token)
      setLabel('')
      tokens.reload()
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    }
  }

  const live = tokens.data?.items.filter((t) => !t.revoked_at) ?? []

  return (
    <>
      <section className="card">
        <h2>Self-registration, with an API key</h2>
        <p style={{ marginTop: 0 }}>
          One key belongs to {software.name} itself, not to any one deployment of it. Give it to
          whatever ships the software — a Helm chart, a compose file, a CI pipeline — and every
          deployment it starts announces itself with the same key.
        </p>
        <p style={{ marginBottom: 0 }}>
          The key decides which software the deployment belongs to. A body naming a different
          one is refused with a <code>403</code>, not quietly reinterpreted, so a key for{' '}
          {software.name} cannot be turned into records for anything else.
        </p>
      </section>

      <section className="card">
        <h2>1. Issue a key</h2>
        {error && <ProblemJsonError error={error} />}
        <form onSubmit={mint}>
          <div className="field">
            <label htmlFor="key-label">Label</label>
            <input
              id="key-label"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="What holds this key — a Helm chart, a CI pipeline"
              style={{ maxWidth: 420 }}
            />
          </div>
          <div className="field">
            <span className="label">Scopes</span>
            {SCOPES.map((s) => (
              <label key={s} className="check">
                <input
                  type="checkbox"
                  checked={scopes.includes(s)}
                  onChange={(e) =>
                    setScopes(e.target.checked ? [...scopes, s] : scopes.filter((v) => v !== s))
                  }
                />
                <code>{s}</code>
              </label>
            ))}
            <p className="hint">
              <code>register:instance</code> is what makes this key a self-registration key. The
              advertise scopes come with it because a deployment that registers itself and then
              cannot say what it produced would need a second key immediately.
            </p>
          </div>
          <div className="field">
            <label htmlFor="key-expires">Expiry</label>
            <select
              id="key-expires"
              value={expiresIn}
              onChange={(e) => setExpiresIn(e.target.value)}
              style={{ width: 220 }}
            >
              <option value="">Never expires</option>
              <option value="30d">30 days</option>
              <option value="90d">90 days</option>
              <option value="365d">1 year</option>
            </select>
          </div>
          <div className="actions">
            <button type="submit" className="primary" disabled={scopes.length === 0}>
              Mint key
            </button>
          </div>
        </form>
      </section>

      <section className="card">
        <h2>2. What the deployment runs</h2>
        <CopyBlock label="the announcement" code={announceCurl(base, software, '$TAR_KEY')} />
        <p className="hint">
          The first call creates the record and answers <code>201</code>; every call after it
          updates the same record and answers <code>200</code>. Run it at start-up and again
          whenever anything it says changes.
        </p>
      </section>

      <section className="card flush">
        <h2>Keys issued for {software.name}</h2>
        {tokens.loading && (
          <div style={{ padding: 16 }}>
            <Skeleton rows={3} />
          </div>
        )}
        {tokens.data && tokens.data.items.length === 0 && (
          <p className="muted" style={{ padding: '0 16px 16px' }}>
            None yet, so no deployment of {software.name} can register itself with a key today.
          </p>
        )}
        {tokens.data && tokens.data.items.length > 0 && (
          <>
            <div className="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Prefix</th>
                    <th scope="col">Label</th>
                    <th scope="col">Scopes</th>
                    <th scope="col">Created</th>
                    <th scope="col">Last used</th>
                    <th scope="col">Expires</th>
                    <th scope="col">
                      <span className="sr-only">Actions</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {tokens.data.items.map((t) => (
                    <tr key={t.id} style={t.revoked_at ? { opacity: 0.55 } : undefined}>
                      <td className="mono">tar_{t.prefix}…</td>
                      <td>{t.label ?? <span className="muted">—</span>}</td>
                      <td>
                        <ul className="chips">
                          {t.scopes.map((s) => (
                            <li key={s}>
                              <span className="chip">{s}</span>
                            </li>
                          ))}
                        </ul>
                      </td>
                      <td><RelativeTime iso={t.created_at} /></td>
                      <td>
                        {t.last_used_at ? <RelativeTime iso={t.last_used_at} /> : <span className="muted">never</span>}
                      </td>
                      <td>
                        {t.expires_at ? <RelativeTime iso={t.expires_at} /> : <span className="muted">never</span>}
                      </td>
                      <td>
                        {t.revoked_at ? (
                          <span className="chip">revoked</span>
                        ) : (
                          <button
                            type="button"
                            className="danger"
                            onClick={() => setConfirmRevoke({ id: t.id, prefix: t.prefix })}
                          >
                            Revoke
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {live.length > 1 && (
              <p className="hint" style={{ padding: '10px 16px 14px', marginTop: 0 }}>
                {live.length} keys are live. Each is enough on its own to add deployment records
                for {software.name}, so revoke the ones nothing is using.
              </p>
            )}
          </>
        )}
      </section>

      {minted && (
        <Modal title="Copy this key now" onClose={() => setMinted(undefined)} dismissible={false}>
          <div className="banner warn">
            <p>This value will not be shown again. If you lose it, revoke it and mint another.</p>
          </div>
          <CopyField value={minted} label="new self-registration key" />
          <div className="actions">
            <button type="button" className="primary" onClick={() => setMinted(undefined)}>
              I have copied it
            </button>
          </div>
        </Modal>
      )}

      {confirmRevoke && (
        <Modal title="Revoke this key?" onClose={() => setConfirmRevoke(undefined)}>
          <p>
            Deployments still using <code>tar_{confirmRevoke.prefix}…</code> stop being able to
            update their records. The records themselves stay.
          </p>
          <div className="actions">
            <button type="button" onClick={() => setConfirmRevoke(undefined)}>
              Cancel
            </button>
            <button
              type="button"
              className="danger"
              onClick={async () => {
                await api.revokeSoftwareToken(software.id, confirmRevoke.id)
                setConfirmRevoke(undefined)
                tokens.reload()
              }}
            >
              Revoke
            </button>
          </div>
        </Modal>
      )}
    </>
  )
}

/** The request reference. Identical whichever credential presents it, so it is stated once. */
function AnnouncementFields({ software }: { software: Software }) {
  return (
    <section className="card">
      <h2>What the announcement contains</h2>
      <p style={{ marginTop: 0 }}>
        <code>PUT /api/v1/instances/self</code>, a JSON body, every field optional except where
        said. A field you leave out keeps whatever is stored: announcing a new endpoint does not
        erase the jurisdiction a curator set.
      </p>
      <div className="table-scroll">
        <table>
          <thead>
            <tr>
              <th scope="col">Field</th>
              <th scope="col">When</th>
              <th scope="col">What it is</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td className="mono nowrap">instance_key</td>
              <td className="nowrap">First announcement</td>
              <td>
                The stable name this deployment calls itself — a hostname, a cluster, a
                namespace. It is what makes the second announcement update the first one's
                record instead of making another. Defaults to the credential's own subject,
                which is right only when one credential means one deployment; the moment two
                deployments share a credential, they need distinct keys.
              </td>
            </tr>
            <tr>
              <td className="mono nowrap">label</td>
              <td className="nowrap">First announcement</td>
              <td>
                What the deployment is called here. Omit it afterwards and the stored label
                stands, so a curator's rename survives the next announcement.
              </td>
            </tr>
            <tr>
              <td className="mono nowrap">software</td>
              <td className="nowrap">Never, here</td>
              <td>
                Which software this is a deployment of. Both credentials on this page are
                already bound to {software.name}, so it is decided for you and this field is
                ignored — except that naming a <em>different</em> software is a{' '}
                <code>403</code>, not a hint. Only a credential bound to nothing has to send it.
              </td>
            </tr>
            <tr>
              <td className="mono nowrap">version</td>
              <td className="nowrap">Optional</td>
              <td>
                The version string the deployment is running. The registry matches it against
                this software's registered releases and links one if it finds a match, which is
                what makes an out-of-date deployment visible.
              </td>
            </tr>
            <tr>
              <td className="mono nowrap">release</td>
              <td className="nowrap">Optional</td>
              <td>The id of a release already registered here, when the deployment knows it.</td>
            </tr>
            <tr>
              <td className="mono nowrap">endpoint_url</td>
              <td className="nowrap">Optional</td>
              <td>
                Where this deployment answers. Leave it out for something with no endpoint — a
                laptop, a batch job — which is a complete record, not a half-filled one.
              </td>
            </tr>
            <tr>
              <td className="mono nowrap">endpoint_description</td>
              <td className="nowrap">Optional</td>
              <td>URL of a machine-readable description of that endpoint, such as an OpenAPI document.</td>
            </tr>
            <tr>
              <td className="mono nowrap">health_endpoint</td>
              <td className="nowrap">Optional</td>
              <td>
                Where the registry probes for liveness, and it is held to a <code>2xx</code>.
                Leave it out and the endpoint URL itself is probed, where anything that answers
                counts as up — many healthy services return <code>401</code> or <code>404</code>{' '}
                at their root. The registry writes the health; a deployment cannot assert its
                own.
              </td>
            </tr>
            <tr>
              <td className="mono nowrap">availability</td>
              <td className="nowrap">Optional</td>
              <td>
                One of <code>public</code>, <code>restricted</code>, <code>embargoed</code>,{' '}
                <code>metadata-only</code>.
              </td>
            </tr>
            <tr>
              <td className="mono nowrap">jurisdiction</td>
              <td className="nowrap">Optional</td>
              <td>Where this deployment runs, for the questions that turn on that — <code>NL</code>.</td>
            </tr>
            <tr>
              <td className="mono nowrap">description</td>
              <td className="nowrap">Optional</td>
              <td>Prose about this deployment specifically, not about the software.</td>
            </tr>
            <tr>
              <td className="mono nowrap">capability</td>
              <td className="nowrap">Optional</td>
              <td>
                <code>{'{"consumes": [...], "produces": [...]}'}</code>, artifact type IRIs. A
                deployment configured for less than the software can do may narrow the
                declaration here. It may never widen it.
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <p className="hint">
        There is no field for the client id, the scopes or the health. A deployment describes
        itself and cannot widen its own authority by announcing — those are set by whoever
        issued the credential.
      </p>
    </section>
  )
}

function endpointHost(software: Software): string {
  const slug = software.name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '')
  return `${slug || 'service'}.example.org`
}

function announceCurl(base: string, software: Software, token: string): string {
  const host = endpointHost(software)
  return `curl -X PUT ${base}/api/v1/instances/self \\
  -H "authorization: Bearer ${token}" \\
  -H "content-type: application/json" \\
  -d '{
    "instance_key": "prod-cluster",
    "label": "${software.name} on prod",
    "endpoint_url": "https://${host}",
    "health_endpoint": "https://${host}/healthz",
    "version": "${software.latest_release?.version ?? '1.0.0'}",
    "availability": "restricted",
    "jurisdiction": "NL"
  }'`
}

function createCurl(base: string, software: Software, token: string): string {
  const host = endpointHost(software)
  return `curl -X POST ${base}/api/v1/instances \\
  -H "authorization: Bearer ${token}" \\
  -H "content-type: application/json" \\
  -d '{
    "label": "${software.name} on prod",
    "software": "${software.id}",
    "endpoint_url": "https://${host}",
    "health_endpoint": "https://${host}/healthz",
    "availability": "restricted",
    "jurisdiction": "NL"
  }'`
}
