export type WindowsServiceState =
  | 'notInstalled'
  | 'stopped'
  | 'running'
  | 'startPending'
  | 'stopPending'
  | 'paused'
  | 'deletePending'
  | 'unknown'

export type WindowsServiceStatus = {
  state: WindowsServiceState
  processId: number
  exitCode: number
  rpc: {
    connected: boolean
    info: {
      name: string
      version: string
      protocolVersion: string
      pipeName: string
    } | null
    error: string | null
  }
}

export type WindowsServiceAction = 'install' | 'uninstall' | 'start' | 'stop'

export type WindowsDeviceInfo = {
  instanceId: string
  endpointPath: string
  driverMounted: boolean
  inputBlocked: boolean
  dataForwardEnabled: boolean
  connected: boolean
  batteryLevel: number | null
  descriptionName: string
}

export type WindowsDevicesProbe = {
  serviceAvailable: boolean
  device: WindowsDeviceInfo | null
  error: string | null
}

/**
 * Formats the device row in the mapping status card.
 *
 * `descriptionName` is best-effort metadata. The service can return a real
 * HID device while Bluetooth/GATT name discovery is unavailable, so presence
 * must be determined from `device`, not from the display name.
 */
export function windowsDeviceDisplayName(probe: WindowsDevicesProbe | null): string {
  if (!probe) return '检测中'
  if (!probe.serviceAvailable) return '无法访问到服务'
  if (probe.error) return '获取异常'
  if (probe.device) return probe.device.descriptionName || '小米遥控器 RC003'
  return '未获取到设备'
}

export const serviceStateLabels: Record<WindowsServiceState, string> = {
  notInstalled: '未安装',
  stopped: '已停止',
  running: '运行中',
  startPending: '正在启动',
  stopPending: '正在停止',
  paused: '已暂停',
  deletePending: '等待卸载完成',
  unknown: '状态未知',
}

export function canManageService(state: WindowsServiceState | undefined, action: WindowsServiceAction) {
  if (action === 'install') return state === 'notInstalled'
  if (action === 'start') return state === 'stopped'
  if (action === 'stop') return state === 'running' || state === 'paused'
  return state === 'stopped' || state === 'running' || state === 'paused'
}
