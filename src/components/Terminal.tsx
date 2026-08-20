import { useEffect, useRef, useImperativeHandle, forwardRef } from 'react'
import { Terminal as XTerminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { ClipboardAddon } from '@xterm/addon-clipboard'
import '@xterm/xterm/css/xterm.css'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { shouldBlockMouseMode } from './terminalMouseModes'
import { shouldCopyTerminalSelection } from './terminalKeyboard'

interface TerminalProps {
  sessionId: string | null
  isActive?: boolean
  theme?: string
}

export interface TerminalHandle {
  sendCommand: (cmd: string) => void
  clear: () => void
}

// GitHub Dark palette (matches the app's default theme)
const XTERM_DARK_THEME = {
  background: '#0d1117',
  foreground: '#c9d1d9',
  cursor: '#58a6ff',
  selectionBackground: '#264f78',
  black: '#0d1117',
  red: '#ff7b72',
  green: '#3fb950',
  yellow: '#d29922',
  blue: '#58a6ff',
  magenta: '#bc8cff',
  cyan: '#39c5cf',
  white: '#c9d1d9',
  brightBlack: '#484f58',
  brightRed: '#ffa198',
  brightGreen: '#56d364',
  brightYellow: '#e3b341',
  brightBlue: '#79c0ff',
  brightMagenta: '#d2a8ff',
  brightCyan: '#56d4dd',
  brightWhite: '#f0f6fc',
}

// GitHub Light palette (used when theme = light)
const XTERM_LIGHT_THEME = {
  background: '#ffffff',
  foreground: '#1f2328',
  cursor: '#0969da',
  selectionBackground: '#c8daf5',
  black: '#1f2328',
  red: '#cf222e',
  green: '#1a7f37',
  yellow: '#9a6700',
  blue: '#0969da',
  magenta: '#8250df',
  cyan: '#1b7c83',
  white: '#6e7781',
  brightBlack: '#6e7781',
  brightRed: '#cf222e',
  brightGreen: '#1a7f37',
  brightYellow: '#9a6700',
  brightBlue: '#0969da',
  brightMagenta: '#8250df',
  brightCyan: '#1b7c83',
  brightWhite: '#ffffff',
}

export default forwardRef<TerminalHandle, TerminalProps>(function Terminal({ sessionId, isActive, theme = 'dark' }, ref) {
  const containerRef = useRef<HTMLDivElement>(null)
  const termRef = useRef<XTerminal | null>(null)
  const fitRef = useRef<FitAddon | null>(null)
  const sidRef = useRef(sessionId)

  useEffect(() => {
    sidRef.current = sessionId
  }, [sessionId])

  // Initialize terminal
  useEffect(() => {
    if (!containerRef.current) return

    const term = new XTerminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: "'Menlo', 'Monaco', 'Liberation Mono', 'DejaVu Sans Mono', 'Courier New', monospace",
      allowProposedApi: true,
      theme: theme === 'light' ? XTERM_LIGHT_THEME : XTERM_DARK_THEME,
      allowTransparency: true,
    })

    const fitAddon = new FitAddon()
    const webLinksAddon = new WebLinksAddon()
    term.loadAddon(fitAddon)
    term.loadAddon(webLinksAddon)
    term.open(containerRef.current)
    // Block DECSET mouse-tracking sequences so local text selection works; allow DECRST cleanup.
    // Remote shells (bash/tmux/vim) send \e[?1000h etc. which capture mouse events
    for (const final of ['h', 'l']) {
      term.parser.registerCsiHandler({ final, prefix: '?' }, (params) => {
        return shouldBlockMouseMode(final, params)
      })
    }

    const clipboardAddon = new ClipboardAddon()
    term.loadAddon(clipboardAddon)

    // Handle copy before xterm turns Ctrl+C into terminal input and clears the selection.
    term.attachCustomKeyEventHandler((event) => {
      if (shouldCopyTerminalSelection(event, term.hasSelection())) {
        navigator.clipboard.writeText(term.getSelection()).catch(() => {})
        return false
      }
      return true
    })

    // ponytail: sync remote PTY size with xterm.js after every fit
    const syncSize = () => {
      const sid = sidRef.current
      if (sid) {
        invoke('ssh_resize', { sessionId: sid, cols: term.cols, rows: term.rows })
      }
    }
    setTimeout(() => { fitAddon.fit(); syncSize() }, 100)

    term.onData((data) => {
      const sid = sidRef.current
      if (sid) {
        invoke('ssh_input', { sessionId: sid, data })
      }
    })

    termRef.current = term
    fitRef.current = fitAddon

    // Expose sendCommand via ref
    // (done in separate useEffect below)

    // Listen for SSH output
    const unlisten = listen<{ sessionId: string; data: string }>('ssh-output', (event) => {
      const sid = sidRef.current
      if (sid && event.payload.sessionId === sid) {
        term.write(event.payload.data)
      }
    })

    // Handle resize
    const handleResize = () => {
      if (fitRef.current) {
        fitRef.current.fit()
        const sid = sidRef.current
        if (sid) {
          invoke('ssh_resize', {
            sessionId: sid,
            cols: term.cols,
            rows: term.rows,
          })
        }
      }
    }
    window.addEventListener('resize', handleResize)

    // Listen for connection closed
    const unlistenClosed = listen<string>('ssh-closed', (event) => {
      const sid = sidRef.current
      if (sid && event.payload === sid) {
        term.clear()
      }
    })

    return () => {
      unlisten.then((fn) => fn())
      unlistenClosed.then((fn) => fn())
      window.removeEventListener('resize', handleResize)
      term.dispose()
      termRef.current = null
      fitRef.current = null
    }
  }, [])

  useImperativeHandle(ref, () => ({
    sendCommand: (cmd: string) => {
      const sid = sidRef.current
      if (sid && termRef.current) {
        invoke('ssh_input', { sessionId: sid, data: cmd + '\r' })
      }
    },
    clear: () => {
      termRef.current?.clear()
    },
  }))

  // ponytail: swap xterm palette live when the app theme changes
  useEffect(() => {
    if (termRef.current) {
      termRef.current.options.theme = theme === 'light' ? XTERM_LIGHT_THEME : XTERM_DARK_THEME
    }
  }, [theme])

  // Refit on session change + sync PTY
  useEffect(() => {
    if (fitRef.current && termRef.current) {
      setTimeout(() => {
        fitRef.current?.fit()
        if (sessionId) {
          invoke('ssh_resize', { sessionId, cols: termRef.current!.cols, rows: termRef.current!.rows })
        }
      }, 100)
    }
  }, [sessionId])

  // Refit when tab becomes active (was hidden with display:none) + sync PTY
  useEffect(() => {
    if (isActive && fitRef.current && termRef.current) {
      setTimeout(() => {
        fitRef.current?.fit()
        const sid = sidRef.current
        if (sid) {
          invoke('ssh_resize', { sessionId: sid, cols: termRef.current!.cols, rows: termRef.current!.rows })
        }
      }, 50)
    }
  }, [isActive])

  return (
    <div
      ref={containerRef}
      style={{ width: '100%', height: '100%', background: 'var(--bg)' }}
    />
  )
})
