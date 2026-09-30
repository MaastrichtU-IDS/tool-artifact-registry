import { describe, expect, it } from 'vitest'
import { safeHref } from './safeHref'

describe('safeHref', () => {
  it('keeps web and mail links', () => {
    expect(safeHref('https://example.org/x')).toBe('https://example.org/x')
    expect(safeHref('HTTP://example.org')).toBe('HTTP://example.org')
    expect(safeHref('mailto:a@example.org')).toBe('mailto:a@example.org')
    expect(safeHref('/software/1')).toBe('/software/1')
  })

  it('refuses every scheme that could run code, however it is spelt', () => {
    for (const bad of ['javascript:alert(1)', ' JavaScript:alert(1)', 'data:text/html,<script>', 'vbscript:x',
                       'file:///etc/passwd', '//evil.example/x', '']) {
      expect(safeHref(bad), bad).toBeUndefined()
    }
    expect(safeHref(undefined)).toBeUndefined()
  })
})
