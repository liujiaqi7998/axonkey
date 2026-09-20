import { useEffect, useRef, useState } from 'react'
import { RotateCcw } from 'lucide-react'
import type { WindowsServiceLog as LogSnapshot } from '../windowsService'

export function WindowsServiceLog({ nativeRuntime, onRead }: { nativeRuntime: boolean; onRead: () => Promise<LogSnapshot> }) {
  const [snapshot, setSnapshot] = useState<LogSnapshot>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [updatedAt, setUpdatedAt] = useState('')
  const active = useRef(false)
  const mounted = useRef(false)
  const output = useRef<HTMLPreElement>(null)
  const read = async () => {
    if (!nativeRuntime || active.current) return
    active.current = true
    setLoading(true)
    setError('')
    try {
      const next = await onRead()
      if (mounted.current) {
        setSnapshot(next)
        setUpdatedAt(new Date().toLocaleTimeString('zh-CN', { hour12: false }))
      }
    } catch (error) {
      if (mounted.current) setError(error instanceof Error ? error.message : String(error))
    } finally {
      active.current = false
      if (mounted.current) setLoading(false)
    }
  }
  useEffect(() => {
    mounted.current = true
    void read()
    return () => { mounted.current = false }
  }, [nativeRuntime, onRead])
  useEffect(() => {
    if (output.current) output.current.scrollTop = output.current.scrollHeight
  }, [snapshot])

  return <section className="setup-service-log" aria-labelledby="service-log-title">
    <div className="setup-service-heading">
      <h3 id="service-log-title">服务运行日志</h3>
      <button type="button" className="dialog-secondary" disabled={!nativeRuntime || loading} aria-label="刷新服务日志" onClick={() => void read()}><RotateCcw size={14} />{loading ? '读取中…' : '刷新'}</button>
    </div>
    {snapshot?.path && <p className="service-log-path" title={snapshot.path}>{snapshot.path}</p>}
    {error && <p className="setup-service-feedback error" role="alert">{error}{snapshot ? ' 下方保留上次读取的日志。' : ''}</p>}
    {snapshot?.content
      ? <pre ref={output} className="service-log-output" tabIndex={0} aria-label="服务运行日志内容" aria-busy={loading}>{snapshot.content}</pre>
      : <div className="service-log-empty" role="status">{!nativeRuntime ? '请在 Windows 桌面版中查看服务运行日志。' : loading ? '正在读取服务日志…' : error ? '暂时无法读取日志，请重试。' : snapshot?.exists ? '日志文件为空，等待服务写入。' : '暂无运行日志。启动服务后，点击刷新查看。'}</div>}
    {updatedAt && <p className="service-log-meta">上次读取 {updatedAt}{snapshot?.truncated ? ' · 仅显示末尾 100 KiB 内的记录' : ''}</p>}
  </section>
}
