import { useMemo } from 'react'
import DOMPurify from 'dompurify'
import { marked } from 'marked'

/**
 * Renders a README the way a forge does: full Markdown, images included.
 *
 * Two things have to be right here.
 *
 * **Safety.** README content is written by whoever curates the record, and it reaches the
 * browser as HTML. It goes through DOMPurify, so a `<script>` or an `onerror=` in someone's
 * README cannot execute against a reader's session. Nothing is rendered that DOMPurify has
 * not seen.
 *
 * **Relative paths.** A README written for a repository says `![](docs/images/x.png)`, which
 * means nothing once the text is served from somewhere else. `baseUrl` — normally the
 * repository's raw content root — is what those resolve against; without it a record shows a
 * page of broken images, which is worse than showing none.
 */
export function Markdown({ source, baseUrl }: { source: string; baseUrl?: string }) {
  const html = useMemo(() => {
    const raw = marked.parse(source, { async: false, gfm: true, breaks: false }) as string
    const clean = DOMPurify.sanitize(raw, {
      USE_PROFILES: { html: true },
      // Anchors open elsewhere; the attributes are added after sanitising, below.
      ADD_ATTR: ['target', 'rel', 'loading'],
    })
    return resolve(clean, baseUrl)
  }, [source, baseUrl])

  return <div className="markdown" dangerouslySetInnerHTML={{ __html: html }} />
}

/** Rewrite relative `src`/`href` against the base, and make links safe to click. */
function resolve(html: string, baseUrl?: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html')

  const absolute = (value: string): string | null => {
    if (/^(https?:|mailto:|data:image\/)/i.test(value)) return value
    // Anything else absolute-looking is a scheme we will not follow (javascript:, file:, …).
    if (/^[a-z][a-z0-9+.-]*:/i.test(value)) return null
    if (value.startsWith('#')) return value
    if (!baseUrl) return null
    try {
      return new URL(value, baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`).toString()
    } catch {
      return null
    }
  }

  doc.querySelectorAll('img').forEach((img) => {
    const src = absolute(img.getAttribute('src') ?? '')
    if (!src) {
      img.remove()
      return
    }
    img.setAttribute('src', src)
    img.setAttribute('loading', 'lazy')
  })

  doc.querySelectorAll('a').forEach((a) => {
    const href = absolute(a.getAttribute('href') ?? '')
    if (!href) {
      // Keep the text, drop the unusable link, rather than silently deleting content.
      a.replaceWith(...Array.from(a.childNodes))
      return
    }
    a.setAttribute('href', href)
    if (!href.startsWith('#')) {
      a.setAttribute('target', '_blank')
      a.setAttribute('rel', 'noreferrer noopener')
    }
  })

  return doc.body.innerHTML
}
