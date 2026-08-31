import { useEffect, useRef, useState } from 'react'
import { vocabularyOf } from './chips'
import { api } from '../lib/api'

export interface Term {
  iri: string
  label: string
  definition?: string
  source?: string
  branch?: string
}

/**
 * Pick vocabulary terms by name instead of by IRI.
 *
 * Nobody should have to know that "data management" is `http://edamontology.org/topic_3071`.
 * This searches the registry's bundled vocabularies plus every type the registry has minted or
 * adopted, and shows what was chosen as removable chips.
 *
 * A write may only name a term the registry holds, so pasting an unknown IRI is no longer an
 * escape hatch — it is a `422` a few clicks later. The escape hatch is here instead, and it is
 * the same one the API offers: type a name nothing matches and the picker offers to record it as
 * a type; paste an IRI the term already has elsewhere and it offers to adopt it under *that*
 * identifier rather than minting a second name for the same thing. Either way what leaves this
 * component is an IRI the registry will accept, which is the whole point — a restriction that
 * leaves a curator stuck is a restriction they route around.
 */
export function TermPicker({
  value, onChange, branch, label, hint, placeholder, id, allowRegister,
}: {
  value: string[]
  onChange: (next: string[]) => void
  /** `topic` or `data`; omit to search everything. */
  branch?: 'topic' | 'data'
  label: string
  hint?: string
  placeholder?: string
  id: string
  /** Offer to register an unmatched term. Defaults on for artifact types, which are the ones
   *  `POST /api/v1/types` can create; topics come from a vocabulary this registry does not own. */
  allowRegister?: boolean
}) {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<Term[]>([])
  const [labels, setLabels] = useState<Record<string, string>>({})
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const [busy, setBusy] = useState(false)
  const [registering, setRegistering] = useState(false)
  const [registerError, setRegisterError] = useState<string | null>(null)
  const box = useRef<HTMLDivElement>(null)

  // Resolve labels for values we were handed, so editing an existing record shows names
  // rather than the IRIs the user was trying to avoid in the first place.
  useEffect(() => {
    const unknown = value.filter((v) => !labels[v])
    if (unknown.length === 0) return
    fetch(`/api/v1/vocab/resolve?iris=${encodeURIComponent(unknown.join(','))}`)
      .then((r) => (r.ok ? r.json() : []))
      .then((terms: Term[]) => {
        setLabels((prev) => {
          const next = { ...prev }
          for (const t of terms) next[t.iri] = t.label ?? t.iri
          return next
        })
      })
      .catch(() => {
        // A failed lookup is not an error worth showing: the chip falls back to the IRI tail.
      })
  }, [value, labels])

  useEffect(() => {
    if (query.trim().length < 2) {
      setResults([])
      return
    }
    let cancelled = false
    setBusy(true)
    const t = setTimeout(() => {
      const params = new URLSearchParams({ q: query, limit: '8' })
      if (branch) params.set('branch', branch)
      fetch(`/api/v1/vocab/search?${params}`)
        .then((r) => (r.ok ? r.json() : { items: [] }))
        .then((d) => {
          if (cancelled) return
          setResults((d.items ?? []).filter((t: Term) => !value.includes(t.iri)))
          setActive(0)
        })
        .catch(() => !cancelled && setResults([]))
        .finally(() => !cancelled && setBusy(false))
    }, 180) // debounce: one request per pause, not per keystroke
    return () => {
      cancelled = true
      clearTimeout(t)
    }
  }, [query, branch, value])

  useEffect(() => {
    const away = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', away)
    return () => document.removeEventListener('mousedown', away)
  }, [])

  const add = (iri: string, name?: string) => {
    if (!iri || value.includes(iri)) return
    if (name) setLabels((p) => ({ ...p, [iri]: name }))
    onChange([...value, iri])
    setQuery('')
    setResults([])
    setOpen(false)
  }

  const typed = query.trim()
  const isIri = /^https?:\/\/\S+$/i.test(typed)
  const canRegister = (allowRegister ?? branch === 'data') && typed.length >= 2
  // Offered only once the search has actually come back empty. Offering it beside real matches
  // would make "register a new one" as easy as reusing the right one, which is the duplication
  // the restriction exists to stop.
  const offerRegister = canRegister && !busy && results.length === 0

  const register = async () => {
    setRegisterError(null)
    setRegistering(true)
    try {
      const term = await api.createArtifactType(
        isIri ? { iri: typed, label: tailName(typed) } : { label: typed },
      )
      add(term.iri, term.label)
    } catch (e) {
      setRegisterError(e instanceof Error ? e.message : 'could not register that type')
    } finally {
      setRegistering(false)
    }
  }

  return (
    <div className="field" ref={box}>
      <label htmlFor={id}>{label}</label>

      {value.length > 0 && (
        <ul className="chips" style={{ marginBottom: 7 }}>
          {value.map((iri) => (
            <li key={iri}>
              <span className="chip accent" title={iri}>
                {labels[iri] ?? iri.split(/[#/]/).filter(Boolean).pop()}
                <button
                  type="button"
                  className="chip-x"
                  aria-label={`Remove ${labels[iri] ?? iri}`}
                  onClick={() => onChange(value.filter((v) => v !== iri))}
                >
                  ×
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}

      <div style={{ position: 'relative' }}>
        <input
          id={id}
          value={query}
          autoComplete="off"
          placeholder={placeholder ?? 'Type to search…'}
          onChange={(e) => {
            setQuery(e.target.value)
            setRegisterError(null)
            setOpen(true)
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') {
              e.preventDefault()
              setActive((a) => Math.min(a + 1, results.length - 1))
            } else if (e.key === 'ArrowUp') {
              e.preventDefault()
              setActive((a) => Math.max(a - 1, 0))
            } else if (e.key === 'Enter') {
              e.preventDefault()
              if (results[active]) add(results[active].iri, results[active].label)
              else if (offerRegister) void register()
              else if (isIri && !canRegister) add(typed)
            } else if (e.key === 'Escape') {
              setOpen(false)
            }
          }}
          role="combobox"
          aria-expanded={open && (results.length > 0 || offerRegister || (isIri && !canRegister))}
          aria-controls={`${id}-listbox`}
        />
        {open && (results.length > 0 || offerRegister || (isIri && !canRegister)) && (
          <ul className="picker" id={`${id}-listbox`} role="listbox">
            {results.map((t, i) => (
              <li
                key={t.iri}
                role="option"
                aria-selected={i === active}
                className={i === active ? 'active' : undefined}
                onMouseEnter={() => setActive(i)}
                onMouseDown={(e) => {
                  e.preventDefault()
                  add(t.iri, t.label)
                }}
              >
                <div className="picker-label">
                  {t.label}
                  {/* Which vocabulary, derived from the term's own IRI — the API field says
                      only where a term stands relative to this registry, deliberately, so that
                      no vocabulary is named in a field that other vocabularies will share. */}
                  {(vocabularyOf(t.iri) ?? t.source) && (
                    <span className="chip plain">{vocabularyOf(t.iri) ?? t.source}</span>
                  )}
                </div>
                {t.definition && <div className="picker-def">{t.definition}</div>}
              </li>
            ))}
            {offerRegister && (
              <li
                role="option"
                aria-selected={false}
                onMouseDown={(e) => {
                  e.preventDefault()
                  void register()
                }}
              >
                <div className="picker-label">
                  {registering
                    ? 'Registering…'
                    : isIri
                      ? 'Adopt this IRI as a type'
                      : `Add “${typed}” as a type`}
                </div>
                <div className="picker-def">
                  {isIri
                    ? 'Recorded under the identifier it already has, so this registry and any other that adopts it agree on one IRI. You can give it a better name afterwards.'
                    : 'Nothing here matches. This records it as a type of this registry’s own, which is the right answer when nothing anywhere names it.'}
                </div>
              </li>
            )}
            {isIri && !canRegister && (
              <li
                role="option"
                aria-selected={false}
                onMouseDown={(e) => {
                  e.preventDefault()
                  add(typed)
                }}
              >
                <div className="picker-label">Use this IRI directly</div>
                <div className="picker-def mono">{typed}</div>
              </li>
            )}
          </ul>
        )}
      </div>
      <p className="hint">
        {hint ?? 'Search by name.'} {busy && <span className="muted">searching…</span>}
      </p>
      {registerError && (
        <p className="field-error" role="alert">
          {registerError}
        </p>
      )}
    </div>
  )
}

/** A provisional name for an IRI being adopted: its last segment, punctuation opened out. It is
 *  the same fallback the chips already render, and better than blocking the adoption on a name
 *  the curator does not have to hand — the type page can rename it. */
function tailName(iri: string): string {
  const tail = iri.split(/[#/]/).filter(Boolean).pop() ?? iri
  return tail.replace(/[_-]+/g, ' ').trim() || iri
}
