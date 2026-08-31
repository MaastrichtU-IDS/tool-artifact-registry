import { useEffect, useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { ApiError } from '../lib/api'

/** Copy-to-clipboard for an IRI, checksum, token or command (handoff §6.2). */
export function CopyField({ value, label, mono = true }: { value: string; label: string; mono?: boolean }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="copy-field">
      <code className={mono ? 'mono' : undefined}>{value}</code>
      <button
        type="button"
        aria-label={`Copy ${label}`}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(value)
            setCopied(true)
            setTimeout(() => setCopied(false), 1600)
          } catch {
            setCopied(false)
          }
        }}
      >
        {copied ? 'copied' : 'copy'}
      </button>
    </div>
  )
}

export function CommandBlock({ command }: { command: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="command">
      <span className="prompt" aria-hidden="true">$</span>
      <code>{command}</code>
      <button
        type="button"
        aria-label="Copy install command"
        onClick={async () => {
          await navigator.clipboard.writeText(command)
          setCopied(true)
          setTimeout(() => setCopied(false), 1600)
        }}
      >
        {copied ? 'copied' : 'copy'}
      </button>
    </div>
  )
}

/** A multi-line snippet with a copy button.
 *
 *  `CommandBlock` is for one shell line and renders a `$` prompt; this is for the several-line
 *  config blocks and JSON that a reader is meant to copy whole, where a prompt character would
 *  end up in the clipboard and break the paste. */
export function CopyBlock({ code, label }: { code: string; label: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="copy-block">
      <button
        type="button"
        className="copy-block-button"
        aria-label={`Copy ${label}`}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(code)
            setCopied(true)
            setTimeout(() => setCopied(false), 1600)
          } catch {
            // A browser that refuses clipboard access leaves the text selectable, which is
            // still a working path — saying "copied" when nothing was would not be.
            setCopied(false)
          }
        }}
      >
        {copied ? 'copied' : 'copy'}
      </button>
      <pre className="report">{code}</pre>
    </div>
  )
}

/** Label/value pairs that degrade to `—` rather than showing zeros or blanks (handoff §7). */
export function SignalBar({ signals }: { signals: { label: string; value: ReactNode; unknown?: boolean }[] }) {
  return (
    <div className="signals">
      {signals.map((s) => (
        <div className="signal" key={s.label}>
          <div className="label">{s.label}</div>
          <div className={s.unknown ? 'value dim' : 'value'}>{s.unknown ? '—' : s.value}</div>
        </div>
      ))}
    </div>
  )
}

export function Skeleton({ rows = 5 }: { rows?: number }) {
  return (
    <div aria-busy="true" aria-live="polite">
      <span className="sr-only">Loading</span>
      {Array.from({ length: rows }).map((_, i) => (
        <div className="skeleton" key={i} style={{ width: `${100 - (i % 3) * 14}%`, height: i === 0 ? 22 : 14 }} />
      ))}
    </div>
  )
}

export function EmptyState({
  title, body, action,
}: { title: string; body?: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty">
      <h3>{title}</h3>
      {body && <p>{body}</p>}
      {action}
    </div>
  )
}

/** RFC 9457 rendered honestly, with the SHACL report behind a disclosure (handoff §6.2). */
export function ProblemJsonError({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  if (error instanceof ApiError) {
    const p = error.problem
    return (
      <div className="banner danger" role="alert">
        <h3>{p.title || 'Request failed'}</h3>
        {p.detail && <p>{p.detail}</p>}
        {p.report && (
          <details className="disclosure">
            <summary>Show validation report</summary>
            <pre className="report">{p.report}</pre>
          </details>
        )}
        {onRetry && (
          <div className="actions">
            <button type="button" onClick={onRetry}>Retry</button>
          </div>
        )}
      </div>
    )
  }
  return (
    <div className="banner danger" role="alert">
      <h3>Something went wrong</h3>
      <p>{error instanceof Error ? error.message : String(error)}</p>
      {onRetry && (
        <div className="actions">
          <button type="button" onClick={onRetry}>Retry</button>
        </div>
      )}
    </div>
  )
}

export function ErrorState({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  return <ProblemJsonError error={error} onRetry={onRetry} />
}

/** Persistent IRI, citation exports and a version selector (handoff §5.2). */
export function CiteBlock({
  iri, title, version, versions,
}: { iri: string; title: string; version?: string; versions?: { label: string; href: string }[] }) {
  const year = new Date().getFullYear()
  const bibtex = `@software{${slug(title)},\n  title = {${title}},\n  url = {${iri}},\n  year = {${year}}${version ? `,\n  version = {${version}}` : ''}\n}`
  const ris = `TY  - COMP\nTI  - ${title}\nUR  - ${iri}\nPY  - ${year}\n${version ? `ET  - ${version}\n` : ''}ER  -`
  return (
    <section className="card rail-section">
      <h2>Cite</h2>
      <CopyField value={iri} label="persistent IRI" />
      <div className="actions" style={{ marginTop: 10 }}>
        <button type="button" onClick={() => download(`${slug(title)}.bib`, bibtex)}>BibTeX</button>
        <button type="button" onClick={() => download(`${slug(title)}.ris`, ris)}>RIS</button>
      </div>
      {versions && versions.length > 1 && (
        <div className="field" style={{ marginTop: 12, marginBottom: 0 }}>
          <label htmlFor="cite-version">Version</label>
          <select id="cite-version" defaultValue={versions[0].href}
                  onChange={(e) => { window.location.href = e.target.value }}>
            {versions.map((v) => <option key={v.href} value={v.href}>{v.label}</option>)}
          </select>
        </div>
      )}
    </section>
  )
}

/** Machine-readable is a first-class affordance: hiding the RDF in a FAIR tool would be
 *  absurd (handoff §2.4). These hit the same IRI with a pinned extension. */
export function FairDownloads({ iri, biotools }: { iri: string; biotools?: string }) {
  return (
    <section className="card rail-section">
      <h2>FAIR</h2>
      <p className="hint" style={{ marginTop: 0 }}>
        This record is RDF. The IRI content-negotiates; these links pin a format.
      </p>
      <ul className="chips">
        <li><a className="chip" href={`${iri}.ttl`}>⬇ Turtle</a></li>
        <li><a className="chip" href={`${iri}.jsonld`}>⬇ JSON-LD</a></li>
        {biotools && <li><a className="chip" href={biotools}>⬇ biotoolsSchema</a></li>}
      </ul>
    </section>
  )
}

export function KeysetPager({
  cursor, nextCursor, onCursor, count, total,
}: {
  cursor?: string
  nextCursor?: string
  onCursor: (c: string | undefined) => void
  count: number
  total: number
}) {
  if (!nextCursor && !cursor) return null
  return (
    <div className="spread" style={{ marginTop: 14 }}>
      <span className="muted">Showing {count} of {total}</span>
      <div className="actions" style={{ margin: 0 }}>
        {cursor && <button type="button" onClick={() => onCursor(undefined)}>First page</button>}
        {nextCursor && <button type="button" onClick={() => onCursor(nextCursor)}>Next →</button>}
      </div>
    </div>
  )
}

/** Focus is trapped and restored; the token modal is dismissible only by an explicit action
 *  because the value it shows is unrecoverable (handoff §8). */
export function Modal({
  title, children, onClose, dismissible = true,
}: { title: string; children: ReactNode; onClose: () => void; dismissible?: boolean }) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    ref.current?.querySelector<HTMLElement>('button, [href], input, select, textarea')?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && dismissible) onClose()
      if (e.key !== 'Tab' || !ref.current) return
      const focusable = ref.current.querySelectorAll<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      )
      if (focusable.length === 0) return
      const first = focusable[0]
      const last = focusable[focusable.length - 1]
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault()
        last.focus()
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault()
        first.focus()
      }
    }
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('keydown', onKey)
      previous?.focus()
    }
  }, [onClose, dismissible])

  return (
    <div className="modal-backdrop" onMouseDown={(e) => { if (dismissible && e.target === e.currentTarget) onClose() }}>
      <div className="modal" role="dialog" aria-modal="true" aria-label={title} ref={ref}>
        <h2>{title}</h2>
        {children}
      </div>
    </div>
  )
}

export function Tombstone({ what }: { what: string }) {
  return (
    <div className="banner warn" role="status">
      <h3>This {what} has been withdrawn</h3>
      <p>
        The record is kept so its IRI keeps resolving and existing lineage stays readable.
        It can no longer be edited, and it should not be cited as current.
      </p>
    </div>
  )
}

export function ForeignNotice({ homeIri }: { homeIri?: string }) {
  return (
    <div className="banner peer" role="status">
      <h3>Cached from another registry</h3>
      <p>
        This is a read-only copy and may be out of date.{' '}
        {homeIri && <a href={homeIri} target="_blank" rel="noreferrer">Open it at its home registry →</a>}
      </p>
    </div>
  )
}

export function Breadcrumb({ items }: { items: { label: string; to?: string }[] }) {
  return (
    <nav aria-label="Breadcrumb" style={{ fontSize: 13, marginBottom: 8 }}>
      {items.map((it, i) => (
        <span key={i}>
          {i > 0 && <span className="muted"> / </span>}
          {it.to ? <Link to={it.to}>{it.label}</Link> : <span className="muted">{it.label}</span>}
        </span>
      ))}
    </nav>
  )
}

function slug(s: string) {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '')
}

function download(filename: string, content: string) {
  const blob = new Blob([content], { type: 'text/plain' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  a.click()
  URL.revokeObjectURL(url)
}

export function formatBytes(n?: number): string {
  if (n === undefined || n === null) return '—'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let v = n
  let u = 0
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024
    u++
  }
  return `${v < 10 && u > 0 ? v.toFixed(1) : Math.round(v)} ${units[u]}`
}

export function formatDuration(seconds?: number): string {
  if (seconds === undefined || seconds === null) return '—'
  if (seconds < 60) return `${seconds}s`
  const m = Math.floor(seconds / 60)
  const s = seconds % 60
  if (m < 60) return s ? `${m}m ${s}s` : `${m}m`
  const h = Math.floor(m / 60)
  return `${h}h ${m % 60}m`
}
