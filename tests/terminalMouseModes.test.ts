import { describe, expect, it } from 'vitest'
import { shouldBlockMouseMode } from '../src/components/terminalMouseModes'

describe('terminal mouse mode filtering', () => {
  it('blocks mouse tracking even when it is not the first DECSET parameter', () => {
    expect(shouldBlockMouseMode('h', [1, 1000, 1006])).toBe(true)
  })

  it('allows DECRST so an active mouse mode can be disabled', () => {
    expect(shouldBlockMouseMode('l', [1000, 1006])).toBe(false)
  })

  it('does not block unrelated terminal modes', () => {
    expect(shouldBlockMouseMode('h', [1, 25, 2004])).toBe(false)
  })
})
