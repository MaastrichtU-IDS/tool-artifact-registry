import { Link } from 'react-router-dom'
import type { Availability, Origin, TypeRef } from '../lib/types'

/** Local vs foreign. On every record header and every list row that can carry foreign data
 *  (handoff §6.1). A cached peer record never renders identically to a local one. */
export function OriginChip({
  origin, cachedNote = true, interactive = true,
}: { origin: Origin; cachedNote?: boolean; interactive?: boolean }) {
  if (origin.kind === 'local') {
    return <span className="chip plain" title="Minted by this registry, which is authoritative for it">local</span>
  }
  const name = origin.peer_title || origin.peer_base_iri || 'unknown peer'
  const body = (
    <>
      <span className="dot square" aria-hidden="true" />
      peer: {name}
    </>
  )
  return (
    <span className="inline" style={{ gap: 6 }}>
      {origin.peer_base_iri && interactive ? (
        <a className="chip peer" href={origin.peer_base_iri} target="_blank" rel="noreferrer"
           title={`Cached from ${origin.peer_base_iri}. Read-only here; its home registry is authoritative.`}>
          {body}
        </a>
      ) : (
        <span className="chip peer">{body}</span>
      )}
      {cachedNote && origin.cached_at && (
        <span className="muted" style={{ fontSize: 12 }}>cached {relative(origin.cached_at)}</span>
      )}
      {cachedNote && !origin.cached_at && origin.resolve_status !== 'live' && (
        <span className="muted" style={{ fontSize: 12 }}>not resolved yet</span>
      )}
    </span>
  )
}

/** Software kind — helps keep Software and Instance visibly different things (handoff §2.2). */
/**  renders a plain span. A chip inside a clickable row must not be a
 *  link of its own: nested anchors are invalid HTML and trap keyboard users. */
export function KindChip({ kind, interactive = true }: { kind?: string; interactive?: boolean }) {
  if (!kind) return null
  if (!interactive) return <span className="chip">{kind}</span>
  return <Link className="chip" to={`/software?kind=${encodeURIComponent(kind)}`}>{kind}</Link>
}

/** Which controlled vocabulary a term's IRI belongs to.
 *
 *  Derived from the IRI rather than hard-coded to one vocabulary: the registry already mixes
 *  several, and it will mix more. An unrecognised IRI gets no badge — better a missing label
 *  than a wrong one. */
function vocabularyOf(iri: string): string | null {
  const known: [string, string][] = [
    ['edamontology.org', 'EDAM'],
    ['data.europa.eu/8mn/euroscivoc', 'EuroSciVoc'],
    ['purl.obolibrary.org/obo/SWO', 'SWO'],
    ['spdx.org/licenses', 'SPDX'],
    ['w3id.org/', 'W3ID'],
  ]
  for (const [needle, name] of known) if (iri.includes(needle)) return name
  return null
}

/** A vocabulary term or a locally-minted type. Falls back to the IRI's last segment when
 *  unlabelled, and badges whichever vocabulary the IRI belongs to. */
export function ArtifactTypeChip({
  type, to, interactive = true,
}: { type: TypeRef; to?: string; interactive?: boolean }) {
  const label = type.label || type.iri.split(/[#/]/).filter(Boolean).pop()
  const href = to ?? `/artifacts?conforms_to=${encodeURIComponent(type.iri)}`
  const body = (
    <>
      <span className="dot" aria-hidden="true" />
      {label}
      {vocabularyOf(type.iri) && (
        <span className="muted" style={{ fontSize: 10 }}>{vocabularyOf(type.iri)}</span>
      )}
    </>
  )
  if (!interactive) {
    return <span className="chip accent" title={type.definition || type.iri}>{body}</span>
  }
  return (
    <Link className="chip accent" to={href} title={type.definition || type.iri}>
      <span className="dot" aria-hidden="true" />
      {label}
      {vocabularyOf(type.iri) && (
        <span className="muted" style={{ fontSize: 10 }}>{vocabularyOf(type.iri)}</span>
      )}
    </Link>
  )
}

const AVAILABILITY_TEXT: Record<Availability, { label: string; cls: string; title: string }> = {
  public: { label: 'public', cls: 'chip ok', title: 'Retrievable without asking anyone' },
  restricted: { label: 'restricted', cls: 'chip warn', title: 'Bytes exist but need credentials' },
  embargoed: { label: 'embargoed', cls: 'chip warn', title: 'Withheld until an embargo lifts' },
  'metadata-only': {
    label: 'metadata-only',
    cls: 'chip',
    title: 'Described and findable, not retrievable. FAIR is not open (spec §6.2).',
  },
}

export function AvailabilityBadge({ availability }: { availability?: Availability }) {
  if (!availability) return null
  const a = AVAILABILITY_TEXT[availability] ?? AVAILABILITY_TEXT['metadata-only']
  return <span className={a.cls} title={a.title}>{a.label}</span>
}

export function LicenseChip({ license, interactive = true }: { license?: string; interactive?: boolean }) {
  if (!license) {
    // Absent is not the same as unlicensed, and the UI must not conflate them.
    return <span className="chip plain muted" title="No licence has been declared">licence not stated</span>
  }
  const spdx = license.replace(/^https?:\/\/spdx\.org\/licenses\//, '')
  if (!interactive) return <span className="chip" title={license}>{spdx}</span>
  return (
    <a className="chip" href={license} target="_blank" rel="noreferrer" title={license}>{spdx}</a>
  )
}

/** Health is never colour alone — shape plus text (handoff §6.1, §8). */
export function HealthDot({ health }: { health?: string }) {
  const map: Record<string, { cls: string; dot: string; text: string }> = {
    up: { cls: 'chip ok', dot: 'dot', text: 'up' },
    down: { cls: 'chip danger', dot: 'dot square', text: 'down' },
    unknown: { cls: 'chip', dot: 'dot hollow', text: 'unknown' },
  }
  const h = map[health ?? 'unknown'] ?? map.unknown
  return (
    <span className={h.cls}>
      <span className={h.dot} aria-hidden="true" />
      {h.text}
    </span>
  )
}

export function RunStatus({ status }: { status: string }) {
  const map: Record<string, { cls: string; dot: string }> = {
    success: { cls: 'chip ok', dot: 'dot' },
    failed: { cls: 'chip danger', dot: 'dot square' },
    aborted: { cls: 'chip danger', dot: 'dot square' },
    running: { cls: 'chip warn', dot: 'dot hollow' },
  }
  const s = map[status] ?? { cls: 'chip', dot: 'dot hollow' }
  return (
    <span className={s.cls}>
      <span className={s.dot} aria-hidden="true" />
      {status}
    </span>
  )
}

/** UUIDv7 puts the timestamp first, so a leading slice is identical across records minted
 *  close together. The distinguishing bits are at the end. */
export function shortId(id: string): string {
  return id.length > 8 ? `…${id.slice(-8)}` : id
}

export function relative(iso?: string): string {
  if (!iso) return '—'
  const then = new Date(iso).getTime()
  if (Number.isNaN(then)) return iso
  const secs = Math.round((Date.now() - then) / 1000)
  const future = secs < 0
  const s = Math.abs(secs)
  const units: [number, string][] = [
    [31536000, 'y'], [2592000, 'mo'], [604800, 'w'], [86400, 'd'], [3600, 'h'], [60, 'min'],
  ]
  for (const [size, name] of units) {
    if (s >= size) {
      const n = Math.floor(s / size)
      return future ? `in ${n}${name}` : `${n}${name} ago`
    }
  }
  return future ? 'soon' : 'just now'
}

/** Relative text, absolute in the tooltip, semantic <time> underneath (handoff §6.2). */
export function RelativeTime({ iso }: { iso?: string }) {
  if (!iso) return <span className="muted">—</span>
  return <time dateTime={iso} title={new Date(iso).toLocaleString()}>{relative(iso)}</time>
}
