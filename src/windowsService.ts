export type WindowsServiceState = 'notInstalled' | 'stopped' | 'running' | 'startPending' | 'stopPending' | 'paused' | 'deletePending' | 'unknown'
export type WindowsServiceStatus = { state: WindowsServiceState; processId: number; exitCode: number }
export type WindowsServiceLog = { path: string; content: string; exists: boolean; truncated: boolean }
export type WindowsServiceAction = 'install' | 'uninstall' | 'start' | 'stop'

export const serviceStateLabels: Record<WindowsServiceState, string> = {
  notInstalled: '未安装', stopped: '已停止', running: '运行中', startPending: '正在启动',
  stopPending: '正在停止', paused: '已暂停', deletePending: '等待卸载完成', unknown: '状态未知',
}

export function canManageService(state: WindowsServiceState | undefined, action: WindowsServiceAction) {
  if (action === 'install') return state === 'notInstalled'
  if (action === 'start') return state === 'stopped'
  if (action === 'stop') return state === 'running' || state === 'paused'
  return state === 'stopped' || state === 'running' || state === 'paused'
}
