import { useCallback, useEffect, useMemo, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'

interface McpStatus {
  codex_found: boolean
  codex_path: string
  registered: boolean
  current: boolean
  registered_path: string
  version: string
  enabled: boolean
}

interface McpPermission {
  profile_id: string
  name: string
  host: string
  port: number
  username: string
  permission_level: 'none' | 'read' | 'manage' | 'system'
}

interface McpAuditEntry {
  id: number
  created_at: string
  profile_id: string
  method: string
  target: string
  success: boolean
  message: string
}

const copy = {
  zh: {
    title: 'MCP / AI 集成', registration: 'MCP 注册', permissions: '服务器权限', audit: '最近调用记录',
    loading: '正在检测…', registered: '已注册并启用', registeredDisabled: '已注册，等待用户启用', outdated: '注册路径需要更新', unregistered: '尚未注册',
    codexMissing: '未找到 ChatGPT / Codex CLI。安装后重新检测。', register: '注册 MCP', reregister: '重新注册',
    unregister: '注销并撤销权限', refresh: '刷新状态', path: '注册路径', codex: 'Codex 路径', version: 'MCP 版本',
    level: '权限级别', none: '未授权', read: '只读', manage: '管理', system: '系统管理', rootWarning: 'root 的写操作需要系统管理权限。',
    noServers: '尚未保存服务器配置。', noAudit: '暂无 MCP 调用记录。', success: '成功', failed: '失败',
    permissionHint: '每台服务器只选择一个级别：管理包含只读，系统管理包含全部权限。授权后不再逐次确认，撤销后立即生效。',
    restartHint: '注册变更后请重启 ChatGPT / Codex。', error: '操作失败',
  },
  en: {
    title: 'MCP / AI Integration', registration: 'MCP Registration', permissions: 'Server Permissions', audit: 'Recent Calls',
    loading: 'Checking…', registered: 'Registered and enabled', registeredDisabled: 'Registered, waiting for user enablement', outdated: 'Registration path needs updating', unregistered: 'Not registered',
    codexMissing: 'ChatGPT / Codex CLI was not found. Install it and check again.', register: 'Register MCP', reregister: 'Re-register',
    unregister: 'Unregister and revoke access', refresh: 'Refresh', path: 'Registered path', codex: 'Codex path', version: 'MCP version',
    level: 'Access level', none: 'No access', read: 'Read only', manage: 'Manage', system: 'System management', rootWarning: 'Write operations through root require system management access.',
    noServers: 'No server profiles are saved.', noAudit: 'No MCP calls recorded.', success: 'Success', failed: 'Failed',
    permissionHint: 'Choose one level per server: management includes read access, and system management includes all access. Revocation takes effect immediately.',
    restartHint: 'Restart ChatGPT / Codex after registration changes.', error: 'Operation failed',
  },
}

export default function McpPanel() {
  const { i18n } = useTranslation()
  const text = useMemo(() => i18n.resolvedLanguage?.startsWith('zh') ? copy.zh : copy.en, [i18n.resolvedLanguage])
  const [status, setStatus] = useState<McpStatus | null>(null)
  const [permissions, setPermissions] = useState<McpPermission[]>([])
  const [audit, setAudit] = useState<McpAuditEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const load = useCallback(async () => {
    setLoading(true)
    setError('')
    try {
      const [nextStatus, nextPermissions, nextAudit] = await Promise.all([
        invoke<McpStatus>('mcp_get_status'),
        invoke<McpPermission[]>('mcp_list_permissions'),
        invoke<McpAuditEntry[]>('mcp_list_audit'),
      ])
      setStatus(nextStatus)
      setPermissions(nextPermissions)
      setAudit(nextAudit)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => { load() }, [load])

  const runRegistration = async (command: 'mcp_register' | 'mcp_unregister') => {
    setBusy(true)
    setError('')
    try {
      await invoke(command)
      await load()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  const updatePermission = async (permission: McpPermission, permissionLevel: McpPermission['permission_level']) => {
    const next = { ...permission, permission_level: permissionLevel }
    setPermissions(current => current.map(item => item.profile_id === next.profile_id ? next : item))
    try {
      await invoke('mcp_set_server_permission', {
        profileId: next.profile_id,
        permissionLevel: next.permission_level,
      })
    } catch (e) {
      setError(String(e))
      await load()
    }
  }

  const statusLabel = !status?.codex_found ? text.codexMissing
    : status.current && status.enabled ? text.registered
      : status.current ? text.registeredDisabled
        : status.registered ? text.outdated : text.unregistered

  return (
    <div className="settings-panel mcp-panel">
      <h2 className="settings-panel-title">{text.title}</h2>
      {error && <div className="settings-error">{text.error}: {error}</div>}

      <div className="settings-card">
        <div className="settings-card-header">{text.registration}</div>
        <div className="settings-card-body">
          <div className="settings-row">
            <span className="settings-label">{loading ? text.loading : statusLabel}</span>
            <span className={`mcp-status-dot ${status?.current && status.enabled ? 'active' : ''}`} />
          </div>
          {status?.codex_path && <div className="settings-row"><span className="settings-label">{text.codex}</span><code>{status.codex_path}</code></div>}
          {status?.registered_path && <div className="settings-row"><span className="settings-label">{text.path}</span><code>{status.registered_path}</code></div>}
          {status?.version && <div className="settings-row"><span className="settings-label">{text.version}</span><code>{status.version}</code></div>}
          <div className="settings-hint">{text.restartHint}</div>
          <div className="settings-btn-row">
            <button className="svc-cfg-btn" onClick={load} disabled={loading || busy}>{text.refresh}</button>
            {(!status?.current || !status.enabled) && (
              <button className="svc-cfg-btn primary" onClick={() => runRegistration('mcp_register')} disabled={busy || !status?.codex_found}>
                {status?.registered ? text.reregister : text.register}
              </button>
            )}
            {(status?.registered || status?.enabled) && (
              <button className="svc-cfg-btn danger" onClick={() => runRegistration('mcp_unregister')} disabled={busy}>{text.unregister}</button>
            )}
          </div>
        </div>
      </div>

      <div className="settings-card">
        <div className="settings-card-header">{text.permissions}</div>
        <div className="settings-card-body">
          <div className="settings-hint">{text.permissionHint}</div>
          {permissions.length === 0 && <div className="settings-muted">{text.noServers}</div>}
          {permissions.map(permission => (
            <div className="mcp-server-permission" key={permission.profile_id}>
              <div className="mcp-server-title">
                <strong>{permission.name}</strong>
                <span>{permission.username}@{permission.host}:{permission.port}</span>
                {permission.username === 'root' && <em>{text.rootWarning}</em>}
              </div>
              <div className="mcp-permission-level">
                <label htmlFor={`mcp-level-${permission.profile_id}`}>{text.level}</label>
                <select
                  id={`mcp-level-${permission.profile_id}`}
                  value={permission.permission_level}
                  onChange={event => updatePermission(permission, event.target.value as McpPermission['permission_level'])}
                  disabled={!status?.current || !status.enabled}
                >
                  <option value="none">{text.none}</option>
                  <option value="read">{text.read}</option>
                  <option value="manage">{text.manage}</option>
                  <option value="system">{text.system}</option>
                </select>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="settings-card">
        <div className="settings-card-header">{text.audit}</div>
        <div className="settings-card-body mcp-audit-list">
          {audit.length === 0 && <div className="settings-muted">{text.noAudit}</div>}
          {audit.map(entry => (
            <div className="mcp-audit-row" key={entry.id}>
              <span>{entry.created_at}</span>
              <code>{entry.method}</code>
              <span>{entry.target || entry.profile_id || '—'}</span>
              <strong className={entry.success ? 'success' : 'failed'}>{entry.success ? text.success : text.failed}</strong>
              {!entry.success && <small title={entry.message}>{entry.message}</small>}
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
