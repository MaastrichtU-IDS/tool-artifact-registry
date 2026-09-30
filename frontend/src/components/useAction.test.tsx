import { act, renderHook } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { useAction } from './common'

describe('useAction', () => {
  it('keeps a failed destructive action to show, instead of throwing it away', async () => {
    const { result } = renderHook(() => useAction())
    await act(() => result.current.run(async () => { throw new Error('revoke failed') }))
    expect((result.current.error as Error).message).toBe('revoke failed')
    expect(result.current.busy).toBe(false)
    await act(() => result.current.run(async () => {}))
    expect(result.current.error).toBeUndefined()
  })
})
