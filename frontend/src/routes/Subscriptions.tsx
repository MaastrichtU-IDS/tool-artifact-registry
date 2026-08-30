import { useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { CopyField, EmptyState, ErrorState, Modal, ProblemJsonError, Skeleton } from '../components/common'
import { ArtifactTypeChip, AvailabilityBadge, RelativeTime, shortId } from '../components/chips'
import { TermPicker } from '../components/TermPicker'
import type { Availability, Subscription, SubscriptionDelivery } from '../lib/types'

const AVAILABILITIES: Availability[] = ['public', 'restricted', 'embargoed', 'metadata-only']

/**
 * Manage a deployment's standing interest in artifacts.
 *
 * The shape follows Tokens (handoff §5.8): the same ownership rule, the same shown-once
 * secret, the same table-then-form layout. The difference is that a subscription has a health
 * of its own — a webhook can be failing right now — so delivery state is a first-class column
 * rather than a footnote, and it is never conveyed by colour alone.
 */
export default function Subscriptions() {
  const { id = '' } = useParams()
  const inst = useAsync(() => api.getInstance(id), [id])
  const subs = useAsync(() => api.listSubscriptions(id), [id])
  const [created, setCreated] = useState<{ secret?: string; pullUrl: string }>()
  const [error, setError] = useState<Error>()
  const [confirmDelete, setConfirmDelete] = useState<Subscription>()
  const [open, setOpen] = useState<string>()

  // form
  const [label, setLabel] = useState('')
  const [types, setTypes] = useState<string[]>([])
  const [availability, setAvailability] = useState<Availability[]>([])
  const [keywords, setKeywords] = useState('')
  const [q, setQ] = useState('')
  const [license, setLicense] = useState('')
  const [roles, setRoles] = useState<('produced' | 'consumed')[]>(['produced'])
  const [excludeOwn, setExcludeOwn] = useState(true)
  const [webhookUrl, setWebhookUrl] = useState('')

  if (inst.loading) return <Skeleton rows={6} />
  if (inst.error) return <ErrorState error={inst.error} onRetry={inst.reload} />
  const instance = inst.data!

  const filterIsEmpty =
    types.length === 0 && availability.length === 0 && !keywords.trim() && !q.trim() && !license.trim()

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(undefined)
    try {
      const res = await api.createSubscription(id, {
        label: label || undefined,
        webhook_url: webhookUrl.trim() || undefined,
        filter: {
          conforms_to: types,
          availability,
          keywords: keywords.split(',').map((k) => k.trim()).filter(Boolean),
          license: license.trim() ? [license.trim()] : [],
          q: q.trim() || undefined,
          roles,
          exclude_own: excludeOwn,
        },
      })
      setCreated({ secret: res.secret, pullUrl: res.subscription.pull_url })
      setLabel('')
      setTypes([])
      setAvailability([])
      setKeywords('')
      setQ('')
      setLicense('')
      setWebhookUrl('')
      subs.reload()
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    }
  }

  const act = async (sid: string, body: unknown) => {
    setError(undefined)
    try {
      await api.updateSubscription(sid, body)
      subs.reload()
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    }
  }

  return (
    <>
      <div className="page-header">
        <h1>Subscriptions — {instance.label}</h1>
        <p className="tagline">
          Tell this deployment when an artifact it cares about appears, instead of making it
          poll the whole registry. <Link to={`/instances/${id}`}>Back to the deployment →</Link>
        </p>
      </div>

      <section className="card">
        <h2>How a match reaches you</h2>
        <p style={{ marginTop: 0 }}>
          Every match is queued the moment it is advertised. There are two ways to collect it,
          and a subscription can use either.
        </p>
        <div className="two-col">
          <div>
            <h3 style={{ fontSize: 13, margin: '0 0 6px' }}>Pull — the default</h3>
            <p className="hint" style={{ marginTop: 0 }}>
              Nothing has to be reachable from the internet. A CLI, a laptop or a batch job asks
              for everything after a cursor whenever it happens to be running. Leave the webhook
              blank to use this.
            </p>
          </div>
          <div>
            <h3 style={{ fontSize: 13, margin: '0 0 6px' }}>Webhook</h3>
            <p className="hint" style={{ marginTop: 0 }}>
              For a deployment that can accept an inbound HTTPS connection. Each POST is signed
              with a secret shown once, so your receiver can tell it came from here. Failures
              back off, and a persistently dead endpoint is suspended rather than hammered — the
              pull path keeps working meanwhile.
            </p>
          </div>
        </div>
      </section>

      <section className="card">
        <h2>New subscription</h2>
        {error && <ProblemJsonError error={error} />}
        <form onSubmit={submit}>
          <div className="field">
            <label htmlFor="sub-label">Label</label>
            <input
              id="sub-label"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="SHACL reports from the MUMC deployment"
            />
          </div>

          <TermPicker
            id="sub-types"
            branch="data"
            label="Artifact type"
            hint="The main question a subscription answers: what kind of thing do you want to hear about? Several types are alternatives — any of them matches."
            placeholder="SHACL validation report, RDF graph…"
            value={types}
            onChange={setTypes}
          />

          <div className="field">
            <label>Availability</label>
            <p className="hint" style={{ marginTop: 0 }}>
              Most artifacts in this registry are described but not retrievable. Narrow to what
              you can act on, or leave blank to hear about everything.
            </p>
            {AVAILABILITIES.map((a) => (
              <label key={a} style={{ fontWeight: 400, display: 'flex', gap: 8, alignItems: 'center', marginTop: 4 }}>
                <input
                  type="checkbox"
                  style={{ width: 'auto' }}
                  checked={availability.includes(a)}
                  onChange={(e) =>
                    setAvailability(e.target.checked ? [...availability, a] : availability.filter((x) => x !== a))
                  }
                />
                <AvailabilityBadge availability={a} />
              </label>
            ))}
          </div>

          <div className="field">
            <label htmlFor="sub-keywords">Keywords</label>
            <input
              id="sub-keywords"
              value={keywords}
              onChange={(e) => setKeywords(e.target.value)}
              placeholder="fhir, cohort-b"
            />
            <p className="hint">Comma separated. Any one of them matches, case-insensitively.</p>
          </div>

          <div className="field">
            <label htmlFor="sub-q">Title or description contains</label>
            <input id="sub-q" value={q} onChange={(e) => setQ(e.target.value)} placeholder="patients.ttl" />
          </div>

          <div className="field">
            <label htmlFor="sub-license">Licence IRI</label>
            <input
              id="sub-license"
              value={license}
              onChange={(e) => setLicense(e.target.value)}
              placeholder="https://spdx.org/licenses/CC-BY-4.0"
            />
            <p className="hint">
              An artifact with no stated licence never matches this — absent is not the same as
              permissive.
            </p>
          </div>

          <div className="field">
            <label>Advertisement kind</label>
            {(['produced', 'consumed'] as const).map((r) => (
              <label key={r} style={{ fontWeight: 400, display: 'flex', gap: 8, alignItems: 'center', marginTop: 4 }}>
                <input
                  type="checkbox"
                  style={{ width: 'auto' }}
                  checked={roles.includes(r)}
                  onChange={(e) => setRoles(e.target.checked ? [...roles, r] : roles.filter((x) => x !== r))}
                />
                <code>{r}</code>
                <span className="muted" style={{ fontSize: 12 }}>
                  {r === 'produced' ? 'something was made' : 'something was used as input'}
                </span>
              </label>
            ))}
            <label style={{ fontWeight: 400, display: 'flex', gap: 8, alignItems: 'center', marginTop: 10 }}>
              <input
                type="checkbox"
                style={{ width: 'auto' }}
                checked={excludeOwn}
                onChange={(e) => setExcludeOwn(e.target.checked)}
              />
              Ignore this deployment&rsquo;s own artifacts
            </label>
          </div>

          <div className="field">
            <label htmlFor="sub-webhook">Webhook URL</label>
            <input
              id="sub-webhook"
              value={webhookUrl}
              onChange={(e) => setWebhookUrl(e.target.value)}
              placeholder="https://receiver.example.org/tar-hook"
            />
            <p className="hint">
              Optional. HTTPS only, no credentials in the URL, and public addresses only — the
              registry will not be used to reach into a private network. Leave blank to poll
              instead.
            </p>
          </div>

          {filterIsEmpty && (
            <div className="banner warn" role="status">
              <h3>This subscription matches everything</h3>
              <p>
                With no type, availability, keyword, text or licence set, every artifact
                advertised here will be queued for you. That is allowed, but it is rarely what
                someone means.
              </p>
            </div>
          )}

          <div className="actions">
            <button type="submit" className="primary" disabled={roles.length === 0}>
              Create subscription
            </button>
          </div>
        </form>
      </section>

      <section className="card flush">
        <h2>Existing subscriptions</h2>
        {subs.loading && (
          <div style={{ padding: 16 }}>
            <Skeleton rows={3} />
          </div>
        )}
        {subs.error && (
          <div style={{ padding: 16 }}>
            <ErrorState error={subs.error} onRetry={subs.reload} />
          </div>
        )}
        {subs.data && subs.data.items.length === 0 && (
          <div style={{ padding: '0 16px 16px' }}>
            <EmptyState
              title="No subscriptions yet"
              body="Nothing is watching for artifacts on this deployment's behalf. Create one above."
            />
          </div>
        )}
        {subs.data && subs.data.items.length > 0 && (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">Label</th>
                  <th scope="col">Matches</th>
                  <th scope="col">Delivery</th>
                  <th scope="col">State</th>
                  <th scope="col">Waiting</th>
                  <th scope="col">Last match</th>
                  <th scope="col">
                    <span className="sr-only">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {subs.data.items.map((s) => (
                  <tr key={s.id} style={s.enabled ? undefined : { opacity: 0.55 }}>
                    <td>
                      <button type="button" className="link" onClick={() => setOpen(open === s.id ? undefined : s.id)}>
                        {s.label ?? shortId(s.id)}
                      </button>
                    </td>
                    <td>
                      <FilterSummary sub={s} />
                    </td>
                    <td>
                      {s.delivery_mode === 'webhook' ? (
                        <span className="chip" title={s.webhook_url}>
                          webhook{s.webhook_signed ? ' · signed' : ''}
                        </span>
                      ) : (
                        <span className="chip plain" title="This subscriber polls; nothing is sent to it">
                          pull
                        </span>
                      )}
                    </td>
                    <td>
                      <DeliveryState sub={s} />
                    </td>
                    <td className="nowrap">
                      {s.unacked_count > 0 ? (
                        <>
                          {s.unacked_count} unread
                          {s.dead_count > 0 && <span className="muted"> · {s.dead_count} undeliverable</span>}
                        </>
                      ) : (
                        <span className="muted">—</span>
                      )}
                    </td>
                    <td>
                      {s.last_match_at ? <RelativeTime iso={s.last_match_at} /> : <span className="muted">never</span>}
                    </td>
                    <td className="nowrap">
                      {s.delivery_state === 'suspended' && (
                        <button type="button" onClick={() => act(s.id, { resume: true })}>
                          Resume
                        </button>
                      )}{' '}
                      <button type="button" onClick={() => act(s.id, { enabled: !s.enabled })}>
                        {s.enabled ? 'Pause' : 'Enable'}
                      </button>{' '}
                      <button type="button" className="danger" onClick={() => setConfirmDelete(s)}>
                        Delete
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {open && <SubscriptionDetailPanel sid={open} onClose={() => setOpen(undefined)} />}

      {created && (
        <Modal title="Subscription created" onClose={() => setCreated(undefined)} dismissible={!created.secret}>
          {created.secret ? (
            <>
              <div className="banner warn">
                <p>
                  This signing secret will not be shown again. Verify it on every POST:
                  <code> HMAC-SHA256(secret, "&lt;x-tar-timestamp&gt;." + body)</code> must equal
                  the hex in <code>x-tar-signature</code>.
                </p>
              </div>
              <CopyField value={created.secret} label="webhook signing secret" />
            </>
          ) : (
            <p>
              This is a pull subscription: nothing will be sent to you. Ask for everything after
              a cursor whenever you are running.
            </p>
          )}
          <p className="hint">Pull endpoint</p>
          <CopyField value={created.pullUrl} label="pull endpoint" />
          <div className="actions">
            <button type="button" className="primary" onClick={() => setCreated(undefined)}>
              {created.secret ? 'I have copied it' : 'Done'}
            </button>
          </div>
        </Modal>
      )}

      {confirmDelete && (
        <Modal title="Delete subscription" onClose={() => setConfirmDelete(undefined)}>
          <p>
            Delete <strong>{confirmDelete.label ?? shortId(confirmDelete.id)}</strong> for{' '}
            <strong>{instance.label}</strong>? Its queued matches, including{' '}
            {confirmDelete.unacked_count} not yet collected, are dropped with it.
          </p>
          <div className="actions">
            <button
              type="button"
              className="danger"
              onClick={async () => {
                await api.deleteSubscription(confirmDelete.id)
                setConfirmDelete(undefined)
                subs.reload()
              }}
            >
              Delete it
            </button>
            <button type="button" onClick={() => setConfirmDelete(undefined)}>
              Cancel
            </button>
          </div>
        </Modal>
      )}
    </>
  )
}

/** What this subscription is actually asking for, in words rather than JSON. */
function FilterSummary({ sub }: { sub: Subscription }) {
  const f = sub.filter
  const parts: React.ReactNode[] = []
  if (f.conforms_to?.length) {
    parts.push(
      <span key="t" className="chips" style={{ display: 'inline-flex' }}>
        {f.conforms_to.map((t) => (
          <ArtifactTypeChip key={t} type={{ iri: t, source: 'external' }} interactive={false} />
        ))}
      </span>,
    )
  }
  if (f.availability?.length) {
    parts.push(
      <span key="a">
        {f.availability.map((a) => (
          <AvailabilityBadge key={a} availability={a} />
        ))}
      </span>,
    )
  }
  if (f.keywords?.length) parts.push(<span key="k" className="chip plain">{f.keywords.join(', ')}</span>)
  if (f.q) parts.push(<span key="q" className="chip plain">“{f.q}”</span>)
  if (f.license?.length) parts.push(<span key="l" className="chip plain">licensed</span>)
  if (f.instance?.length || f.software?.length) parts.push(<span key="p" className="chip plain">from named sources</span>)
  if (parts.length === 0) {
    return <span className="muted">everything advertised here</span>
  }
  return (
    <span className="inline" style={{ gap: 6, flexWrap: 'wrap' }}>
      {parts}
    </span>
  )
}

/** Never colour alone: shape plus words (handoff §6.1, §8). */
function DeliveryState({ sub }: { sub: Subscription }) {
  if (!sub.enabled) {
    return (
      <span className="chip">
        <span className="dot hollow" aria-hidden="true" />
        paused
      </span>
    )
  }
  if (sub.delivery_state === 'suspended') {
    return (
      <span className="chip danger" title={sub.last_error}>
        <span className="dot square" aria-hidden="true" />
        suspended after {sub.consecutive_failures} failures
      </span>
    )
  }
  if (sub.delivery_mode === 'pull') {
    return (
      <span className="chip ok">
        <span className="dot" aria-hidden="true" />
        collecting
      </span>
    )
  }
  if (sub.consecutive_failures > 0) {
    return (
      <span className="chip warn" title={sub.last_error}>
        <span className="dot hollow" aria-hidden="true" />
        retrying · {sub.consecutive_failures} failed
      </span>
    )
  }
  return (
    <span className="chip ok">
      <span className="dot" aria-hidden="true" />
      delivering
    </span>
  )
}

/** Recent deliveries, so a failure is visible to the person who owns it rather than only in a
 *  server log. */
function SubscriptionDetailPanel({ sid, onClose }: { sid: string; onClose: () => void }) {
  const detail = useAsync(() => api.getSubscription(sid), [sid])
  return (
    <section className="card flush">
      <div className="spread" style={{ padding: '0 16px' }}>
        <h2>Recent deliveries</h2>
        <button type="button" className="link" onClick={onClose}>
          Close
        </button>
      </div>
      {detail.loading && (
        <div style={{ padding: 16 }}>
          <Skeleton rows={3} />
        </div>
      )}
      {detail.error && (
        <div style={{ padding: 16 }}>
          <ErrorState error={detail.error} onRetry={detail.reload} />
        </div>
      )}
      {detail.data && (
        <>
          {detail.data.subscription.last_error && (
            <div className="banner danger" role="alert" style={{ margin: '0 16px 12px' }}>
              <h3>The last delivery attempt failed</h3>
              <p>{detail.data.subscription.last_error}</p>
            </div>
          )}
          <div style={{ padding: '0 16px 12px' }}>
            <p className="hint" style={{ marginTop: 0 }}>
              Collect these from a tool with no inbound endpoint:
            </p>
            <CopyField
              value={`curl -H "Authorization: Bearer $TOKEN" '${detail.data.subscription.pull_url}?limit=50'`}
              label="pull command"
              mono
            />
          </div>
          {detail.data.recent_deliveries.length === 0 ? (
            <p className="muted" style={{ padding: '0 16px 16px' }}>
              Nothing has matched this subscription yet.
            </p>
          ) : (
            <div className="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Seq</th>
                    <th scope="col">Artifact</th>
                    <th scope="col">Role</th>
                    <th scope="col">Matched</th>
                    <th scope="col">Status</th>
                    <th scope="col">Detail</th>
                  </tr>
                </thead>
                <tbody>
                  {detail.data.recent_deliveries.map((d) => (
                    <tr key={d.id}>
                      <td className="mono">{d.seq}</td>
                      <td>
                        <ArtifactLink d={d} />
                      </td>
                      <td>{d.role}</td>
                      <td>
                        <RelativeTime iso={d.matched_at} />
                      </td>
                      <td>
                        <DeliveryStatus d={d} />
                      </td>
                      <td className="muted" style={{ fontSize: 12 }}>
                        {d.last_error ?? (d.next_attempt_at ? <>retry <RelativeTime iso={d.next_attempt_at} /></> : '—')}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </section>
  )
}

function ArtifactLink({ d }: { d: SubscriptionDelivery }) {
  const artifact = d.notification?.artifact
  const title = artifact?.title ?? d.artifact_iri.split('/').pop() ?? d.artifact_iri
  if (artifact?.id) return <Link to={`/artifacts/${artifact.id}`}>{title}</Link>
  return <span className="mono">{title}</span>
}

function DeliveryStatus({ d }: { d: SubscriptionDelivery }) {
  const map: Record<string, { cls: string; dot: string; text: string; title: string }> = {
    delivered: { cls: 'chip ok', dot: 'dot', text: 'delivered', title: `HTTP ${d.last_status ?? ''}` },
    pending: { cls: 'chip', dot: 'dot hollow', text: 'queued', title: 'Waiting to be sent or collected' },
    failed: {
      cls: 'chip warn',
      dot: 'dot hollow',
      text: `retrying · attempt ${d.attempts}`,
      title: 'Backing off before the next attempt',
    },
    dead: {
      cls: 'chip danger',
      dot: 'dot square',
      text: 'undeliverable',
      title: 'Attempts exhausted. Still readable through the pull endpoint.',
    },
  }
  const s = map[d.status] ?? map.pending
  return (
    <span className={s.cls} title={s.title}>
      <span className={s.dot} aria-hidden="true" />
      {s.text}
    </span>
  )
}
