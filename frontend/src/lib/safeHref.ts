/**
 * The only way a URL that came from data — a record field, a peer's search hit, a SPARQL
 * result — becomes a link. http(s) and mailto pass; anything else with a scheme
 * (`javascript:`, `data:`, `vbscript:`, `file:`) becomes no link at all, so the text still shows
 * but clicking it cannot run code on this page, which holds the signed-in user's token.
 *
 * Records written here are checked by the server as well, but a peer's records never pass
 * through this registry's rules, so the check has to live where the link is drawn.
 */
export function safeHref(url: string | null | undefined): string | undefined {
  if (!url) return undefined
  const v = url.trim()
  if (/^(https?:|mailto:)/i.test(v)) return v
  // Relative paths inside this app are fine; anything else with a scheme is refused.
  if (v.startsWith('/') && !v.startsWith('//')) return v
  return undefined
}
