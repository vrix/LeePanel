import { useMemo, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'

interface MarketplaceImage {
  name: string
  description: string
  stars: number
  official: boolean
  automated: boolean
  source: string
  category?: string
}

interface ImageTagDetail {
  tag: string
  digest: string
  architectures: string[]
  size: number
  updated: string
}

interface RegistryConfig { address: string; username: string }

const CURATED_DATA: [string, string, string][] = [
  ['nginx', '高性能 Web 与反向代理服务器', 'web'], ['httpd', 'Apache HTTP Server', 'web'],
  ['caddy', '自动 HTTPS Web 服务器', 'web'], ['traefik', '云原生反向代理与网关', 'web'],
  ['haproxy', '高性能负载均衡器', 'web'], ['tomcat', 'Java Web 应用服务器', 'web'],
  ['mysql', '流行的关系型数据库', 'database'], ['mariadb', 'MySQL 兼容数据库', 'database'],
  ['postgres', '先进的开源关系型数据库', 'database'], ['mongo', '文档数据库', 'database'],
  ['redis', '内存数据库与缓存', 'database'], ['memcached', '分布式内存缓存', 'database'],
  ['influxdb', '时序数据库', 'database'], ['clickhouse/clickhouse-server', '列式分析数据库', 'database'],
  ['node', 'Node.js 运行环境', 'runtime'], ['python', 'Python 运行环境', 'runtime'],
  ['php', 'PHP 运行环境', 'runtime'], ['golang', 'Go 构建与运行环境', 'runtime'],
  ['eclipse-temurin', 'OpenJDK Java 运行环境', 'runtime'], ['rust', 'Rust 构建环境', 'runtime'],
  ['ruby', 'Ruby 运行环境', 'runtime'], ['gcc', 'GNU 编译工具链', 'runtime'],
  ['grafana/grafana', '可观测性仪表盘', 'monitoring'], ['prom/prometheus', '指标监控与告警', 'monitoring'],
  ['prom/node-exporter', 'Linux 主机指标采集器', 'monitoring'], ['grafana/loki', '日志聚合系统', 'monitoring'],
  ['elastic/elasticsearch', '搜索与分析引擎', 'monitoring'], ['elastic/kibana', 'Elastic 数据可视化', 'monitoring'],
  ['jaegertracing/all-in-one', '分布式链路追踪', 'monitoring'], ['netdata/netdata', '实时系统监控', 'monitoring'],
  ['jenkins/jenkins', '持续集成与交付', 'devops'], ['gitea/gitea', '轻量 Git 服务', 'devops'],
  ['gitlab/gitlab-ce', 'GitLab 社区版', 'devops'], ['sonarqube', '代码质量分析平台', 'devops'],
  ['registry', 'OCI/Docker 私有镜像仓库', 'devops'], ['hashicorp/vault', '密钥与凭据管理', 'devops'],
  ['rabbitmq', '消息队列', 'service'], ['eclipse-mosquitto', 'MQTT 消息代理', 'service'],
  ['nats', '云原生消息系统', 'service'], ['apache/kafka', '分布式事件流平台', 'service'],
  ['minio/minio', 'S3 兼容对象存储', 'service'], ['nextcloud', '私有云文件服务', 'service'],
  ['wordpress', '内容管理系统', 'service'], ['adminer', '轻量数据库管理界面', 'service'],
  ['ubuntu', 'Ubuntu 基础系统', 'os'], ['debian', 'Debian 基础系统', 'os'],
  ['alpine', '轻量 Linux 基础镜像', 'os'], ['rockylinux/rockylinux', 'Rocky Linux 基础系统', 'os'],
  ['amazonlinux', 'Amazon Linux 基础系统', 'os'], ['busybox', '轻量 Unix 工具集', 'os'],
]
const CURATED: MarketplaceImage[] = CURATED_DATA.map(([name, description, category]) => ({
  name, description, category, stars: 0, official: !name.includes('/'), automated: false, source: 'Docker Hub',
}))
const PAGE_SIZE = 12

function bytes(value: number) {
  if (!value) return '-'
  const units = ['B', 'KB', 'MB', 'GB']
  let size = value; let unit = 0
  while (size >= 1024 && unit < units.length - 1) { size /= 1024; unit++ }
  return `${size.toFixed(unit ? 1 : 0)} ${units[unit]}`
}

export default function ImageMarketplace({ sessionId, mirrors, onPull }: { sessionId: string; mirrors: string[]; onPull: (image: string) => Promise<void> }) {
  const { t } = useTranslation()
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<MarketplaceImage[]>([])
  const [searched, setSearched] = useState(false)
  const [loading, setLoading] = useState(false)
  const [pulling, setPulling] = useState('')
  const [detail, setDetail] = useState<MarketplaceImage | null>(null)
  const [tags, setTags] = useState<ImageTagDetail[]>([])
  const [tagsLoading, setTagsLoading] = useState(false)
  const [registries, setRegistries] = useState<RegistryConfig[]>(() => {
    try { return JSON.parse(localStorage.getItem('leepanel.privateRegistries') || '[]') }
    catch { return [] }
  })
  const [source, setSource] = useState('')
  const [registryForm, setRegistryForm] = useState(false)
  const [registryAddress, setRegistryAddress] = useState('')
  const [registryUsername, setRegistryUsername] = useState('')
  const [registryPassword, setRegistryPassword] = useState('')
  const [registryBusy, setRegistryBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [category, setCategory] = useState('all')
  const [page, setPage] = useState(1)
  const activeRegistry = registries.find(r => r.address === source)

  const filtered = useMemo(() => searched ? results : (category === 'all' ? CURATED : CURATED.filter(item => item.category === category)), [searched, results, category])
  const pageCount = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE))
  const visible = useMemo(() => filtered.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE), [filtered, page])

  const search = async () => {
    setLoading(true); setMessage('')
    try {
      const list = await invoke<MarketplaceImage[]>('server_docker_marketplace_search', {
        sessionId, query: query.trim(), registry: source,
        username: activeRegistry?.username || '', password: registryPassword,
      })
      setResults(list); setSearched(true); setPage(1)
    } catch (e) { setMessage(String(e)) }
    finally { setLoading(false) }
  }

  const showDetail = async (image: MarketplaceImage) => {
    setDetail(image); setTags([]); setTagsLoading(true); setMessage('')
    try {
      const list = await invoke<ImageTagDetail[]>('server_docker_marketplace_tags', {
        sessionId, image: image.name, registry: source,
        username: activeRegistry?.username || '', password: registryPassword,
      })
      setTags(list)
    } catch (e) { setMessage(String(e)) }
    finally { setTagsLoading(false) }
  }

  const pull = async (image: string) => {
    setPulling(image); setMessage('')
    try { await onPull(image) }
    catch (e) { setMessage(String(e)) }
    finally { setPulling('') }
  }

  const loginRegistry = async () => {
    setRegistryBusy(true); setMessage('')
    try {
      await invoke('server_docker_registry_login', {
        sessionId, registry: registryAddress.trim(), username: registryUsername.trim(), password: registryPassword,
      })
      const address = registryAddress.trim().replace(/^https?:\/\//, '').replace(/\/$/, '')
      const next = [...registries.filter(r => r.address !== address), { address, username: registryUsername.trim() }]
      setRegistries(next); localStorage.setItem('leepanel.privateRegistries', JSON.stringify(next))
      setSource(address); setRegistryAddress(''); setRegistryUsername(''); setRegistryForm(false)
      setMessage(t('dockerPanel.registryLoginSuccess', { defaultValue: '私有仓库已登录并保存（密码未保存在本机）' }))
    } catch (e) { setMessage(String(e)) }
    finally { setRegistryBusy(false) }
  }

  const removeRegistry = async () => {
    if (!source) return
    setRegistryBusy(true)
    try { await invoke('server_docker_registry_logout', { sessionId, registry: source }) } catch { /* remove local entry anyway */ }
    const next = registries.filter(r => r.address !== source)
    setRegistries(next); localStorage.setItem('leepanel.privateRegistries', JSON.stringify(next))
    setSource(''); setRegistryPassword(''); setSearched(false); setRegistryBusy(false)
  }

  return <div className="marketplace">
    <div className="marketplace-toolbar">
      <input className="docker-pull-input" value={query} onChange={e => setQuery(e.target.value)}
        onKeyDown={e => { if (e.key === 'Enter') search() }}
        placeholder={mirrors.length ? t('dockerPanel.marketExactPlaceholder', { defaultValue: '输入完整镜像名，如 nginx 或 grafana/grafana' }) : t('dockerPanel.marketSearchPlaceholder', { defaultValue: '搜索 nginx、数据库、监控工具…' })} />
      <select className="marketplace-select" value={source} onChange={e => { setSource(e.target.value); setSearched(false); setRegistryPassword(''); setPage(1) }}>
        <option value="">{mirrors.length ? `当前镜像源：${mirrors[0]}${mirrors.length > 1 ? ` +${mirrors.length - 1}` : ''}` : 'Docker Hub / Podman registries'}</option>
        {registries.map(r => <option value={r.address} key={r.address}>{r.address}</option>)}
      </select>
      <button className="docker-btn primary" onClick={search} disabled={loading}>{loading ? '...' : t('dockerPanel.search', { defaultValue: '搜索' })}</button>
      <button className="docker-btn" onClick={() => setRegistryForm(v => !v)}>{t('dockerPanel.privateRegistry', { defaultValue: '私有仓库' })}</button>
    </div>
    {!source && mirrors.length > 0 && <div className="marketplace-source-hint">
      {t('dockerPanel.mirrorSearchHint', { defaultValue: '镜像加速源不提供关键词目录；请输入完整仓库名。搜索、标签和拉取将使用当前镜像源。' })}
    </div>}

    {source && <div className="marketplace-private-hint">
      <span>{source} · {activeRegistry?.username}</span>
      <input type="password" className="docker-pull-input" value={registryPassword} onChange={e => setRegistryPassword(e.target.value)}
        placeholder={t('dockerPanel.registryPasswordSession', { defaultValue: '密码/Token（仅本次使用）' })} />
      <button className="docker-btn danger" onClick={removeRegistry} disabled={registryBusy}>{t('dockerPanel.remove', { defaultValue: '移除' })}</button>
    </div>}

    {registryForm && <div className="marketplace-registry-form">
      <input className="docker-pull-input" value={registryAddress} onChange={e => setRegistryAddress(e.target.value)} placeholder="registry.example.com:5000" />
      <input className="docker-pull-input" value={registryUsername} onChange={e => setRegistryUsername(e.target.value)} placeholder={t('dockerPanel.username', { defaultValue: '用户名' })} />
      <input type="password" className="docker-pull-input" value={registryPassword} onChange={e => setRegistryPassword(e.target.value)} placeholder={t('dockerPanel.passwordToken', { defaultValue: '密码 / Token' })} />
      <button className="docker-btn primary" onClick={loginRegistry} disabled={registryBusy || !registryAddress || !registryUsername || !registryPassword}>
        {registryBusy ? '...' : t('dockerPanel.loginAndSave', { defaultValue: '登录并保存' })}
      </button>
      <small>{t('dockerPanel.passwordNotSaved', { defaultValue: '密码不会写入 LeePanel 数据库或本机配置。' })}</small>
    </div>}

    {message && <div className="docker-message docker-error">{message}</div>}
    {!searched && <div className="marketplace-catalog-head">
      <div><div className="marketplace-section-title">{t('dockerPanel.curatedImages', { defaultValue: '推荐镜像目录' })}</div><small>{t('dockerPanel.curatedBasis', { defaultValue: '按常见部署场景人工整理，并非实时热度排名' })}</small></div>
      <div className="marketplace-categories">
        {[
          ['all', '全部'], ['web', 'Web'], ['database', '数据库'], ['runtime', '开发环境'],
          ['monitoring', '监控'], ['devops', 'DevOps'], ['service', '应用服务'], ['os', '基础系统'],
        ].map(([key, label]) => <button key={key} className={`marketplace-category${category === key ? ' active' : ''}`} onClick={() => { setCategory(key); setPage(1) }}>{label}</button>)}
      </div>
    </div>}
    {searched && !loading && visible.length === 0 && <div className="docker-empty">{t('dockerPanel.noSearchResults', { defaultValue: '没有找到镜像' })}</div>}
    <div className="marketplace-grid">
      {visible.map(image => <div className="marketplace-card" key={`${image.source}/${image.name}`}>
        <div className="marketplace-card-head"><strong>{image.name}</strong>{image.official && <span className="marketplace-badge">Official</span>}</div>
        <p>{image.description || t('dockerPanel.noDescription', { defaultValue: '暂无描述' })}</p>
        <div className="marketplace-card-meta"><span>{image.source}</span>{image.stars > 0 && <span>★ {image.stars.toLocaleString()}</span>}</div>
        <div className="marketplace-card-actions">
          <button className="docker-btn" onClick={() => showDetail(image)}>{t('dockerPanel.detailsAndTags', { defaultValue: '详情与标签' })}</button>
          <button className="docker-btn primary" onClick={() => pull(`${image.name}:latest`)} disabled={!!pulling}>
            {pulling === `${image.name}:latest` ? '...' : t('dockerPanel.pullLatest', { defaultValue: '拉取 latest' })}
          </button>
        </div>
      </div>)}
    </div>
    {filtered.length > PAGE_SIZE && <div className="marketplace-pagination">
      <button className="docker-btn" disabled={page <= 1} onClick={() => setPage(p => p - 1)}>‹ {t('dockerPanel.previousPage', { defaultValue: '上一页' })}</button>
      <span>{t('dockerPanel.pageStatus', { page, count: pageCount, total: filtered.length, defaultValue: `第 ${page} / ${pageCount} 页，共 ${filtered.length} 个` })}</span>
      <button className="docker-btn" disabled={page >= pageCount} onClick={() => setPage(p => p + 1)}>{t('dockerPanel.nextPage', { defaultValue: '下一页' })} ›</button>
    </div>}

    {detail && <div className="docker-modal-overlay" onClick={() => setDetail(null)}>
      <div className="docker-modal marketplace-detail" onClick={e => e.stopPropagation()}>
        <div className="docker-modal-header"><span className="docker-modal-title">{detail.name}</span><button className="docker-modal-close" onClick={() => setDetail(null)}>×</button></div>
        <div className="docker-modal-body">
          <p className="marketplace-detail-desc">{detail.description}</p>
          {tagsLoading ? <div className="docker-loading">{t('dockerPanel.loadingTags', { defaultValue: '加载标签和架构信息…' })}</div> :
            <div className="marketplace-tags">
              {tags.map(tag => <div className="marketplace-tag-row" key={tag.tag}>
                <div><strong>{tag.tag}</strong><small>{tag.architectures.join(' / ') || '-'} · {bytes(tag.size)}</small>{tag.digest && <code title={tag.digest}>{tag.digest.slice(0, 26)}…</code>}</div>
                <button className="docker-btn primary" onClick={() => pull(`${detail.name}:${tag.tag}`)} disabled={!!pulling}>{pulling === `${detail.name}:${tag.tag}` ? '...' : t('dockerPanel.pullImage')}</button>
              </div>)}
              {!tags.length && <div className="docker-empty">{t('dockerPanel.noTags', { defaultValue: '没有可显示的标签' })}</div>}
            </div>}
        </div>
      </div>
    </div>}
  </div>
}
