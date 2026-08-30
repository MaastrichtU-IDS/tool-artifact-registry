import { useCallback, useEffect, useState } from 'react'

export interface AsyncState<T> {
  data?: T
  // Typed as Error rather than unknown so `{error && <ErrorState …/>}` type-checks in JSX;
  // ApiError extends Error, so the RFC 9457 payload still survives the narrowing.
  error?: Error
  loading: boolean
  reload: () => void
}

/** Load once per dependency change, with an explicit retry for idempotent reads. */
export function useAsync<T>(fn: () => Promise<T>, deps: unknown[]): AsyncState<T> {
  const [data, setData] = useState<T>()
  const [error, setError] = useState<Error>()
  const [loading, setLoading] = useState(true)
  const [nonce, setNonce] = useState(0)

  // The caller's closure changes every render; deps decide when we actually refetch.
  const run = useCallback(fn, deps) // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(undefined)
    run()
      .then((d) => {
        if (!cancelled) setData(d)
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e : new Error(String(e)))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [run, nonce])

  return { data, error, loading, reload: () => setNonce((n) => n + 1) }
}
