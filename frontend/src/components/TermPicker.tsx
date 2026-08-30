import { useEffect, useRef, useState } from 'react'

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
 * This searches the registry's bundled vocabulary (EDAM plus any locally-minted types) and
 * shows what was chosen as removable chips.
 *
 * It keeps a free-IRI escape hatch, deliberately: `ArtifactType` is any IRI (spec D11), so a
 * picker that only offered known terms would make the model narrower than it is. Anything that
 * looks like an absolute IRI can be added directly.
 */
export function TermPicker({
  value, onChange, branch, label, hint, placeholder, id,
}: {
  value: string[]
  onChange: (next: string[]) => void
  /** `topic` or `data`; omit to search everything. */
  branch?: 'topic' | 'data'
  label: string
  hint?: string
  placeholder?: string
  id: string
}) {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<Term[]>([])
  const [labels, setLabels] = useState<Record<string, string>>({})
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const [busy, setBusy] = useState(false)
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

  const isIri = /^https?:\/\/\S+$/i.test(query.trim())

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
              else if (isIri) add(query.trim())
            } else if (e.key === 'Escape') {
              setOpen(false)
            }
          }}
          role="combobox"
          aria-expanded={open && (results.length > 0 || isIri)}
          aria-controls={`${id}-listbox`}
        />
        {open && (results.length > 0 || isIri) && (
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
                  {t.source && <span className="chip plain">{t.source}</span>}
                </div>
                {t.definition && <div className="picker-def">{t.definition}</div>}
              </li>
            ))}
            {isIri && (
              <li
                role="option"
                aria-selected={false}
                onMouseDown={(e) => {
                  e.preventDefault()
                  add(query.trim())
                }}
              >
                <div className="picker-label">Use this IRI directly</div>
                <div className="picker-def mono">{query.trim()}</div>
              </li>
            )}
          </ul>
        )}
      </div>
      <p className="hint">
        {hint ?? 'Search by name.'} {busy && <span className="muted">searching…</span>}
      </p>
    </div>
  )
}
