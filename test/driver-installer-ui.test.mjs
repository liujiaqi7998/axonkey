import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync, existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import vm from 'node:vm'
import ts from 'typescript'
import React from 'react'
import Renderer, { act } from 'react-test-renderer'

const require = createRequire(import.meta.url)
const timers = new Map()
let nextTimer = 0
const window = { setInterval: callback => { timers.set(++nextTimer, callback); return nextTimer }, clearInterval: id => timers.delete(id), addEventListener() {}, removeEventListener() {} }
function load(file) {
  const module = { exports: {} }
  const source = ts.transpileModule(readFileSync(file, 'utf8'), { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText
  vm.runInNewContext(source, {
    module, exports: module.exports, window, setTimeout, clearTimeout,
    require: name => {
      if (!name.startsWith('.')) return require(name)
      const target = resolve(dirname(file), name)
      return load(existsSync(target + '.ts') ? target + '.ts' : target + '.tsx')
    },
  }, { filename: file })
  return module.exports
}
const { SetupDialog } = load(resolve('src/components/SetupDialog.tsx'))
const { createDefaultSetupState } = load(resolve('src/setupModel.ts'))
const text = node => typeof node === 'string' ? node : (node.children ?? []).map(text).join('')
const button = (renderer, label) => renderer.root.findAllByType('button').find(node => text(node).trim() === label)

function props(overrides = {}) {
  const state = createDefaultSetupState()
  state.currentStep = 'inputDriver'
  const noop = () => {}
  return {
    platform: 'windows', state, nativeRuntime: false,
    onReadServiceLog: () => assert.fail('Browser preview must not read logs'),
    onQueryService: () => assert.fail('Browser preview must not query the SCM'),
    onServiceAction: () => assert.fail('Browser preview must not manage services'),
    macPermissions: { inputMonitoring: false, accessibility: false },
    onClose: noop, onOpenStep: noop, onCompleteStep: noop, onSkipStep: noop,
    onSkipAll: noop, onReset: noop, onDriverAction: noop, onInstallDriver: async () => {},
    onProbeAudio: noop, onOpenSystemSettings: noop, onRequestMacPermission: noop,
    onCheckDevice: noop, onMarkDeviceConnected: noop, onFinish: noop,
    ...overrides,
  }
}

test('installer launch blocks duplicate clicks and does not complete installation or advance setup', async () => {
  let resolveLaunch, launches = 0, advances = 0
  const pending = new Promise(resolve => { resolveLaunch = resolve })
  const options = props({
    onInstallDriver: () => { launches++; return pending },
    onCompleteStep: () => { advances++ },
    onDriverAction: () => assert.fail('legacy driver action must not run'),
  })
  const originalState = JSON.stringify(options.state)
  const renderer = Renderer.create(React.createElement(SetupDialog, options))
  try {
    const install = button(renderer, '安装驱动')
    await act(async () => { install.props.onClick(); install.props.onClick() })
    assert.equal(launches, 1)
    assert.equal(button(renderer, '正在打开…').props.disabled, true)
    assert.equal(button(renderer, '继续').props.disabled, true)
    await act(async () => { resolveLaunch() })
    assert.equal(button(renderer, '安装驱动').props.disabled, false)
    assert.match(text(renderer.root.findByProps({ className: 'driver-installer-feedback' })), /安装器已打开/)
    assert.equal(advances, 0)
    assert.equal(JSON.stringify(options.state), originalState)
    await act(async () => { button(renderer, '继续').props.onClick() })
    assert.equal(advances, 1)
  } finally { renderer.unmount() }
})

test('cancelled authorization shows an error and permits a successful retry', async () => {
  let launches = 0
  const renderer = Renderer.create(React.createElement(SetupDialog, props({ onInstallDriver: async () => {
    if (++launches === 1) throw '已取消管理员授权'
  } })))
  try {
    await act(async () => { button(renderer, '安装驱动').props.onClick() })
    assert.match(text(renderer.root.findByProps({ role: 'alert' })), /已取消管理员授权/)
    assert.equal(button(renderer, '安装驱动').props.disabled, false)
    await act(async () => { button(renderer, '安装驱动').props.onClick() })
    assert.equal(launches, 2)
    assert.equal(renderer.root.findAllByProps({ role: 'alert' }).length, 0)
    assert.match(text(renderer.root.findByProps({ className: 'driver-installer-feedback' })), /安装器已打开/)
  } finally { renderer.unmount() }
})

test('macOS continues to use its own audio driver action', async () => {
  const actions = []
  const renderer = Renderer.create(React.createElement(SetupDialog, props({
    platform: 'macos',
    onInstallDriver: () => assert.fail('Windows installer must not open on macOS'),
    onDriverAction: (...args) => actions.push(args),
  })))
  try {
    await act(async () => { button(renderer, '安装驱动').props.onClick() })
    assert.deepEqual(actions, [['audio', 'install']])
  } finally { renderer.unmount() }
})

const { WindowsServiceControl } = load(resolve('src/components/WindowsServiceControl.tsx'))
const status = state => ({ state, processId: state === 'running' ? 42 : 0, exitCode: 0 })
const serviceButton = (renderer, action) => renderer.root.findByProps({ 'aria-label': `${action}服务` })

async function serviceHarness(initial = 'notInstalled', options = {}) {
  let current = status(initial)
  const requests = [], busy = []
  let queries = 0
  const config = {
    nativeRuntime: true, disabled: false, onBusyChange: value => busy.push(value),
    onQuery: async () => { queries++; return current },
    onAction: async action => {
      requests.push(action)
      current = status({ install: 'stopped', start: 'running', stop: 'stopped', uninstall: 'notInstalled' }[action])
      return current
    },
    ...options,
  }
  let renderer
  await act(async () => { renderer = Renderer.create(React.createElement(WindowsServiceControl, config)) })
  return { renderer, requests, busy, get queries() { return queries },
    async click(label) { await act(async () => { serviceButton(renderer, label).props.onClick() }) },
    async close() { await act(async () => { renderer.unmount() }) },
  }
}

test('service lifecycle enables only valid actions and refreshes actual state', async () => {
  const app = await serviceHarness()
  try {
    assert.equal(serviceButton(app.renderer, '安装').props.disabled, false)
    assert.equal(serviceButton(app.renderer, '启动').props.disabled, true)
    await app.click('安装')
    assert.equal(serviceButton(app.renderer, '安装').props.disabled, true)
    assert.equal(serviceButton(app.renderer, '启动').props.disabled, false)
    await app.click('启动')
    assert.match(text(app.renderer.root), /运行中/)
    assert.equal(serviceButton(app.renderer, '停止').props.disabled, false)
    await app.click('停止')
    assert.match(text(app.renderer.root), /已停止/)
    await app.click('卸载')
    assert.match(text(app.renderer.root), /未安装/)
    assert.deepEqual(app.requests, ['install', 'start', 'stop', 'uninstall'])
    assert.deepEqual(app.busy, [true, false, true, false, true, false, true, false])
  } finally { await app.close() }
})

test('service UAC cancellation preserves state, prevents duplicate submission and allows retry', async () => {
  let rejectAction, attempts = 0
  const app = await serviceHarness('stopped', { onAction: () => { attempts++; return new Promise((_, reject) => { rejectAction = reject }) } })
  try {
    const start = serviceButton(app.renderer, '启动')
    await act(async () => { start.props.onClick(); start.props.onClick() })
    assert.equal(attempts, 1)
    for (const label of ['安装', '卸载', '启动', '停止']) assert.equal(serviceButton(app.renderer, label).props.disabled, true)
    await act(async () => { rejectAction('已取消管理员授权，可重试。') })
    assert.match(text(app.renderer.root.findByProps({ role: 'alert' })), /已取消管理员授权/)
    assert.match(text(app.renderer.root), /已停止/)
    assert.equal(serviceButton(app.renderer, '启动').props.disabled, false)
    await app.click('启动')
    assert.equal(attempts, 2)
    await act(async () => { rejectAction('操作失败') })
  } finally { await app.close() }
})

test('service query failure stays unknown and disables actions instead of claiming uninstalled', async () => {
  const app = await serviceHarness('notInstalled', { onQuery: async () => { throw '拒绝访问' } })
  try {
    assert.match(text(app.renderer.root), /无法获取状态/)
    assert.match(text(app.renderer.root.findByProps({ role: 'alert' })), /拒绝访问/)
    for (const label of ['安装', '卸载', '启动', '停止']) assert.equal(serviceButton(app.renderer, label).props.disabled, true)
  } finally { await app.close() }
})

test('browser preview never queries or changes a Windows service', async () => {
  const app = await serviceHarness('notInstalled', { nativeRuntime: false })
  try {
    assert.equal(app.queries, 0)
    assert.match(text(app.renderer.root), /仅桌面版可检测/)
    for (const label of ['安装', '卸载', '启动', '停止']) assert.equal(serviceButton(app.renderer, label).props.disabled, true)
  } finally { await app.close() }
})

const { WindowsServiceLog } = load(resolve('src/components/WindowsServiceLog.tsx'))
test('service logs load on opening and refresh replaces rotated content, retaining last snapshot on failure', async () => {
  let calls = 0
  let current = { path: 'C:/服务/AxonkeyService.log', content: '中文旧日志', exists: true, truncated: false }
  let failure = false
  let renderer
  await act(async () => { renderer = Renderer.create(React.createElement(WindowsServiceLog, {
    nativeRuntime: true, onRead: async () => { calls++; if (failure) throw '读取失败'; return current },
  })) })
  try {
    assert.equal(calls, 1)
    assert.equal(text(renderer.root.findByType('pre')), '中文旧日志')
    current = { ...current, content: '轮转后新日志' }
    await act(async () => renderer.root.findByProps({ 'aria-label': '刷新服务日志' }).props.onClick())
    assert.equal(text(renderer.root.findByType('pre')), '轮转后新日志')
    failure = true
    await act(async () => renderer.root.findByProps({ 'aria-label': '刷新服务日志' }).props.onClick())
    assert.match(text(renderer.root.findByProps({ role: 'alert' })), /读取失败/)
    assert.equal(text(renderer.root.findByType('pre')), '轮转后新日志')
    failure = false
    current = { ...current, exists: false, content: '' }
    await act(async () => renderer.root.findByProps({ 'aria-label': '刷新服务日志' }).props.onClick())
    assert.equal(renderer.root.findAllByType('pre').length, 0)
    assert.match(text(renderer.root), /暂无运行日志/)
  } finally { await act(async () => renderer.unmount()) }
})

test('service log refresh blocks overlapping requests and browser mode never reads files', async () => {
  let calls = 0, finish
  const onRead = () => { calls++; return new Promise(resolve => { finish = resolve }) }
  let renderer
  await act(async () => { renderer = Renderer.create(React.createElement(WindowsServiceLog, { nativeRuntime: false, onRead })) })
  assert.equal(calls, 0)
  await act(async () => renderer.update(React.createElement(WindowsServiceLog, { nativeRuntime: true, onRead })))
  try {
    const refresh = renderer.root.findByProps({ 'aria-label': '刷新服务日志' })
    assert.equal(refresh.props.disabled, true)
    await act(async () => { refresh.props.onClick(); refresh.props.onClick() })
    assert.equal(calls, 1)
    await act(async () => finish({ path: 'test.log', content: '', exists: true, truncated: false }))
    assert.match(text(renderer.root), /日志文件为空/)
    assert.equal(renderer.root.findByProps({ 'aria-label': '刷新服务日志' }).props.disabled, false)
  } finally { await act(async () => renderer.unmount()) }
})
