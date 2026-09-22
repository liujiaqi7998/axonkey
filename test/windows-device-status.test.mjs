import assert from 'node:assert/strict'
import test from 'node:test'
import { windowsDeviceDisplayName } from '../src/windowsService.ts'

const connectedDevice = {
  instanceId: 'HID\\VID_2717&PID_32B8\\RC003',
  endpointPath: '\\\\.\\Quarbor0',
  driverMounted: true,
  inputBlocked: true,
  dataForwardEnabled: true,
  connected: true,
  batteryLevel: 80,
  descriptionName: '',
}

test('uses the RC003 fallback when a connected device has no best-effort name', () => {
  assert.equal(windowsDeviceDisplayName({ serviceAvailable: true, device: connectedDevice, error: null }), '小米遥控器 RC003')
})

test('distinguishes service, request, and device states', () => {
  assert.equal(windowsDeviceDisplayName(null), '检测中')
  assert.equal(windowsDeviceDisplayName({ serviceAvailable: false, device: null, error: 'pipe unavailable' }), '无法访问到服务')
  assert.equal(windowsDeviceDisplayName({ serviceAvailable: true, device: null, error: 'GetDevices failed' }), '获取异常')
  assert.equal(windowsDeviceDisplayName({ serviceAvailable: true, device: null, error: null }), '未获取到设备')
  assert.equal(windowsDeviceDisplayName({ serviceAvailable: true, device: { ...connectedDevice, descriptionName: 'Xiaomi RC003' }, error: null }), 'Xiaomi RC003')
})
