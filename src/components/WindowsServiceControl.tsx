import { useCallback, useEffect, useRef, useState } from 'react'
import { Download, Play, RotateCcw, Square, Trash2 } from 'lucide-react'
import { SettingsHelp } from './SettingsHelp'
import { canManageService, serviceStateLabels } from '../windowsService'
import type { WindowsServiceAction, WindowsServiceStatus } from '../windowsService'

type WindowsServiceControlProps = {
  nativeRuntime: boolean
  disabled: boolean
  onBusyChange: (busy: boolean) => void
  onQuery: () => Promise<WindowsServiceStatus>
  onAction: (action: WindowsServiceAction) => Promise<WindowsServiceStatus>
}

const actions = [
  { kind: 'install', label: '安装', pending: '正在安装…', Icon: Download },
  { kind: 'start', label: '启动', pending: '正在启动…', Icon: Play },
  { kind: 'stop', label: '停止', pending: '正在停止…', Icon: Square },
  { kind: 'uninstall', label: '卸载', pending: '正在卸载…', Icon: Trash2 },
] as const

export function WindowsServiceControl({ nativeRuntime, disabled, onBusyChange, onQuery, onAction }: WindowsServiceControlProps) {
  const [status, setStatus] = useState<WindowsServiceStatus>()
  const [checking, setChecking] = useState(false)
  const [pending, setPending] = useState<WindowsServiceAction | null>(null)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const mounted = useRef(false)
  const querying = useRef(false)
  const active = useRef(false)
  const refreshRevision = useRef(0)

  const refresh = useCallback(async () => {
    if (!nativeRuntime || querying.current || active.current) return
    querying.current = true
    const revision = ++refreshRevision.current
    setChecking(true)
    try {
      const next = await onQuery()
      if (mounted.current && revision === refreshRevision.current) {
        setStatus(next)
        setError('')
      }
    } catch (reason) {
      if (mounted.current && revision === refreshRevision.current) {
        setStatus(undefined)
        setError(reason instanceof Error ? reason.message : String(reason))
      }
    } finally {
      querying.current = false
      if (mounted.current) setChecking(false)
    }
  }, [nativeRuntime, onQuery])

  useEffect(() => {
    mounted.current = true
    void refresh()
    if (!nativeRuntime) return () => { mounted.current = false }
    const timer = window.setInterval(() => void refresh(), 3000)
    window.addEventListener('focus', refresh)
    return () => {
      mounted.current = false
      ++refreshRevision.current
      window.clearInterval(timer)
      window.removeEventListener('focus', refresh)
    }
  }, [nativeRuntime, refresh])

  const run = async (action: WindowsServiceAction) => {
    if (active.current || disabled || !nativeRuntime || !canManageService(status?.state, action)) return
    active.current = true
    ++refreshRevision.current
    setPending(action)
    setError('')
    setMessage('')
    onBusyChange(true)
    try {
      const next = await onAction(action)
      if (mounted.current) {
        setStatus(next)
        setMessage(action === 'install' ? '服务已安装并设为开机自动启动。现在可以点击“启动”。' : '操作已完成，服务状态已刷新。')
      }
    } catch (reason) {
      if (mounted.current) setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      active.current = false
      onBusyChange(false)
      if (mounted.current) {
        setPending(null)
        void refresh()
      }
    }
  }

  const rpcReady = status?.state === 'running' && status.rpc?.connected === true && status.rpc.info !== null
  const stateLabel = !nativeRuntime
    ? '仅桌面版可检测'
    : status?.state === 'running' && !rpcReady ? 'RPC 未就绪'
    : status ? serviceStateLabels[status.state] : checking ? '检测中…' : '无法获取状态'
  const rpcInfo = rpcReady ? status?.rpc.info : null
  const rpcLabel = !nativeRuntime ? '仅桌面版可检测'
    : !status ? checking ? '检测中…' : '未检查'
    : status.state !== 'running' ? '服务未运行'
    : rpcReady ? '已响应' : '未就绪'
  const rpcError = status?.state === 'running' && !rpcReady
    ? `GetServiceInfo 未通过，后台服务尚未就绪。${status.rpc?.error ? ` ${status.rpc.error}` : ''}`
    : ''

  return <section className="setup-service-panel" aria-labelledby="setup-service-title">
    <div className="setup-service-heading">
      <div className="setup-service-title">
        <h3 id="setup-service-title">AxonkeyService 后台服务</h3>
        <SettingsHelp id="setup-service-help" label="AxonkeyService 后台服务">服务负责 Windows 上的遥控器设备管理和语音接收。只有 GetServiceInfo 响应正常才视为运行中。安装、启动、停止和卸载均会通过 PowerShell 请求管理员授权。</SettingsHelp>
      </div>
      <button type="button" className="setup-service-refresh" aria-label="刷新服务状态" title="刷新服务状态" disabled={!nativeRuntime || checking || pending !== null} onClick={() => void refresh()}><RotateCcw size={15} /></button>
    </div>
    <div className="setup-service-status" aria-live="polite">
      <span className={`setup-service-badge ${status?.state === 'running' && !rpcReady ? 'rpcUnavailable' : status?.state ?? 'unknown'}`}><i aria-hidden="true" />{stateLabel}</span>
      {status?.state === 'running' && status.processId > 0 && <span>PID {status.processId}</span>}
    </div>
    <div className="setup-service-rpc" aria-live="polite">
      <span>GetServiceInfo：{rpcLabel}</span>
      {rpcInfo && <span>{rpcInfo.name} v{rpcInfo.version} · {rpcInfo.protocolVersion} · {rpcInfo.pipeName}</span>}
    </div>
    <div className="setup-service-actions">
      {actions.map(({ kind, label, pending: busyLabel, Icon }) => <button key={kind} type="button" className={`dialog-secondary ${kind === 'uninstall' ? 'danger' : ''}`} aria-label={`${label}服务`} disabled={!nativeRuntime || disabled || pending !== null || !canManageService(status?.state, kind)} onClick={() => void run(kind)}><Icon size={14} />{pending === kind ? busyLabel : label}</button>)}
    </div>
    {error && <p className="setup-service-feedback error" role="alert">{error}</p>}
    {!error && <p className={`setup-service-feedback ${!pending && rpcError ? 'error' : ''}`} role="status">{pending ? '正在请求管理员权限，请在 Windows 授权窗口中允许。' : rpcError || message || (status?.state === 'stopped' && status.exitCode !== 0 ? `服务已退出，系统错误码：${status.exitCode}。` : '')}</p>}
  </section>
}
