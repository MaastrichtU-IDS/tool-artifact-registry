import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'
import { ProblemJsonError, Skeleton } from '../components/common'

const KINDS = ['service', 'library', 'cli', 'workflow']

/** Sectioned to mirror the model: identity → links → licence and party → topics → capability
 *  (handoff §5.7). Validation failures come back as a SHACL report and are mapped to fields. */
export default function SoftwareForm() {
  const { id } = useParams()
  const editing = Boolean(id)
  const navigate = useNavigate()
  const { isCurator, loading: sessionLoading } = useSession()

  const [form, setForm] = useState({
    name: '', tagline: '', description: '', homepage: '', code_repository: '', documentation: '',
    license: '', kind: '', maturity: '', keywords: '', edam_topics: '', publisher_name: '',
    publisher_id: '', consumes: '', produces: '',
    image: '', screenshots: '', readme: '', readme_base_url: '',
  })
  const [loading, setLoading] = useState(editing)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<Error>()
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({})

  useEffect(() => {
    if (!id) return
    api.getSoftware(id).then((s) => {
      setForm({
        name: s.name, tagline: s.tagline ?? '', description: s.description ?? '',
        homepage: s.homepage ?? '', code_repository: s.code_repository ?? '',
        documentation: s.documentation ?? '', license: s.license ?? '', kind: s.kind ?? '',
        image: s.image ?? '',
        screenshots: s.screenshots.join('\n'),
        readme: s.readme ?? '',
        readme_base_url: s.readme_base_url ?? '',
        maturity: s.maturity ?? '', keywords: s.keywords.join(', '),
        edam_topics: s.edam_topics.map((t) => t.iri).join('\n'),
        publisher_name: s.publisher?.name ?? '', publisher_id: s.publisher?.identifier ?? '',
        consumes: (s.capability?.consumes ?? []).map((t) => t.iri).join('\n'),
        produces: (s.capability?.produces ?? []).map((t) => t.iri).join('\n'),
      })
      setLoading(false)
    }).catch((e) => { setError(e as Error); setLoading(false) })
  }, [id])

  if (sessionLoading) return <Skeleton rows={4} />
  if (!isCurator) {
    return (
      <div className="banner warn">
        <h3>Curator role required</h3>
        <p>Sign in with an account that has the curator or admin role to register software.</p>
      </div>
    )
  }
  if (loading) return <Skeleton rows={8} />

  const set = (k: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>) =>
    setForm({ ...form, [k]: e.target.value })

  const lines = (s: string) => s.split('\n').map((x) => x.trim()).filter(Boolean)
  const commas = (s: string) => s.split(',').map((x) => x.trim()).filter(Boolean)

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setSaving(true)
    setError(undefined)
    setFieldErrors({})
    const body = {
      name: form.name,
      tagline: form.tagline || undefined,
      description: form.description || undefined,
      homepage: form.homepage || undefined,
      code_repository: form.code_repository || undefined,
      documentation: form.documentation || undefined,
      image: form.image || undefined,
      screenshots: lines(form.screenshots),
      readme: form.readme || undefined,
      readme_base_url: form.readme_base_url || undefined,
      license: form.license || undefined,
      kind: form.kind || undefined,
      maturity: form.maturity || undefined,
      keywords: commas(form.keywords),
      edam_topics: lines(form.edam_topics),
      publisher: form.publisher_name || form.publisher_id
        ? { name: form.publisher_name || undefined, identifier: form.publisher_id || undefined, kind: 'organization' }
        : undefined,
      capability: (lines(form.consumes).length || lines(form.produces).length)
        ? { consumes: lines(form.consumes), produces: lines(form.produces) }
        : undefined,
    }
    try {
      const saved = editing ? await api.updateSoftware(id!, body) : await api.createSoftware(body)
      navigate(`/software/${saved.id}`)
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
        <h1>{editing ? 'Edit software' : 'Register software'}</h1>
        <p className="tagline">
          Everything but the name is optional, but a tool with no declared capability cannot be
          matched to anything.
        </p>
      </div>

      {error && <ProblemJsonError error={error} />}

      <fieldset>
        <legend>Identity</legend>
        <div className={field('name')}>
          <label htmlFor="name">Name *</label>
          <input id="name" required value={form.name} onChange={set('name')} />
          {err('name')}
        </div>
        <div className={field('tagline')}>
          <label htmlFor="tagline">Tagline</label>
          <input id="tagline" value={form.tagline} onChange={set('tagline')} placeholder="One line, shown in lists" />
          {err('tagline')}
        </div>
        <div className={field('description')}>
          <label htmlFor="description">Description</label>
          <textarea id="description" value={form.description} onChange={set('description')} />
          {err('description')}
        </div>
        <div className={field('kind')}>
          <label htmlFor="kind">Kind</label>
          <select id="kind" value={form.kind} onChange={set('kind')}>
            <option value="">—</option>
            {KINDS.map((k) => <option key={k} value={k}>{k}</option>)}
          </select>
          {err('kind')}
        </div>
      </fieldset>

      <fieldset>
        <legend>Links</legend>
        <div className={field('code_repository')}>
          <label htmlFor="repo">Repository</label>
          <input id="repo" value={form.code_repository} onChange={set('code_repository')} placeholder="https://github.com/…" />
          {err('code_repository')}
        </div>
        <div className={field('homepage')}>
          <label htmlFor="homepage">Homepage</label>
          <input id="homepage" value={form.homepage} onChange={set('homepage')} />
          {err('homepage')}
        </div>
        <div className={field('documentation')}>
          <label htmlFor="docs">Documentation</label>
          <input id="docs" value={form.documentation} onChange={set('documentation')} />
          {err('documentation')}
        </div>
      </fieldset>

      <fieldset>
        <legend>Images and readme</legend>
        <p className="hint" style={{ marginTop: 0 }}>
          The registry stores pointers, never bytes — these are URLs to wherever the images
          already live.
        </p>
        <div className={field('image')}>
          <label htmlFor="image">Logo or hero image</label>
          <input id="image" value={form.image} onChange={set('image')} placeholder="https://…/logo.png" />
          <p className="hint">Shown as the thumbnail in the software list.</p>
          {err('image')}
        </div>
        <div className={field('screenshots')}>
          <label htmlFor="screenshots">Screenshots — one URL per line</label>
          <textarea id="screenshots" value={form.screenshots} onChange={set('screenshots')}
                    placeholder={'https://…/screen-1.png\nhttps://…/screen-2.png'} />
          {err('screenshots')}
        </div>
        <div className={field('readme')}>
          <label htmlFor="readme">Readme — full Markdown</label>
          <textarea id="readme" value={form.readme} onChange={set('readme')} rows={14}
                    style={{ minHeight: 260, fontFamily: 'var(--mono)', fontSize: 13 }}
                    placeholder={'# Tool name\n\nParagraphs, lists, tables, code fences and images all render.'} />
          <p className="hint">
            Paste the repository's README. It renders below the short description on the tool's
            page, images and all.
          </p>
          {err('readme')}
        </div>
        <div className={field('readme_base_url')}>
          <label htmlFor="readme_base">Readme base URL</label>
          <input id="readme_base" value={form.readme_base_url} onChange={set('readme_base_url')}
                 placeholder="https://raw.githubusercontent.com/OWNER/REPO/main/" />
          <p className="hint">
            Relative images and links in the README resolve against this — a repository's raw
            content root. Without it, <code>![](docs/img.png)</code> resolves nowhere and is
            dropped rather than shown broken.
          </p>
          {err('readme_base_url')}
        </div>
      </fieldset>

      <fieldset>
        <legend>Licence and responsible party</legend>
        <div className={field('license')}>
          <label htmlFor="license">Licence (SPDX IRI)</label>
          <input id="license" value={form.license} onChange={set('license')} placeholder="https://spdx.org/licenses/Apache-2.0" />
          <p className="hint">FAIR R1.1 asks for one. A write without it is accepted but warned about.</p>
          {err('license')}
        </div>
        <div className={field('publisher.name')}>
          <label htmlFor="pub">Publisher</label>
          <input id="pub" value={form.publisher_name} onChange={set('publisher_name')} placeholder="Maastricht University — IDS" />
          {err('publisher.name')}
        </div>
        <div className="field">
          <label htmlFor="pubid">Publisher identifier (ROR or ORCID)</label>
          <input id="pubid" value={form.publisher_id} onChange={set('publisher_id')} placeholder="https://ror.org/02jz4aj89" />
          <p className="hint">Used as the Agent's IRI when given, so the same body is the same node across registries.</p>
        </div>
      </fieldset>

      <fieldset>
        <legend>Topics and keywords</legend>
        <div className="field">
          <label htmlFor="topics">EDAM topics — one IRI per line</label>
          <textarea id="topics" value={form.edam_topics} onChange={set('edam_topics')} placeholder="http://edamontology.org/topic_3071" />
        </div>
        <div className="field">
          <label htmlFor="keywords">Keywords — comma separated</label>
          <input id="keywords" value={form.keywords} onChange={set('keywords')} />
        </div>
      </fieldset>

      <fieldset>
        <legend>Capability</legend>
        <p className="hint" style={{ marginTop: 0 }}>
          Artifact types are any IRI. EDAM is the recommended default; a registry-local type
          IRI works for the things EDAM does not name.
        </p>
        <div className={field('capability.consumes')}>
          <label htmlFor="consumes">Consumes — one type IRI per line</label>
          <textarea id="consumes" value={form.consumes} onChange={set('consumes')} />
          {err('capability.consumes')}
        </div>
        <div className={field('capability.produces')}>
          <label htmlFor="produces">Produces — one type IRI per line</label>
          <textarea id="produces" value={form.produces} onChange={set('produces')} />
          {err('capability.produces')}
        </div>
      </fieldset>

      <div className="actions">
        <button type="submit" className="primary" disabled={saving || !form.name.trim()}>
          {saving ? 'Saving…' : editing ? 'Save changes' : 'Register'}
        </button>
        <button type="button" onClick={() => navigate(-1)}>Cancel</button>
      </div>
    </form>
  )
}
