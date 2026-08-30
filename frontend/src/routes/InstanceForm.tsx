import { useEffect, useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { ApiError, api } from '../lib/api'
import { useAsync } from '../lib/useAsync'
import { useSession } from '../lib/session'
import { ProblemJsonError, Skeleton } from '../components/common'
import { TermPicker } from '../components/TermPicker'

const SCOPES = ['advertise:produce', 'advertise:consume', 'register:software', 'register:instance', 'read:private']

export default function InstanceForm() {
  const { id } = useParams()
  const [params] = useSearchParams()
  const editing = Boolean(id)
  const navigate = useNavigate()
  const { isCurator, loading: sessionLoading, registry } = useSession()
  const software = useAsync(() => api.listSoftware({ limit: '200' }), [])

  const [form, setForm] = useState({
    label: '', software: params.get('software') ?? '', release: '', endpoint_url: '',
    endpoint_description: '', operator_name: '', availability: '', jurisdiction: '',
    description: '', oidc_client_id: '', oidc_issuer: '',
  })
  const [scopes, setScopes] = useState<string[]>(['advertise:produce', 'advertise:consume'])
  // An instance may narrow what its software declares (spec §7.3); left empty it inherits.
  const [consumes, setConsumes] = useState<string[]>([])
  const [produces, setProduces] = useState<string[]>([])
  const [loading, setLoading] = useState(editing)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<Error>()
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({})

  useEffect(() => {
    if (!id) return
    api.getInstance(id).then((i) => {
      setForm({
        label: i.label,
        software: i.software?.split('/').pop() ?? '',
        release: i.release?.split('/').pop() ?? '',
        endpoint_url: i.endpoint_url ?? '',
        endpoint_description: i.endpoint_description ?? '',
        operator_name: i.operator?.name ?? '',
        availability: i.availability ?? '',
        jurisdiction: i.jurisdiction ?? '',
        description: i.description ?? '',
        oidc_client_id: i.oidc_client_id ?? '',
        oidc_issuer: i.oidc_issuer ?? '',
      })
      if (i.allowed_scopes.length) setScopes(i.allowed_scopes)
      setConsumes((i.capability?.consumes ?? []).map((t) => t.iri))
      setProduces((i.capability?.produces ?? []).map((t) => t.iri))
      setLoading(false)
    }).catch((e) => { setError(e as Error); setLoading(false) })
  }, [id])

  const releases = useAsync(
    () => (form.software ? api.listReleases(form.software) : Promise.resolve({ items: [], total: 0 })),
    [form.software],
  )

  if (sessionLoading) return <Skeleton rows={4} />
  if (!isCurator) {
    return (
      <div className="banner warn">
        <h3>Curator role required</h3>
        <p>Sign in with the curator or admin role to register a deployment.</p>
      </div>
    )
  }
  if (loading) return <Skeleton rows={8} />

  const set = (k: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>) =>
    setForm({ ...form, [k]: e.target.value })

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setSaving(true)
    setError(undefined)
    setFieldErrors({})
    const body = {
      label: form.label,
      software: form.software || undefined,
      release: form.release || undefined,
      endpoint_url: form.endpoint_url || undefined,
      endpoint_description: form.endpoint_description || undefined,
      operator: form.operator_name ? { name: form.operator_name, kind: 'organization' } : undefined,
      availability: form.availability || undefined,
      jurisdiction: form.jurisdiction || undefined,
      description: form.description || undefined,
      oidc_client_id: form.oidc_client_id || undefined,
      oidc_issuer: form.oidc_issuer || undefined,
      allowed_scopes: scopes,
      capability: consumes.length || produces.length ? { consumes, produces } : undefined,
    }
    try {
      const saved = editing ? await api.updateInstance(id!, body) : await api.createInstance(body)
      navigate(`/instances/${saved.id}`)
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
      if (err instanceof ApiError) setFieldErrors(err.fieldErrors())
      setSaving(false)
    }
  }

  const field = (key: string) => (fieldErrors[key] ? 'field invalid' : 'field')
  const err = (key: string) => fieldErrors[key] && <p className="field-error" role="alert">{fieldErrors[key]}</p>

  return (
    <form onSubmit={submit} style={{ maxWidth: 720 }}>
      <div className="page-header">
        <h1>{editing ? 'Edit deployment' : 'Register deployment'}</h1>
        <p className="tagline">
          One record per place the software runs. Two sites running the same tool are two
          deployments of one software — that is the join key across registries.
        </p>
      </div>

      {error && <ProblemJsonError error={error} />}

      <fieldset>
        <legend>Identity</legend>
        <div className={field('label')}>
          <label htmlFor="label">Label *</label>
          <input id="label" required value={form.label} onChange={set('label')} placeholder="shacl.ids.unimaas.nl" />
          {err('label')}
        </div>
        <div className={field('software')}>
          <label htmlFor="software">Software *</label>
          <select id="software" value={form.software} onChange={set('software')} required>
            <option value="">—</option>
            {software.data?.items.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
          </select>
          {err('software')}
        </div>
        <div className="field">
          <label htmlFor="release">Release</label>
          <select id="release" value={form.release} onChange={set('release')}>
            <option value="">—</option>
            {releases.data?.items.map((r) => <option key={r.id} value={r.id}>{r.version}</option>)}
          </select>
        </div>
      </fieldset>

      <fieldset>
        <legend>Endpoint</legend>
        <div className={field('endpoint_url')}>
          <label htmlFor="endpoint">Endpoint URL</label>
          <input id="endpoint" value={form.endpoint_url} onChange={set('endpoint_url')} placeholder="https://shacl.ids.unimaas.nl" />
          <p className="hint">Leave empty for a laptop or a batch job. That is normal, not incomplete.</p>
          {err('endpoint_url')}
        </div>
        <div className="field">
          <label htmlFor="openapi">Endpoint description (OpenAPI)</label>
          <input id="openapi" value={form.endpoint_description} onChange={set('endpoint_description')} />
        </div>
      </fieldset>

      <fieldset>
        <legend>Operator and availability</legend>
        <div className="field">
          <label htmlFor="operator">Operator</label>
          <input id="operator" value={form.operator_name} onChange={set('operator_name')} />
        </div>
        <div className={field('availability')}>
          <label htmlFor="availability">Availability</label>
          <select id="availability" value={form.availability} onChange={set('availability')}>
            <option value="">—</option>
            <option value="public">public</option>
            <option value="restricted">restricted</option>
            <option value="embargoed">embargoed</option>
            <option value="metadata-only">metadata-only</option>
          </select>
          {err('availability')}
        </div>
        <div className="field">
          <label htmlFor="jurisdiction">Jurisdiction</label>
          <input id="jurisdiction" value={form.jurisdiction} onChange={set('jurisdiction')} placeholder="NL" />
        </div>
      </fieldset>

      <fieldset>
        <legend>Workload identity</legend>
        <p className="hint" style={{ marginTop: 0 }}>
          Bind this deployment to a client in your identity provider and it can advertise with a
          short-lived token it fetches itself — no secret is stored here, and rotation is the
          provider's job. Leave empty to use registry API tokens instead.
          {registry?.auth?.oidc?.workload_issuers?.length ? (
            <> Trusted issuers: {registry.auth.oidc.workload_issuers.join(', ')}.</>
          ) : (
            <> This registry has no trusted issuer configured yet, so a client id here will not
              authenticate anything until one is set.</>
          )}
        </p>
        <div className={field('oidc_client_id')}>
          <label htmlFor="client">OIDC client id / subject</label>
          <input id="client" value={form.oidc_client_id} onChange={set('oidc_client_id')} placeholder="shacl-manager-ids3" />
          <p className="hint">
            A Keycloak client id, a Kubernetes ServiceAccount subject
            (<code>system:serviceaccount:ns:name</code>), or a GitHub Actions subject
            (<code>repo:org/repo:ref:refs/heads/main</code>).
          </p>
          {err('oidc_client_id')}
        </div>
        <div className="field">
          <label htmlFor="issuer">Restrict to issuer</label>
          <input id="issuer" value={form.oidc_issuer} onChange={set('oidc_issuer')} placeholder="https://keycloak.example.org/realms/ids" />
          <p className="hint">A client id is only unique within an issuer. Set this when more than one issuer is trusted.</p>
        </div>
        <div className="field">
          <label>Scopes granted to that identity</label>
          {SCOPES.map((s) => (
            <label key={s} style={{ fontWeight: 400, display: 'flex', gap: 8, alignItems: 'center', marginTop: 4 }}>
              <input
                type="checkbox"
                style={{ width: 'auto' }}
                checked={scopes.includes(s)}
                onChange={(e) => setScopes(e.target.checked ? [...scopes, s] : scopes.filter((x) => x !== s))}
              />
              <code>{s}</code>
            </label>
          ))}
          <p className="hint">Applied when the presented token carries no scope this registry recognises.</p>
        </div>
      </fieldset>

      <fieldset>
        <legend>Capability narrowing</legend>
        <p className="hint" style={{ marginTop: 0 }}>
          Optional. A deployment may handle less than its software can — an instance configured
          for one format, say. Leave both empty and it inherits the software's declaration.
        </p>
        <TermPicker
          id="inst-consumes"
          label="Consumes here"
          branch="data"
          value={consumes}
          onChange={setConsumes}
          hint="Narrower than the software's declaration, not wider."
        />
        <TermPicker
          id="inst-produces"
          label="Produces here"
          branch="data"
          value={produces}
          onChange={setProduces}
        />
      </fieldset>

      <div className="actions">
        <button type="submit" className="primary" disabled={saving || !form.label.trim()}>
          {saving ? 'Saving…' : editing ? 'Save changes' : 'Register'}
        </button>
        <button type="button" onClick={() => navigate(-1)}>Cancel</button>
      </div>
    </form>
  )
}
