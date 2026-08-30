import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'
import { ProblemJsonError, Skeleton } from '../components/common'
import { TermPicker } from '../components/TermPicker'

const KINDS = ['service', 'library', 'cli', 'desktop', 'workflow']
// repostatus.org concepts, which is what CodeMeta's developmentStatus ranges over.
const MATURITIES = ['concept', 'wip', 'active', 'inactive', 'unsupported', 'suspended', 'abandoned', 'moved']

/** Sectioned to mirror the model: identity → links → licence and party → topics → capability
 *  (handoff §5.7). Validation failures come back as a SHACL report and are mapped to fields. */
export default function SoftwareForm() {
  const { id } = useParams()
  const editing = Boolean(id)
  const navigate = useNavigate()
  const { isCurator, loading: sessionLoading } = useSession()

  const [form, setForm] = useState({
    name: '', tagline: '', description: '', homepage: '', code_repository: '', documentation: '',
    license: '', maturity: '', keywords: '', publisher_name: '',
    publisher_id: '', contact_name: '', contact_id: '', contact_email: '', publications: '',
    image: '', screenshots: '', readme: '', readme_base_url: '', download_url: '',
  })
  const [kinds, setKinds] = useState<string[]>([])
  const [topics, setTopics] = useState<string[]>([])
  const [consumes, setConsumes] = useState<string[]>([])
  const [produces, setProduces] = useState<string[]>([])
  const [deployable, setDeployable] = useState(true)
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
        documentation: s.documentation ?? '', license: s.license ?? '',
        image: s.image ?? '',
        screenshots: s.screenshots.join('\n'),
        readme: s.readme ?? '',
        readme_base_url: s.readme_base_url ?? '',
        download_url: s.download_url ?? '',
        maturity: s.maturity ?? '', keywords: s.keywords.join(', '),
        publisher_name: s.publisher?.name ?? '', publisher_id: s.publisher?.identifier ?? '',
        contact_name: s.contact?.name ?? '', contact_id: s.contact?.identifier ?? '',
        contact_email: s.contact?.email ?? '', publications: s.publications.join('\n'),
      })
      setKinds(s.kinds ?? (s.kind ? [s.kind] : []))
      setTopics(s.edam_topics.map((t) => t.iri))
      setConsumes((s.capability?.consumes ?? []).map((t) => t.iri))
      setProduces((s.capability?.produces ?? []).map((t) => t.iri))
      setDeployable(s.deployable)
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
      download_url: form.download_url || undefined,
      deployable,
      image: form.image || undefined,
      screenshots: lines(form.screenshots),
      readme: form.readme || undefined,
      readme_base_url: form.readme_base_url || undefined,
      license: form.license || undefined,
      kinds,
      maturity: form.maturity || undefined,
      keywords: commas(form.keywords),
      edam_topics: topics,
      publications: lines(form.publications),
      publisher: form.publisher_name || form.publisher_id
        ? { name: form.publisher_name || undefined, identifier: form.publisher_id || undefined, kind: 'organization' }
        : undefined,
      contact: form.contact_name || form.contact_id || form.contact_email
        ? {
            name: form.contact_name || undefined,
            identifier: form.contact_id || undefined,
            email: form.contact_email || undefined,
            kind: 'person',
          }
        : undefined,
      capability: consumes.length || produces.length ? { consumes, produces } : undefined,
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
        <div className={field('kinds')}>
          <label>Kind</label>
          <p className="hint" style={{ marginTop: 0 }}>
            More than one can apply — a tool with a desktop build and a hosted deployment is
            both, and picking one would make the record wrong about the other.
          </p>
          <div className="inline">
            {KINDS.map((k) => (
              <label key={k} style={{ fontWeight: 400, display: 'flex', gap: 6, alignItems: 'center', margin: 0 }}>
                <input
                  type="checkbox"
                  style={{ width: 'auto' }}
                  checked={kinds.includes(k)}
                  onChange={(e) => setKinds(e.target.checked ? [...kinds, k] : kinds.filter((x) => x !== k))}
                />
                {k}
              </label>
            ))}
          </div>
          {err('kinds')}
        </div>
        <div className={field('maturity')}>
          <label htmlFor="maturity">Maturity</label>
          <select id="maturity" value={form.maturity} onChange={set('maturity')} style={{ width: 240 }}>
            <option value="">—</option>
            {MATURITIES.map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
          <p className="hint">A repostatus.org concept, which is what CodeMeta's developmentStatus expects.</p>
          {err('maturity')}
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
        <div className={field('download_url')}>
          <label htmlFor="download">Download</label>
          <input id="download" value={form.download_url} onChange={set('download_url')}
                 placeholder="https://github.com/OWNER/REPO/releases" />
          <p className="hint">
            Where to get it — a releases page, a package listing, an installer. For software that
            cannot be hosted this is the link that matters most.
          </p>
          {err('download_url')}
        </div>
        <div className="field">
          <label style={{ fontWeight: 400, display: 'flex', gap: 8, alignItems: 'center' }}>
            <input type="checkbox" style={{ width: 'auto' }} checked={!deployable}
                   onChange={(e) => setDeployable(!e.target.checked)} />
            This software cannot be hosted — it runs on a machine
          </label>
          <p className="hint">
            A desktop application or a CLI. Its instances are installations, and the registry
            will refuse an endpoint on any of them.
          </p>
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
        <div className="field">
          <label htmlFor="contact">Contact</label>
          <input id="contact" value={form.contact_name} onChange={set('contact_name')} placeholder="Name of the person to ask" />
        </div>
        <div className="field">
          <label htmlFor="contact_id">Contact ORCID</label>
          <input id="contact_id" value={form.contact_id} onChange={set('contact_id')} placeholder="https://orcid.org/0000-0002-1825-0097" />
        </div>
        <div className="field">
          <label htmlFor="contact_email">Contact email</label>
          <input id="contact_email" type="email" value={form.contact_email} onChange={set('contact_email')} />
        </div>
      </fieldset>

      <fieldset>
        <legend>Topics and keywords</legend>
        <TermPicker
          id="topics"
          label="EDAM topics"
          branch="topic"
          value={topics}
          onChange={setTopics}
          placeholder="ontology, data management, imaging…"
          hint="What the tool is about. Search EDAM by name."
        />
        <div className="field">
          <label htmlFor="keywords">Keywords — comma separated</label>
          <input id="keywords" value={form.keywords} onChange={set('keywords')} />
          <p className="hint">Free text, for things EDAM does not cover.</p>
        </div>
        <div className="field">
          <label htmlFor="publications">Publications — one DOI or URL per line</label>
          <textarea id="publications" value={form.publications} onChange={set('publications')}
                    placeholder="https://doi.org/10.1234/example" />
        </div>
      </fieldset>

      <fieldset>
        <legend>Capability</legend>
        <p className="hint" style={{ marginTop: 0 }}>
          Artifact types are any IRI. EDAM is the recommended default; a registry-local type
          IRI works for the things EDAM does not name.
        </p>
        <TermPicker
          id="consumes"
          label="Consumes"
          branch="data"
          value={consumes}
          onChange={setConsumes}
          placeholder="ontology, alignment, sequence…"
          hint="What it takes in. EDAM data types and this registry's own types both appear; an IRI can be pasted directly."
        />
        <TermPicker
          id="produces"
          label="Produces"
          branch="data"
          value={produces}
          onChange={setProduces}
          placeholder="report, ontology, mapping…"
          hint="What it emits."
        />
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
