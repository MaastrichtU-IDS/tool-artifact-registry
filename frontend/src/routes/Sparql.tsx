import { useCallback, useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { api, ApiError } from '../lib/api'
import { useSession } from '../lib/session'
import { EmptyState, ProblemJsonError } from '../components/common'
import type { SparqlAnswer, SparqlTerm } from '../lib/types'

/**
 * Read-only SPARQL (spec §7.7). The endpoint is "a first-class surface, not a bonus", so the
 * tab has to be usable by an analyst who has never read `shapes/vocab.ttl`: the examples run
 * against this schema, the named graphs and prefixes are on screen, and the one trap that
 * makes a first query silently return nothing — the default graph is empty, everything lives
 * in a named graph — is stated rather than left to be discovered.
 */

/** Rendering an unbounded result set would lock the tab. The cap is announced, never silent. */
const ROW_CAP = 500

const PREFIXES: { prefix: string; iri: string }[] = [
  { prefix: 'tar', iri: 'https://w3id.org/tar/ns#' },
  { prefix: 'dcat', iri: 'http://www.w3.org/ns/dcat#' },
  { prefix: 'dct', iri: 'http://purl.org/dc/terms/' },
  { prefix: 'prov', iri: 'http://www.w3.org/ns/prov#' },
  { prefix: 'schema', iri: 'https://schema.org/' },
  { prefix: 'skos', iri: 'http://www.w3.org/2004/02/skos/core#' },
  { prefix: 'rdfs', iri: 'http://www.w3.org/2000/01/rdf-schema#' },
  { prefix: 'spdx', iri: 'http://spdx.org/rdf/terms#' },
  { prefix: 'codemeta', iri: 'https://w3id.org/codemeta/terms/' },
  { prefix: 'adms', iri: 'http://www.w3.org/ns/adms#' },
]

/** Spec §5.4. Provenance of every triple is recoverable by construction, which is exactly why
 *  a query has to name the graph it means. */
const GRAPHS: { name: string; what: string }[] = [
  { name: 'urn:tar:local', what: 'authoritative — triples this registry minted' },
  { name: 'urn:tar:peer:{id}', what: 'cached foreign stubs, read-only' },
  { name: 'urn:tar:shapes', what: 'SHACL shapes used for write validation' },
  { name: 'urn:tar:vocab', what: 'preloaded vocabulary terms (topics, data types, licences)' },
]

interface Example {
  title: string
  note: string
  query: string
}

/** Every one of these was run against a seeded registry and returns rows; they are the answer
 *  to "what can I ask?" for someone who does not know the model. */
const EXAMPLES: Example[] = [
  {
    title: 'Every tool, with its licence',
    note: 'The list screen, as a query. Start here.',
    query: `PREFIX tar: <https://w3id.org/tar/ns#>
PREFIX schema: <https://schema.org/>
PREFIX dct: <http://purl.org/dc/terms/>

SELECT ?software ?name ?tagline ?license WHERE {
  GRAPH <urn:tar:local> {
    ?software a tar:Software ; schema:name ?name .
    OPTIONAL { ?software dct:abstract ?tagline }
    OPTIONAL { ?software dct:license  ?license }
  }
}
ORDER BY ?name`,
  },
  {
    title: 'What each tool consumes and produces',
    note: 'The capability declaration — the matchmaking answer, available before anything has run.',
    query: `PREFIX tar: <https://w3id.org/tar/ns#>
PREFIX schema: <https://schema.org/>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>

SELECT ?name ?direction ?type ?typeLabel WHERE {
  GRAPH <urn:tar:local> {
    ?software a tar:Software ; schema:name ?name ; tar:hasCapability ?cap .
    { ?cap tar:consumes ?type . BIND("consumes" AS ?direction) }
    UNION
    { ?cap tar:produces ?type . BIND("produces" AS ?direction) }
  }
  OPTIONAL { GRAPH ?any { ?type skos:prefLabel ?typeLabel } }
}
ORDER BY ?name ?direction ?typeLabel`,
  },
  {
    title: 'Deployments and the release each one runs',
    note: 'Software is abstract; an Instance is a concrete deployment that can act.',
    query: `PREFIX tar: <https://w3id.org/tar/ns#>
PREFIX schema: <https://schema.org/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX dct: <http://purl.org/dc/terms/>

SELECT ?deployment ?label ?tool ?version ?operator WHERE {
  GRAPH <urn:tar:local> {
    ?deployment a tar:Instance ; rdfs:label ?label .
    OPTIONAL { ?deployment tar:instanceOf  ?sw  . ?sw  schema:name            ?tool    }
    OPTIONAL { ?deployment tar:runsRelease ?rel . ?rel schema:softwareVersion ?version }
    OPTIONAL { ?deployment dct:publisher ?operator }
  }
}
ORDER BY ?label`,
  },
  {
    title: 'Releases, newest first',
    note: 'A Release is the versioned, runnable plan — the container image lives here.',
    query: `PREFIX tar: <https://w3id.org/tar/ns#>
PREFIX schema: <https://schema.org/>
PREFIX dct: <http://purl.org/dc/terms/>

SELECT ?tool ?version ?published ?image WHERE {
  GRAPH <urn:tar:local> {
    ?release a tar:Release ; schema:softwareVersion ?version ; dct:isVersionOf ?sw .
    ?sw schema:name ?tool .
    OPTIONAL { ?release schema:datePublished ?published }
    OPTIONAL { ?release tar:containerImage   ?image     }
  }
}
ORDER BY DESC(?published)`,
  },
  {
    title: 'Artifacts, and whether the bytes are reachable',
    note: 'FAIR is not open: metadata-only means described and provably not retrievable.',
    query: `PREFIX tar: <https://w3id.org/tar/ns#>
PREFIX dcat: <http://www.w3.org/ns/dcat#>
PREFIX dct: <http://purl.org/dc/terms/>

SELECT ?artifact ?title ?availability ?mediaType ?bytes ?downloadURL WHERE {
  GRAPH <urn:tar:local> {
    ?artifact a dcat:Dataset ; dct:title ?title ; dcat:distribution ?dist .
    ?dist tar:availability ?availability .
    OPTIONAL { ?dist dcat:mediaType   ?mediaType   }
    OPTIONAL { ?dist dcat:byteSize    ?bytes       }
    OPTIONAL { ?dist dcat:downloadURL ?downloadURL }
  }
}`,
  },
  {
    title: 'Which predicates does the local graph actually use?',
    note: 'Schema discovery. The honest way to find out what you can ask for next.',
    query: `SELECT ?predicate (COUNT(*) AS ?uses) WHERE {
  GRAPH <urn:tar:local> { ?s ?predicate ?o }
}
GROUP BY ?predicate
ORDER BY DESC(?uses)`,
  },
  {
    title: 'Named graphs and how big each one is',
    note: 'Orientation: where the triples in this registry actually live.',
    query: `SELECT ?graph (COUNT(*) AS ?triples) WHERE {
  GRAPH ?graph { ?s ?p ?o }
}
GROUP BY ?graph
ORDER BY DESC(?triples)`,
  },
  {
    title: 'Can any tool here consume what another one produces? (ASK)',
    note: 'An ASK answers yes or no, with no bindings.',
    query: `PREFIX tar: <https://w3id.org/tar/ns#>

ASK {
  GRAPH <urn:tar:local> {
    ?producer tar:hasCapability/tar:produces ?type .
    ?consumer tar:hasCapability/tar:consumes ?type .
    FILTER(?producer != ?consumer)
  }
}`,
  },
  {
    title: 'One tool as RDF (CONSTRUCT)',
    note: 'CONSTRUCT and DESCRIBE answer in Turtle — this is what a peer registry asks for.',
    query: `PREFIX tar: <https://w3id.org/tar/ns#>

CONSTRUCT { ?software ?p ?o }
WHERE {
  GRAPH <urn:tar:local> {
    ?software a tar:Software ; ?p ?o
  }
}
LIMIT 60`,
  },
]

export default function Sparql() {
  const { registry } = useSession()
  const base = registry?.base_iri ?? ''
  const [query, setQuery] = useState(EXAMPLES[0].query)
  const [ran, setRan] = useState<string>()
  const [answer, setAnswer] = useState<SparqlAnswer>()
  const [error, setError] = useState<unknown>()
  const [running, setRunning] = useState(false)
  const editor = useRef<HTMLTextAreaElement>(null)
  const inFlight = useRef<AbortController>()

  useEffect(() => () => inFlight.current?.abort(), [])

  const run = useCallback(async (text: string) => {
    const q = text.trim()
    if (!q) return
    inFlight.current?.abort()
    const controller = new AbortController()
    inFlight.current = controller
    setRunning(true)
    setError(undefined)
    try {
      const result = await api.sparql(q, controller.signal)
      if (controller.signal.aborted) return
      setAnswer(result)
      setRan(q)
    } catch (e) {
      if (controller.signal.aborted || (e instanceof DOMException && e.name === 'AbortError')) return
      setAnswer(undefined)
      setRan(q)
      setError(e)
    } finally {
      if (!controller.signal.aborted) setRunning(false)
    }
  }, [])

  return (
    <section className="detail">
      <div>
        <div className="page-header">
          <h1>SPARQL</h1>
          <p className="tagline">
            Read-only SPARQL 1.1 over this registry&rsquo;s graph. Writes are refused here —
            they go through the REST API, where they are validated and audited.
          </p>
        </div>

        <form
          className="card"
          onSubmit={(e) => {
            e.preventDefault()
            void run(query)
          }}
        >
          <label htmlFor="sparql-query">Query</label>
          <textarea
            id="sparql-query"
            ref={editor}
            className="sparql-editor mono"
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              // Ctrl/Cmd+Enter runs. Tab is deliberately left alone: capturing it would trap
              // keyboard users inside the textarea (handoff §8).
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault()
                void run(query)
              }
            }}
          />
          <div className="actions" style={{ marginTop: 10 }}>
            <button type="submit" className="primary" disabled={running || !query.trim()}>
              {running ? 'Running…' : 'Run query'}
            </button>
            <span className="hint" style={{ margin: 0 }}>
              <kbd className="kbd">Ctrl</kbd>/<kbd className="kbd">⌘</kbd> +{' '}
              <kbd className="kbd">Enter</kbd> runs it
            </span>
          </div>
        </form>

        <div aria-live="polite">
          {error !== undefined && <QueryError error={error} onRetry={() => void run(query)} />}
          {error === undefined && answer && <Answer answer={answer} query={ran ?? ''} base={base} />}
          {error === undefined && !answer && !running && (
            <EmptyState
              title="No query run yet"
              body="Edit the query above and run it, or pick one of the examples."
            />
          )}
        </div>
      </div>

      <aside className="rail">
        <section className="card rail-section">
          <h2>Example queries</h2>
          <p className="hint" style={{ marginTop: 0 }}>
            Each one loads into the editor and runs against this registry.
          </p>
          <ul className="example-list">
            {EXAMPLES.map((ex) => (
              <li key={ex.title}>
                <button
                  type="button"
                  className="link"
                  onClick={() => {
                    setQuery(ex.query)
                    editor.current?.focus()
                    void run(ex.query)
                  }}
                >
                  {ex.title}
                </button>
                <p className="hint">{ex.note}</p>
              </li>
            ))}
          </ul>
        </section>

        <section className="card rail-section">
          <h2>Named graphs</h2>
          <p className="hint" style={{ marginTop: 0 }}>
            The default graph is empty: every triple lives in a named graph, so a pattern
            outside <code>GRAPH</code> (or <code>FROM</code>) matches nothing.
          </p>
          <dl>
            {GRAPHS.map((g) => (
              <div key={g.name}>
                <dt className="mono" style={{ textTransform: 'none', letterSpacing: 0 }}>
                  {g.name}
                </dt>
                <dd className="hint" style={{ marginTop: 0 }}>{g.what}</dd>
              </div>
            ))}
          </dl>
        </section>

        <section className="card rail-section">
          <h2>Prefixes</h2>
          <p className="hint" style={{ marginTop: 0 }}>
            Nothing is bound for you — declare what you use.
          </p>
          <ul className="prefix-list mono">
            {PREFIXES.map((p) => (
              <li key={p.prefix}>
                <span className="prefix-name">{p.prefix}:</span>
                <span className="muted">{p.iri}</span>
              </li>
            ))}
          </ul>
        </section>
      </aside>
    </section>
  )
}

/** The server says what went wrong — a parse error with its position, or the read-only
 *  refusal. Repeating it verbatim is more useful than any wording we could invent. */
function QueryError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className="banner danger" role="alert">
        <h3>Refused: this endpoint is read-only</h3>
        <p>{error.problem.detail ?? error.message}</p>
      </div>
    )
  }
  if (error instanceof ApiError && error.status === 400) {
    const detail = error.problem.detail ?? error.message
    // The parser reports `error at LINE:COLUMN` and then every token it would have accepted.
    // The position is the actionable part, so it is promoted; the rest is kept verbatim
    // underneath rather than paraphrased away.
    const at = /error at (\d+):(\d+)/.exec(detail)
    return (
      <div className="banner danger" role="alert">
        <h3>
          The server could not parse this query
          {at && ` — line ${at[1]}, column ${at[2]}`}
        </h3>
        <details className="disclosure" open>
          <summary>What the server said</summary>
          <pre className="report sparql-parse-error">{detail}</pre>
        </details>
      </div>
    )
  }
  return <ProblemJsonError error={error} onRetry={onRetry} />
}

function Answer({ answer, query, base }: { answer: SparqlAnswer; query: string; base: string }) {
  if (answer.form === 'ask') {
    return (
      <div className="card">
        <h2>Answer</h2>
        <p className={answer.boolean ? 'ask-answer yes' : 'ask-answer no'}>
          {/* Never colour alone (handoff §6.1): the word carries the answer. */}
          {answer.boolean ? 'Yes' : 'No'}
        </p>
        <p className="hint">
          An <code>ASK</code> returns a boolean, not bindings.{' '}
          {answer.boolean
            ? 'At least one solution matches this pattern.'
            : 'Nothing in the graph matches this pattern.'}
        </p>
      </div>
    )
  }

  if (answer.form === 'graph') {
    const empty = answer.turtle.trim().length === 0
    return (
      <div className="card">
        <h2>Turtle</h2>
        {empty ? (
          <EmptyState
            title="No triples"
            body="The query ran and constructed nothing. That is an answer, not a failure."
          />
        ) : (
          <>
            <p className="hint" style={{ marginTop: 0 }}>
              {answer.turtle.split('\n').length} lines of Turtle. CONSTRUCT and DESCRIBE answer
              as a graph, so there is no table to show.
            </p>
            <pre className="report sparql-turtle">{answer.turtle}</pre>
          </>
        )}
      </div>
    )
  }

  if (answer.rows.length === 0) {
    // Empty is not an error and must not look like one — but the single most likely cause
    // deserves naming.
    const namesAGraph = /\bGRAPH\b|\bFROM\b/i.test(query)
    return (
      <div className="card">
        <h2>0 rows</h2>
        <EmptyState
          title="The query ran and matched nothing"
          body={
            namesAGraph
              ? 'That is a valid answer: no solution in this registry satisfies the pattern.'
              : 'This query does not name a graph. The default graph here is empty — wrap the pattern in GRAPH <urn:tar:local> { … } and try again.'
          }
        />
      </div>
    )
  }

  const shown = answer.rows.slice(0, ROW_CAP)
  const capped = answer.rows.length > ROW_CAP
  return (
    <div className="card flush">
      <div className="spread" style={{ padding: '12px 16px 8px' }}>
        <h2 style={{ margin: 0 }}>
          {answer.rows.length} {answer.rows.length === 1 ? 'row' : 'rows'}
        </h2>
        {capped && (
          <span className="chip warn">Showing the first {ROW_CAP} — add a LIMIT to narrow it</span>
        )}
      </div>
      <div className="table-scroll">
        <table>
          <thead>
            <tr>
              {answer.vars.map((v) => (
                <th key={v} scope="col">?{v}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {shown.map((row, i) => (
              <tr key={i}>
                {answer.vars.map((v) => (
                  <td key={v}>
                    {/* An unbound variable in an OPTIONAL is `—`, never a blank cell. */}
                    {row[v] ? <TermCell term={row[v]} base={base} /> : <span className="muted">—</span>}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

/** Which SPA routes exist for a locally-minted IRI. Registry IRIs and UI routes are the same
 *  URLs (handoff §3), but only these four kinds have a screen — linking a `/release/…` or
 *  `/distribution/…` IRI would land the reader on a 404, so those stay plain text. */
const ROUTED: Record<string, string> = {
  software: '/software',
  instance: '/instances',
  instances: '/instances',
  artifact: '/artifacts',
  artifacts: '/artifacts',
  run: '/runs',
  runs: '/runs',
}

export function isLocal(iri: string, base: string): boolean {
  return Boolean(base) && iri.startsWith(`${base}/`)
}

export function routeFor(iri: string, base: string): string | undefined {
  if (!isLocal(iri, base)) return undefined
  const [kind, id, ...rest] = iri.slice(base.length + 1).split('/')
  if (rest.length > 0 || !id) return undefined
  // An ArtifactType has no page of its own; the useful destination is what conforms to it,
  // which is exactly where an ArtifactTypeChip goes (handoff §5.2).
  if (kind === 'type') return `/artifacts?conforms_to=${encodeURIComponent(iri)}`
  const path = ROUTED[kind]
  return path ? `${path}/${id}` : undefined
}

function TermCell({ term, base }: { term: SparqlTerm; base: string }) {
  if (term.type === 'bnode') {
    return <code className="mono muted" title="blank node">_:{term.value}</code>
  }

  if (term.type === 'uri') {
    const internal = routeFor(term.value, base)
    if (internal) {
      return (
        <Link to={internal} className="mono" title={term.value}>
          {shorten(term.value, base)}
        </Link>
      )
    }
    if (isLocal(term.value, base)) {
      // Minted here, but no screen renders it — a Release, Distribution, Capability or Agent.
      // Linking it would land the reader on "No such record here", so it stays text; the RDF
      // is still one `.ttl` away for anyone who wants it.
      return (
        <code
          className="mono muted"
          title={`${term.value} — no screen for this record; append .ttl for its RDF`}
        >
          {shorten(term.value, base)}
        </code>
      )
    }
    return (
      <a className="mono" href={term.value} target="_blank" rel="noreferrer" title={term.value}>
        {shorten(term.value, base)}
      </a>
    )
  }

  const lang = term['xml:lang']
  return (
    <span className="term-literal">
      <span className="literal-value">{term.value}</span>
      {lang && <span className="chip plain term-tag" title={`language tag ${lang}`}>@{lang}</span>}
      {!lang && term.datatype && (
        <span className="chip plain term-tag" title={term.datatype}>
          {curie(term.datatype)}
        </span>
      )}
    </span>
  )
}

/** A full IRI in every cell makes a table unreadable; the whole IRI stays in `title`. */
function shorten(iri: string, base: string): string {
  if (base && iri.startsWith(`${base}/`)) return iri.slice(base.length)
  for (const p of PREFIXES) {
    if (iri.startsWith(p.iri)) return `${p.prefix}:${iri.slice(p.iri.length)}`
  }
  return iri.replace(/^https?:\/\//, '')
}

function curie(iri: string): string {
  if (iri.startsWith('http://www.w3.org/2001/XMLSchema#')) {
    return `xsd:${iri.slice('http://www.w3.org/2001/XMLSchema#'.length)}`
  }
  for (const p of PREFIXES) {
    if (iri.startsWith(p.iri)) return `${p.prefix}:${iri.slice(p.iri.length)}`
  }
  return iri.split(/[#/]/).filter(Boolean).pop() ?? iri
}
