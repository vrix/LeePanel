const MOUSE_MODES = new Set([9, 1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1015])

export function shouldBlockMouseMode(final: string, params: (number | number[])[]): boolean {
  return final === 'h' && params.some(param =>
    Array.isArray(param) ? param.some(value => MOUSE_MODES.has(value)) : MOUSE_MODES.has(param)
  )
}
