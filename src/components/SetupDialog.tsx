import { useRef, useState } from 'react'
import {
  AudioLines,
  Bluetooth,
  Check,
  CheckCircle2,
  ChevronRight,
  Command,
  Download,
  ExternalLink,
  FolderOpen,
  Info,
  Keyboard,
  RotateCcw,
  Settings2,
  ShieldCheck,
  Trash2,
  X,
} from 'lucide-react'
import { WindowsServiceLog } from './WindowsServiceLog'
import { WindowsServiceControl } from './WindowsServiceControl'
import type { WindowsServiceAction, WindowsServiceStatus, WindowsServiceLog as ServiceLogSnapshot } from '../windowsService'
import type { DriverActionKind, DriverKind, SetupState, SetupStepId } from '../setupModel'
import { isSetupComplete } from '../setupModel'
import type { MacPermissionKind, MacPermissions, Platform } from '../appTypes'

type SetupDialogProps = {
  platform: Platform
  macPermissions: MacPermissions
  state: SetupState
  onClose: () => void
  onOpenStep: (step: SetupStepId) => void
  onCompleteStep: () => void
  onSkipStep: () => void
  onSkipAll: () => void
  onReset: () => void
  onDriverAction: (driver: DriverKind, action: DriverActionKind) => void
  onInstallDriver: () => Promise<void>
  nativeRuntime: boolean
  onQueryService: () => Promise<WindowsServiceStatus>
  onReadServiceLog: () => Promise<ServiceLogSnapshot>
  onServiceAction: (action: WindowsServiceAction) => Promise<WindowsServiceStatus>
  onProbeAudio: () => void
  onOpenSystemSettings: (page: 'bluetooth' | 'sound' | MacPermissionKind) => void
  onRequestMacPermission: (kind: MacPermissionKind) => void
  onCheckDevice: () => void
  onMarkDeviceConnected: () => void
  onFinish: () => void
}

const windowsSetupStepLabels: Record<SetupStepId, string> = {
  welcome: '开始',
  inputDriver: '驱动安装',
  deviceConnection: '连接设备',
  complete: '完成',
}

const macSetupStepLabels: Record<SetupStepId, string> = {
  ...windowsSetupStepLabels,
  inputDriver: '权限与音频',
}

const windowsSetupSteps: SetupStepId[] = ['welcome', 'inputDriver', 'deviceConnection', 'complete']
const macSetupSteps: SetupStepId[] = ['inputDriver', 'deviceConnection']

export function SetupDialog({ platform, macPermissions, state, onClose, onOpenStep, onCompleteStep, onSkipStep, onSkipAll, onReset, onDriverAction, onInstallDriver, nativeRuntime, onQueryService, onReadServiceLog, onServiceAction, onProbeAudio, onOpenSystemSettings, onRequestMacPermission, onCheckDevice, onMarkDeviceConnected, onFinish }: SetupDialogProps) {
  const step = state.currentStep
  const setupStepLabels = platform === 'macos' ? macSetupStepLabels : windowsSetupStepLabels
  const visibleSteps = platform === 'macos' ? macSetupSteps : windowsSetupSteps
  return <div className="setup-backdrop" role="presentation">
    <section className="setup-dialog" role="dialog" aria-modal="true" aria-labelledby="setup-title">
      <aside className="setup-progress">
        <div className="setup-brand"><span className="brand-mark">A</span><div><strong>Axonkey</strong><span>{platform === 'macos' ? '2 步完成设置' : '首次使用设置'}</span></div></div>
        <div className="setup-step-list">
          {visibleSteps.map((stepId, index) => {
            const status = state.steps[stepId].status
            const statusClass = status === 'complete' || status === 'skipped' ? status : ''
            return <button type="button" key={stepId} className={`setup-step ${step === stepId ? 'active' : ''} ${statusClass}`} onClick={() => onOpenStep(stepId)}>
              <span className="setup-step-index">{status === 'complete' ? <Check size={12} /> : index + 1}</span>
              <span>{setupStepLabels[stepId]}</span>
              {status === 'skipped' && <small>已跳过</small>}
            </button>
          })}
        </div>
        <button type="button" className="setup-skip-all" onClick={state.setupSkipped || isSetupComplete(state) ? onReset : onSkipAll}>{state.setupSkipped || isSetupComplete(state) ? '重新运行引导' : '跳过整个引导'}</button>
      </aside>
      <div className="setup-content">
        <button type="button" className="dialog-close setup-close" aria-label="关闭引导" onClick={onClose}><X size={17} /></button>
        {step === 'welcome' && <div className="setup-screen">
          <span className="setup-hero-icon"><Settings2 size={24} /></span>
          <span className="section-kicker">FIRST RUN</span>
          <h2 id="setup-title">{platform === 'macos' ? '准备好权限与遥控器' : '准备好驱动与遥控器'}</h2>
          <p className="setup-lead">{platform === 'macos'
            ? '整个过程只在本机完成。Axonkey 需要读取 RC003 输入并向当前应用发送映射后的按键。'
            : '先安装驱动和后台服务，再通过蓝牙连接遥控器，即可开始配置按键映射。'}</p>
          <div className={`setup-summary-grid ${platform === 'macos' ? '' : 'windows-setup-summary'}`}>
            {platform === 'macos' ? <>
              <div><Keyboard size={18} /><strong>输入监控</strong><span>仅监听目标 RC003 的 HID 报告</span></div>
              <div><Command size={18} /><strong>辅助功能</strong><span>发送映射后的按键与文本</span></div>
              <div><Bluetooth size={18} /><strong>连接 RC003</strong><span>通过 macOS 蓝牙配对并唤醒</span></div>
            </> : <>
              <div><ShieldCheck size={18} /><strong>安装驱动</strong><span>安装专用驱动与后台服务</span></div>
              <div><Bluetooth size={18} /><strong>连接 RC003</strong><span>通过 Windows 蓝牙配对并唤醒</span></div>
            </>}
          </div>
          <div className="setup-actions"><button type="button" className="button primary setup-primary" onClick={onCompleteStep}>开始设置 <ChevronRight size={15} /></button></div>
        </div>}
        {step === 'inputDriver' && (platform === 'macos'
          ? <MacPermissionsSetupScreen
            permissions={macPermissions}
            state={state}
            onRequest={onRequestMacPermission}
            onAudioAction={onDriverAction}
            onProbeAudio={onProbeAudio}
            onOpenSound={() => onOpenSystemSettings('sound')}
            onContinue={onCompleteStep}
            onSkip={onSkipStep}
          />
          : <DriversSetupScreen
            onInstall={onInstallDriver}
            nativeRuntime={nativeRuntime}
            onQueryService={onQueryService}
            onReadServiceLog={onReadServiceLog}
            onServiceAction={onServiceAction}
            onContinue={onCompleteStep}
            onSkip={onSkipStep}
          />)}
        {step === 'deviceConnection' && <div className="setup-screen">
          <span className="setup-hero-icon"><Bluetooth size={24} /></span>
          <span className="section-kicker">DEVICE</span>
          <h2 id="setup-title">连接小米遥控器 RC003</h2>
          <p className="setup-lead">先在{platform === 'macos' ? '系统' : ' Windows'}蓝牙设置中完成配对，再按遥控器任意按键将它唤醒。Axonkey 只处理 VID 2717 / PID 32B8 的目标设备。</p>
          <div className={`setup-status-panel ${state.device.status}`}><span className="setup-status-dot" /><div><strong>{state.device.status === 'connected' ? 'RC003 已连接' : state.device.status === 'checking' ? '正在检查设备' : '尚未确认连接'}</strong><span>{state.device.message ?? '打开系统设置完成蓝牙配对，然后返回这里检查。'}</span></div></div>
          <div className="setup-inline-actions"><button type="button" className="dialog-secondary" onClick={() => onOpenSystemSettings('bluetooth')}><Bluetooth size={14} /> 打开蓝牙设置</button><button type="button" className="dialog-secondary" onClick={onCheckDevice}><RotateCcw size={14} /> 重新检测</button><button type="button" className="dialog-secondary" onClick={onMarkDeviceConnected}><Check size={14} /> 我已连接</button></div>
          <div className="setup-actions"><button type="button" className="setup-text-button" onClick={platform === 'macos' ? () => { onSkipStep(); onFinish() } : onSkipStep}>稍后连接</button><button type="button" className="button primary setup-primary" disabled={state.device.status !== 'connected'} onClick={platform === 'macos' ? onFinish : onCompleteStep}>{platform === 'macos' ? '完成设置' : '继续'} <ChevronRight size={15} /></button></div>
        </div>}
        {step === 'complete' && <div className="setup-screen setup-complete">
          <span className="setup-hero-icon success"><Check size={26} /></span>
          <span className="section-kicker">READY</span>
          <h2 id="setup-title">基本设置已完成</h2>
          <p className="setup-lead">映射配置会自动保存。以后点击顶部的设备状态卡，可以重新打开这里检查{platform === 'macos' ? '系统权限' : '驱动'}或 RC003 连接。</p>
          <div className="setup-result-list">
            {platform === 'macos' ? <>
              <span><Keyboard size={15} /> 输入监控：{macPermissions.inputMonitoring ? '已授权' : '稍后授权'}</span>
              <span><Command size={15} /> 辅助功能：{macPermissions.accessibility ? '已授权' : '稍后授权'}</span>
              <span><AudioLines size={15} /> MiRemoteV 2ch：{driverStatusLabel(state.drivers.audio.status)}</span>
            </> : null}
            <span><Bluetooth size={15} /> RC003：{state.device.status === 'connected' ? '已连接' : '稍后连接'}</span>
          </div>
          <div className="setup-actions"><button type="button" className="button primary setup-primary" onClick={onFinish}>进入按键映射</button></div>
        </div>}
      </div>
    </section>
  </div>
}

type MacPermissionsSetupScreenProps = {
  permissions: MacPermissions
  state: SetupState
  onRequest: (kind: MacPermissionKind) => void
  onAudioAction: (driver: DriverKind, action: DriverActionKind) => void
  onProbeAudio: () => void
  onOpenSound: () => void
  onContinue: () => void
  onSkip: () => void
}

function MacPermissionsSetupScreen({ permissions, state, onRequest, onAudioAction, onProbeAudio, onOpenSound, onContinue, onSkip }: MacPermissionsSetupScreenProps) {
  const ready = permissions.inputMonitoring && permissions.accessibility
  const grantedCount = Number(permissions.inputMonitoring) + Number(permissions.accessibility)
  const activeKind: MacPermissionKind | null = !permissions.inputMonitoring
    ? 'inputMonitoring'
    : !permissions.accessibility ? 'accessibility' : null
  const items = [
    {
      kind: 'inputMonitoring' as const,
      title: '输入监控',
      description: '允许 IOKit 读取目标 RC003 的原始 HID 按键报告。',
      granted: permissions.inputMonitoring,
      icon: <Keyboard size={18} />,
    },
    {
      kind: 'accessibility' as const,
      title: '辅助功能',
      description: '允许 CoreGraphics 发送映射后的按键、快捷键和文本。',
      granted: permissions.accessibility,
      icon: <Command size={18} />,
    },
  ]
  const audio = state.drivers.audio
  const inputAuthorizationError = state.drivers.input.status === 'error'
    ? state.drivers.input.message
    : null
  const audioInstalled = isDriverInstalled(state, 'audio')
  const audioRunning = audio.action.status === 'running'
  return <div className="setup-screen mac-permission-screen">
    <span className="section-kicker">SYSTEM ACCESS</span>
    <h2 id="setup-title">先完成两项必要授权</h2>
    <p className="setup-lead">按顺序操作即可。每次授权都会打开对应的系统设置，并保留一个置顶小窗协助完成添加。</p>

    <div className={`permission-progress-summary ${ready ? 'ready' : ''}`} aria-live="polite">
      <span className="permission-progress-icon"><ShieldCheck size={24} /></span>
      <div><strong>{ready ? '系统权限已就绪' : `已完成 ${grantedCount} / 2`}</strong><span>{ready ? 'Axonkey 可以拦截原始按键并执行你的映射。' : '只突出当前需要完成的一项，授权后会自动刷新。'}</span></div>
      <span className="permission-progress-count">{grantedCount}/2</span>
    </div>

    <div className="mac-permission-list">
      {items.map((item, index) => {
        const current = activeKind === item.kind
        const waiting = !item.granted && !current
        return <section key={item.kind} className={`mac-permission-step ${item.granted ? 'granted' : current ? 'current' : 'waiting'}`}>
          <div className="permission-step-number">{item.granted ? <Check size={16} /> : index + 1}</div>
          <span className="permission-step-icon">{item.icon}</span>
          <div className="permission-step-copy"><div><h3>{item.title}</h3><span className="permission-status-label">{item.granted ? '已授权' : current ? '现在完成' : '下一步'}</span></div><p>{item.description}</p></div>
          {current && <button type="button" className="button primary permission-request-button" onClick={() => onRequest(item.kind)}>开始授权 <ExternalLink size={15} /></button>}
          {item.granted && <CheckCircle2 className="permission-complete-icon" size={21} />}
          {waiting && <span className="permission-waiting-label">完成上一步后继续</span>}
        </section>
      })}
    </div>

    <section className={`mac-audio-setup ${audio.status}`}>
      <span className="permission-step-icon"><AudioLines size={18} /></span>
      <div className="mac-audio-copy">
        <div><h3>MiRemoteV 2ch 虚拟麦克风</h3><span className="permission-status-label">{driverStatusLabel(audio.status)}</span></div>
        <p>{audio.action.error ?? audio.message ?? '安装后，Axonkey 会将 RC003 语音转发到虚拟麦克风。'}</p>
        <p>请将语音输入法的麦克风设为 <strong>MiRemoteV 2ch</strong>，以接收遥控器语音。</p>
        <p>豆包输入法：设置 → 语音输入 → 麦克风选择。</p>
      </div>
      <div className="mac-audio-actions">
        {!audioInstalled && <button type="button" className="dialog-secondary" disabled={audioRunning} onClick={() => onAudioAction('audio', 'install')}><Download size={14} /> {audioRunning ? '等待授权…' : '安装驱动'}</button>}
        {audioInstalled && <button type="button" className="dialog-secondary danger" disabled={audioRunning} onClick={() => onAudioAction('audio', 'uninstall')}><Trash2 size={14} /> {audioRunning ? '等待授权…' : '卸载'}</button>}
        <button type="button" className="dialog-secondary" disabled={audioRunning || audio.status === 'checking'} onClick={onProbeAudio}><RotateCcw size={14} /> {audio.status === 'checking' ? '检测中' : '重新检测'}</button>
        <button type="button" className="dialog-secondary" disabled={audioRunning} onClick={onOpenSound}><Settings2 size={14} /> 声音设置</button>
      </div>
    </section>

    {inputAuthorizationError
      ? <div className="permission-drag-note"><Info size={17} /><div><strong>需要重新授权当前构建</strong><span>{inputAuthorizationError}</span></div></div>
      : !ready && <div className="permission-drag-note"><Info size={17} /><div><strong>系统列表中没有 Axonkey？</strong><span>授权小窗可以在 Finder 中定位当前应用，再将 Axonkey.app 拖入系统设置列表。</span></div></div>}
    <div className="setup-actions"><button type="button" className="setup-text-button" onClick={onSkip}>稍后授权</button><button type="button" className="button primary setup-primary" disabled={!ready} onClick={onContinue}>继续 <ChevronRight size={15} /></button></div>
  </div>
}

export type MacPermissionHelperWindowProps = {
  activePermission: MacPermissionKind
  permissions: MacPermissions
  onOpenSettings: (kind: MacPermissionKind) => void
  onRevealApp: () => void
  onRefresh: () => void
  onClose: () => void
}

export function MacPermissionHelperWindow({ activePermission, permissions, onOpenSettings, onRevealApp, onRefresh, onClose }: MacPermissionHelperWindowProps) {
  const ready = permissions.inputMonitoring && permissions.accessibility
  const activeTitle = activePermission === 'inputMonitoring' ? '输入监控' : '辅助功能'
  return <main className="permission-helper-shell">
    <header className="permission-helper-header">
      <div className="setup-brand"><span className="brand-mark">A</span><div><strong>Axonkey</strong><span>授权助手 · 保持置顶</span></div></div>
      <button type="button" className="dialog-close" aria-label="返回完整窗口" onClick={onClose}><X size={17} /></button>
    </header>

    <section className="permission-helper-content">
      <span className="section-kicker">{ready ? 'PERMISSIONS READY' : `CURRENT · ${activePermission === 'inputMonitoring' ? '1 / 2' : '2 / 2'}`}</span>
      <h1>{ready ? '两项权限都已打开' : `在系统设置中允许${activeTitle}`}</h1>
      <p className="permission-helper-lead">{ready ? '状态已自动刷新，可以返回引导继续连接遥控器。' : '若列表中已有 Axonkey，直接打开右侧开关；没有时按下面两步添加。'}</p>

      <div className="permission-helper-status" aria-live="polite">
        <span className={permissions.inputMonitoring ? 'granted' : activePermission === 'inputMonitoring' ? 'active' : ''}><Keyboard size={15} /> 输入监控 <strong>{permissions.inputMonitoring ? '已授权' : '待授权'}</strong></span>
        <span className={permissions.accessibility ? 'granted' : activePermission === 'accessibility' ? 'active' : ''}><Command size={15} /> 辅助功能 <strong>{permissions.accessibility ? '已授权' : '待授权'}</strong></span>
      </div>

      {!ready && <div className="permission-drag-guide">
        <div className="permission-drag-visual" aria-hidden="true"><span className="permission-app-tile"><span className="brand-mark">A</span>Axonkey.app</span><ChevronRight size={18} /><span className="permission-settings-tile"><Settings2 size={18} />系统设置</span></div>
        <ol><li>点击“在 Finder 中显示”，找到高亮的 Axonkey.app。</li><li>将它拖到已打开的系统授权列表，再打开开关。</li></ol>
      </div>}

      <div className="permission-helper-actions">
        {!ready && <button type="button" className="button primary" onClick={onRevealApp}><FolderOpen size={16} /> 在 Finder 中显示</button>}
        {!ready && <button type="button" className="dialog-secondary" onClick={() => onOpenSettings(activePermission)}><ExternalLink size={15} /> 再次打开系统设置</button>}
      </div>
    </section>

    <footer className="permission-helper-footer">
      <button type="button" className="setup-text-button permission-refresh-button" onClick={onRefresh}><RotateCcw size={14} /> 重新检测</button>
      <button type="button" className={`button ${ready ? 'primary' : 'permission-return-button'}`} onClick={onClose}>{ready ? '继续连接遥控器' : '返回引导'} <ChevronRight size={15} /></button>
    </footer>
  </main>
}

type DriversSetupScreenProps = Pick<SetupDialogProps, 'nativeRuntime' | 'onQueryService' | 'onServiceAction' | 'onReadServiceLog'> & {
  onInstall: () => Promise<void>
  onContinue: () => void
  onSkip: () => void
}

function driverStatusLabel(status: SetupState['drivers']['input']['status']) {
  const labels = { unknown: '未检查', checking: '检查中', missing: '未安装', installed: '已安装', restartRequired: '等待重启', error: '操作失败' }
  return labels[status]
}

function isDriverInstalled(state: SetupState, kind: DriverKind) {
  const status = state.drivers[kind].status
  return status === 'installed' || status === 'restartRequired'
}

function DriversSetupScreen({ onInstall, onContinue, onSkip, nativeRuntime, onQueryService, onReadServiceLog, onServiceAction }: DriversSetupScreenProps) {
  const [serviceBusy, setServiceBusy] = useState(false)
  const launchingRef = useRef(false)
  const [launching, setLaunching] = useState(false)
  const [opened, setOpened] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const install = async () => {
    if (launchingRef.current || serviceBusy) return
    launchingRef.current = true
    setLaunching(true)
    setOpened(false)
    setError(null)
    try {
      await onInstall()
      setOpened(true)
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error))
    } finally {
      launchingRef.current = false
      setLaunching(false)
    }
  }

  return <div className="setup-screen drivers-setup-screen">
    <h2 id="setup-title">驱动与服务</h2>
    <div className="driver-installer-content">
      <div className="driver-installer-heading"><h3>设备驱动</h3><span>Quarbor</span></div>
      <button type="button" className="button primary setup-primary driver-install-button" disabled={launching || serviceBusy} aria-busy={launching} onClick={() => void install()}>
        <ShieldCheck size={20} /> {launching ? '正在打开…' : '安装驱动'}
      </button>
      {error && <p className="driver-installer-feedback error" role="alert">{error}</p>}
      {!error && <p className="driver-installer-feedback" role="status">{launching ? '请在 Windows 授权窗口中允许打开安装器。' : opened ? '安装器已打开，请在安装器中完成安装后继续。' : ''}</p>}
    </div>
    <WindowsServiceControl nativeRuntime={nativeRuntime} disabled={launching} onBusyChange={setServiceBusy} onQuery={onQueryService} onAction={onServiceAction} />
    <WindowsServiceLog nativeRuntime={nativeRuntime} onRead={onReadServiceLog} />
    <div className="setup-actions">
      <button type="button" className="setup-text-button" disabled={launching || serviceBusy} onClick={onSkip}>稍后安装</button>
      <button type="button" className="dialog-secondary setup-primary" disabled={launching || serviceBusy} onClick={onContinue}>继续 <ChevronRight size={15} /></button>
    </div>
  </div>
}
