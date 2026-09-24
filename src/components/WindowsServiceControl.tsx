import { useCallback, useEffect, useRef, useState } from 'react'
import { Download, Radio, RotateCcw, Trash2 } from 'lucide-react'
import { SettingsHelp } from './SettingsHelp'
import { serviceStateLabels } from '../windowsService'
import type { WindowsServiceAction, WindowsServiceStatus } from '../windowsService'

type WindowsServiceControlProps = {
  nativeRuntime: boolean
  disabled: boolean
  onBusyChange: (busy: boolean) => void
  onQuery: () => Promise<WindowsServiceStatus>
  onAction: (action: WindowsServiceAction) => Promise<WindowsServiceStatus>
}

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
    if (active.current || disabled || !nativeRuntime || !status) return
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
        setMessage(action === 'install' ? '服务已安装并启动，已设为开机自动启动。' : '服务已停止并卸载。')
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

  const stateLabel = !nativeRuntime
    ? '仅桌面版可检测'
    : status ? serviceStateLabels[status.state] : checking ? '检测中…' : '无法获取状态'
  const rpcConnected = status?.rpc?.connected === true
  const rpcInfo = rpcConnected ? status?.rpc?.info : null
  const rpcLabel = !nativeRuntime ? '仅桌面版可检测'
    : !status ? checking ? '检测中…' : '未检查'
    : rpcConnected ? '已响应' : '未响应'
  const rpcError = status && !rpcConnected
    ? `AxonkeyService RPC 未连接。${status.rpc?.error ? ` ${status.rpc.error}` : ''}`
    : ''
  const serviceRunning = status?.state === 'running'
  const action: WindowsServiceAction = serviceRunning ? 'uninstall' : 'install'
  const actionLabel = serviceRunning ? '卸载' : '安装'
  const pendingLabel = serviceRunning ? '正在停止并卸载…' : '正在安装并启动…'
  const ActionIcon = serviceRunning ? Trash2 : Download
  const actionDisabled = !nativeRuntime || disabled || pending !== null || !status

  return <section className={`driver-setup-item setup-service-item ${status?.state ?? 'unknown'}`} aria-labelledby="setup-service-title">
    <div className="driver-setup-heading">
      <span className="driver-setup-icon"><Radio size={18} /></span>
      <div className="setup-service-heading-copy">
        <div className="setup-service-title">
          <h3 id="setup-service-title">AxonkeyService 服务</h3>
        </div>
        <p>通过 RPC 连接确认服务是否运行</p>
      </div>
      <span className="driver-status-chip"><span className="setup-status-dot" /> {stateLabel}</span>
    </div>
    <div className="driver-suite-status" aria-live="polite">
      <span><Radio size={15} /> RPC 连接：{rpcLabel}</span>
      {rpcInfo && <span>{rpcInfo.name} v{rpcInfo.version} · {rpcInfo.protocolVersion} · {rpcInfo.pipeName}</span>}
    </div>
    {error
      ? <p className="driver-setup-message setup-service-message error" role="alert">{error}</p>
      : <p className={`driver-setup-message setup-service-message ${!pending && rpcError ? 'error' : ''}`} role="status">{pending ? '正在请求管理员权限，请在 Windows 授权窗口中允许。' : rpcError || message}</p>}
    <div className="driver-setup-actions">
      <button type="button" className={`dialog-secondary ${serviceRunning ? 'danger' : ''}`} aria-label={`${actionLabel}服务`} disabled={actionDisabled} onClick={() => void run(action)}><ActionIcon size={14} /> {pending === action ? pendingLabel : actionLabel}</button>
      <button type="button" className="dialog-secondary" aria-label="刷新服务状态" disabled={!nativeRuntime || checking || pending !== null} onClick={() => void refresh()}><RotateCcw size={14} /> {checking ? '检测中…' : '重新检测'}</button>
    </div>
  </section>
}
