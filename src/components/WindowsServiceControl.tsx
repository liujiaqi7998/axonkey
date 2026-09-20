import { useCallback, useEffect, useRef, useState } from 'react'
import { Download, Play, RotateCcw, Square, Trash2 } from 'lucide-react'
import { SettingsHelp } from './SettingsHelp'
import { canManageService, serviceStateLabels } from '../windowsService'
import type { WindowsServiceAction, WindowsServiceStatus } from '../windowsService'

export type WindowsServiceControlProps = {
  nativeRuntime: boolean
  disabled: boolean
  onBusyChange: (busy: boolean) => void
  onQuery: () => Promise<WindowsServiceStatus>
  onAction: (action: WindowsServiceAction) => Promise<WindowsServiceStatus>
}

const actions = [
  { kind: 'install', label: '安装', pending: '正在安装…', Icon: Download },
  { kind: 'uninstall', label: '卸载', pending: '正在卸载…', Icon: Trash2 },
  { kind: 'start', label: '启动', pending: '正在启动…', Icon: Play },
  { kind: 'stop', label: '停止', pending: '正在停止…', Icon: Square },
] as const

export function WindowsServiceControl({ nativeRuntime, disabled, onBusyChange, onQuery, onAction }: WindowsServiceControlProps) {
  const [status, setStatus] = useState<WindowsServiceStatus>()
  const [checking, setChecking] = useState(false)
  const [pending, setPending] = useState<WindowsServiceAction | null>(null)
  const [queryError, setQueryError] = useState('')
  const [actionError, setActionError] = useState('')
  const [message, setMessage] = useState('')
  const active = useRef(false)
  const querying = useRef(false)
  const mounted = useRef(false)
  const revision = useRef(0)
  const refresh = useCallback(async () => {
    if (!nativeRuntime || querying.current || active.current) return
    querying.current = true
    const request = ++revision.current
    setChecking(true)
    try {
      const next = await onQuery()
      if (mounted.current && request === revision.current) { setStatus(next); setQueryError('') }
    } catch (error) {
      if (mounted.current && request === revision.current) {
        setStatus(undefined)
        setQueryError(error instanceof Error ? error.message : String(error))
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
    return () => { mounted.current = false; ++revision.current; window.clearInterval(timer); window.removeEventListener('focus', refresh) }
  }, [nativeRuntime, refresh])

  const run = async (action: WindowsServiceAction) => {
    if (active.current || disabled || !nativeRuntime || !canManageService(status?.state, action)) return
    active.current = true
    ++revision.current
    setPending(action)
    onBusyChange(true)
    setActionError('')
    setMessage('')
    try {
      const next = await onAction(action)
      if (mounted.current) {
        setStatus(next)
        setQueryError('')
        setMessage(action === 'install' ? '服务已安装，已设为开机自动启动。现在可点击“启动”。' : '操作已完成，服务状态已刷新。')
      }
    } catch (error) {
      if (mounted.current) setActionError(error instanceof Error ? error.message : String(error))
    } finally {
      active.current = false
      onBusyChange(false)
      if (mounted.current) { setPending(null); void refresh() }
    }
  }

  return <section className="setup-service-panel" aria-labelledby="setup-service-title">
    <div className="setup-service-heading">
      <div className="setup-service-title"><h3 id="setup-service-title">后台服务</h3>
        <SettingsHelp id="setup-service-help" label="后台服务">AxonkeyService 负责遥控器设备管理和语音接收。请先完成驱动安装。服务安装后开机自动启动；停止或卸载会中断相关功能。安装、卸载、启动和停止均需 Windows 管理员授权。</SettingsHelp>
      </div>
      <button type="button" className="setup-service-refresh" aria-label="刷新服务状态" title="刷新服务状态" disabled={!nativeRuntime || checking || pending !== null} onClick={() => void refresh()}><RotateCcw size={15} /></button>
    </div>
    <div className="setup-service-status" aria-live="polite">
      <span className={`setup-service-badge ${status?.state ?? 'unknown'}`}><i aria-hidden="true" />{!nativeRuntime ? '仅桌面版可检测' : status ? serviceStateLabels[status.state] : checking ? '检测中…' : '无法获取状态'}</span>
      <span>AxonkeyService{status?.state === 'running' && status.processId > 0 ? ` · PID ${status.processId}` : ''}</span>
    </div>
    <div className="setup-service-actions">
      {actions.map(({ kind, label, pending: busyLabel, Icon }) => <button key={kind} type="button" className={`dialog-secondary ${kind === 'uninstall' ? 'danger' : ''}`} aria-label={`${label}服务`} disabled={!nativeRuntime || disabled || pending !== null || !canManageService(status?.state, kind)} onClick={() => void run(kind)}><Icon size={14} />{pending === kind ? busyLabel : label}</button>)}
    </div>
    {(actionError || queryError) && <p className="setup-service-feedback error" role="alert">{actionError || queryError}</p>}
    {!actionError && !queryError && <p className="setup-service-feedback" role="status">{pending ? '正在请求管理员权限并执行操作，请在 Windows 授权窗口中允许。' : status?.state === 'stopped' && status.exitCode !== 0 ? `服务已退出，系统错误码：${status.exitCode}。可重新启动或检查服务日志。` : message}</p>}
  </section>
}
