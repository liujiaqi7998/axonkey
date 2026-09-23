import { useCallback, useEffect, useRef, useState } from 'react'
import { Download, RotateCcw, Trash2 } from 'lucide-react'
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

  return <section className="setup-service-panel" aria-labelledby="setup-service-title">
    <div className="setup-service-heading">
      <div className="setup-service-title">
        <h3 id="setup-service-title">AxonkeyService 后台服务</h3>
        <SettingsHelp id="setup-service-help" label="AxonkeyService 后台服务">服务负责 Windows 上的遥控器设备管理和语音接收。按钮会根据 RPC 状态在安装和卸载之间切换。安装会在必要时创建服务并立即启动；卸载会先停止服务，再删除服务和文件。</SettingsHelp>
      </div>
      <button type="button" className="setup-service-refresh" aria-label="刷新服务状态" title="刷新服务状态" disabled={!nativeRuntime || checking || pending !== null} onClick={() => void refresh()}><RotateCcw size={15} /></button>
    </div>
    <div className="setup-service-status" aria-live="polite">
      <span className={`setup-service-badge ${status?.state ?? 'unknown'}`}><i aria-hidden="true" />{stateLabel}</span>
    </div>
    <div className="setup-service-rpc" aria-live="polite">
      <span>RPC 连接：{rpcLabel}</span>
      {rpcInfo && <span>{rpcInfo.name} v{rpcInfo.version} · {rpcInfo.protocolVersion} · {rpcInfo.pipeName}</span>}
    </div>
    <div className="setup-service-actions">
      <button type="button" className={`dialog-secondary ${serviceRunning ? 'danger' : ''}`} aria-label={`${actionLabel}服务`} disabled={actionDisabled} onClick={() => void run(action)}><ActionIcon size={14} />{pending === action ? pendingLabel : actionLabel}</button>
    </div>
    {error && <p className="setup-service-feedback error" role="alert">{error}</p>}
    {!error && <p className={`setup-service-feedback ${!pending && rpcError ? 'error' : ''}`} role="status">{pending ? '正在请求管理员权限，请在 Windows 授权窗口中允许。' : rpcError || message}</p>}
  </section>
}
