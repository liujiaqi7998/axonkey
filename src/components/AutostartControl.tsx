import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { SettingsHelp } from './SettingsHelp'
import { disable, enable, isEnabled } from '@tauri-apps/plugin-autostart'

type AutostartControlProps = {
  supported: boolean
  windows: boolean
}

export function AutostartControl({ supported, windows }: AutostartControlProps) {
  const [enabled, setEnabled] = useState<boolean | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [refreshVersion, setRefreshVersion] = useState(0)
  const inFlight = useRef(false)

  useEffect(() => {
    if (!supported) return
    let active = true
    let reading = false
    const refresh = async () => {
      if (inFlight.current || reading) return
      reading = true
      setBusy(true)
      try {
        const value = await isEnabled()
        if (active) { setEnabled(value); setError('') }
      } catch (cause) {
        if (active) setError(`无法读取开机自启设置：${String(cause)}`)
      } finally {
        reading = false
        if (active) setBusy(false)
      }
    }
    void refresh()
    window.addEventListener('focus', refresh)
    return () => { active = false; window.removeEventListener('focus', refresh) }
  }, [supported, refreshVersion])

  const toggle = async () => {
    if (!supported || inFlight.current) return
    inFlight.current = true
    setBusy(true)
    setError('')
    try {
      // Read again before changing: the OS setting may have changed externally.
      const current = await isEnabled()
      const next = !current
      try {
        if (next) await enable()
        else await disable()
        if (windows) await invoke('set_windows_service_autostart', { enabled: next })
      } catch (cause) {
        // Keep the app and service settings aligned when UAC is cancelled or
        // changing the service startup type otherwise fails.
        if (windows) {
          const changed = await isEnabled().catch(() => next)
          if (changed !== current) {
            if (current) await enable()
            else await disable()
          }
        }
        throw cause
      }
      const actual = await isEnabled()
      setEnabled(actual)
      if (actual === current) setError('开机自启设置未生效，请重试。')
    } catch (cause) {
      setError(`无法更改开机自启设置：${String(cause)}`)
      try { setEnabled(await isEnabled()) } catch { setEnabled(null) }
    } finally {
      inFlight.current = false
      setBusy(false)
    }
  }

  return <section className="settings-autostart" aria-label="启动设置">
    <div className="settings-form-row">
      <span className="settings-form-label">启动：</span>
      <div className="settings-form-control">
        <label className="settings-checkbox"><input type="checkbox" checked={enabled === true} disabled={!supported || busy || enabled === null} onChange={() => void toggle()} />开机自启</label>
        <SettingsHelp id="autostart-description" label="开机自启">{supported ? windows ? '登录电脑后在后台启动 Axonkey，并通过 AxonkeyService RPC 将服务设为自动启动；关闭后改为手动启动。' : '登录电脑后在后台启动 Axonkey，不会自动打开窗口。' : '请在 Windows 或 macOS 桌面应用中设置。'}</SettingsHelp>
        <span className="settings-autostart-status" role="status">{busy ? '正在同步…' : ''}</span>
      </div>
    </div>
    {error && <p className="settings-autostart-error" role="alert">{error}<button type="button" disabled={busy} onClick={() => setRefreshVersion(value => value + 1)}>重新读取</button></p>}
  </section>
}
