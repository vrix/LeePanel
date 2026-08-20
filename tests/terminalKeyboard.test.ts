import { describe, expect, it } from 'vitest'
import { shouldCopyTerminalSelection } from '../src/components/terminalKeyboard'

const keyEvent = (key: string, ctrlKey = false, metaKey = false, type = 'keydown') => ({
  type,
  key,
  ctrlKey,
  metaKey,
})

describe('terminal copy shortcuts', () => {
  it('copies a selection when Enter is pressed', () => {
    expect(shouldCopyTerminalSelection(keyEvent('Enter'), true)).toBe(true)
  })

  it('passes Enter through when there is no selection', () => {
    expect(shouldCopyTerminalSelection(keyEvent('Enter'), false)).toBe(false)
  })

  it('copies a selection with Ctrl+C or Command+C', () => {
    expect(shouldCopyTerminalSelection(keyEvent('c', true), true)).toBe(true)
    expect(shouldCopyTerminalSelection(keyEvent('c', false, true), true)).toBe(true)
  })

  it('does not copy on keyup', () => {
    expect(shouldCopyTerminalSelection(keyEvent('Enter', false, false, 'keyup'), true)).toBe(false)
  })
})
