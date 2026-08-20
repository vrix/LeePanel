type TerminalKeyEvent = Pick<KeyboardEvent, 'type' | 'key' | 'ctrlKey' | 'metaKey'>

export function shouldCopyTerminalSelection(event: TerminalKeyEvent, hasSelection: boolean): boolean {
  if (!hasSelection || event.type !== 'keydown') return false
  return event.key === 'Enter'
    || (event.key.toLowerCase() === 'c' && (event.ctrlKey || event.metaKey))
}
