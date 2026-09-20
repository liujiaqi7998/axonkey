#Requires -Version 5.1
<#
.SYNOPSIS
  验证并管理 windows/service/dist/AxonkeyService.exe（安装/卸载/启停/状态）。

.DESCRIPTION
  默认服务名 AxonkeyService；二进制固定为脚本上级目录 dist\AxonkeyService.exe。
  不带 -Action 进入交互菜单。Install/Uninstall/Start/Stop/Restart 需管理员并自动 UAC。
  Status 不需管理员。不安装 HID/虚拟麦克风驱动。

.PARAMETER Action
  Install | Uninstall | Start | Stop | Restart | Status

.PARAMETER ServiceName
  SCM 服务名，默认 AxonkeyService。

.PARAMETER NoElevate
  需要提权时不自动 UAC，直接失败。

.EXAMPLE
  .\Test-AxonkeyService.ps1
  .\Test-AxonkeyService.ps1 -Action Status
  .\Test-AxonkeyService.ps1 -Action Install
#>
[CmdletBinding()]
param(
    [Parameter()]
    [ValidateSet('Install', 'Uninstall', 'Start', 'Stop', 'Restart', 'Status')]
    [string]$Action,

    [Parameter()]
    [string]$ServiceName = 'AxonkeyService',

    [Parameter()]
    [switch]$NoElevate
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script:ServiceDisplayName = 'Axonkey Service'
$script:ServiceDescription = 'RC003 HID and voice service for Axonkey (dev/dist verification).'

function Get-ScriptRoot {
    if ($PSScriptRoot) { return $PSScriptRoot }
    return Split-Path -Parent $MyInvocation.MyCommand.Path
}

function Get-DistServiceExe {
    $root = Get-ScriptRoot
    return [System.IO.Path]::GetFullPath((Join-Path $root '..\dist\AxonkeyService.exe'))
}

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Write-Info([string]$Message) { Write-Host "[信息] $Message" -ForegroundColor Cyan }
function Write-Ok([string]$Message) { Write-Host "[成功] $Message" -ForegroundColor Green }
function Write-WarnLine([string]$Message) { Write-Host "[警告] $Message" -ForegroundColor Yellow }
function Write-ErrLine([string]$Message) { Write-Host "[错误] $Message" -ForegroundColor Red }

function Request-Elevation {
    param([string]$RequestedAction)
    if (Test-IsAdministrator) { return }
    if ($NoElevate) {
        throw "操作 '$RequestedAction' 需要管理员权限，且已指定 -NoElevate。"
    }
    Write-WarnLine "操作 '$RequestedAction' 需要管理员权限，正在请求 UAC 提权..."
    $argList = @(
        '-NoProfile'
        '-ExecutionPolicy', 'Bypass'
        '-File', ('"{0}"' -f $PSCommandPath)
        '-Action', $RequestedAction
        '-ServiceName', $ServiceName
    )
    $process = Start-Process -FilePath (Get-Process -Id $PID).Path `
        -Verb RunAs `
        -ArgumentList $argList `
        -Wait `
        -PassThru
    exit $process.ExitCode
}

function Get-ServiceRecord {
    try {
        return Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'" -ErrorAction Stop
    } catch {
        return $null
    }
}

function Get-ServiceObject {
    try {
        return Get-Service -Name $ServiceName -ErrorAction Stop
    } catch {
        return $null
    }
}

function Wait-ServiceState {
    param(
        [Parameter(Mandatory)][object]$Service,
        [Parameter(Mandatory)][string]$Desired,
        [int]$TimeoutSeconds = 45
    )
    $Service.Refresh()
    if ($Service.Status -eq $Desired) { return }
    $Service.WaitForStatus($Desired, [TimeSpan]::FromSeconds($TimeoutSeconds))
    $Service.Refresh()
    if ($Service.Status -ne $Desired) {
        throw "等待服务进入 $Desired 超时（当前: $($Service.Status)）。"
    }
}

function Assert-DistExe {
    $exe = Get-DistServiceExe
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
        throw @"
未找到服务程序:
  $exe

请先构建:
  cmake -S windows/service -B .build/service
  cmake --build .build/service --config Release
产物应位于 windows/service/dist/AxonkeyService.exe
"@
    }
    return $exe
}

function Format-FileInfo {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return '(文件不存在)'
    }
    $item = Get-Item -LiteralPath $Path
    $version = $null
    try {
        $version = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($Path).FileVersion
    } catch {}
    $sizeKb = [math]::Round($item.Length / 1KB, 1)
    $verText = if ($version) { $version } else { 'n/a' }
    $mtime = $item.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss')
    return "大小 ${sizeKb} KB | 修改 $mtime | 文件版本 $verText"
}

function Show-Status {
    $exe = Get-DistServiceExe
    Write-Host ''
    Write-Host '======== AxonkeyService 状态 ========' -ForegroundColor White
    Write-Host ("服务名        : {0}" -f $ServiceName)
    Write-Host ("期望 EXE      : {0}" -f $exe)
    Write-Host ("EXE 信息      : {0}" -f (Format-FileInfo $exe))

    $record = Get-ServiceRecord
    $service = Get-ServiceObject
    if (-not $record -and -not $service) {
        Write-Host '安装状态      : 未安装' -ForegroundColor Yellow
        Write-Host '===================================='
        Write-Host ''
        return
    }

    $status = if ($service) { $service.Status.ToString() } else { 'Unknown' }
    $statusColor = switch ($status) {
        'Running' { 'Green' }
        'Stopped' { 'Yellow' }
        default { 'Cyan' }
    }
    Write-Host '安装状态      : 已安装' -ForegroundColor Green
    Write-Host ("运行状态      : {0}" -f $status) -ForegroundColor $statusColor

    if ($record) {
        Write-Host ("显示名称      : {0}" -f $record.DisplayName)
        Write-Host ("启动类型      : {0}" -f $record.StartMode)
        Write-Host ("登录账户      : {0}" -f $record.StartName)
        Write-Host ("BinPath       : {0}" -f $record.PathName)
        $pidText = if ($record.ProcessId) { $record.ProcessId } else { '-' }
        Write-Host ("进程 PID      : {0}" -f $pidText)
        Write-Host ("可接受停止    : {0}" -f $record.AcceptStop)

        $registered = $record.PathName.Trim().Trim('"')
        if ($registered -and (Test-Path -LiteralPath $registered -PathType Leaf)) {
            $registeredFull = [System.IO.Path]::GetFullPath($registered)
            $expectedFull = [System.IO.Path]::GetFullPath($exe)
            if ($registeredFull -ieq $expectedFull) {
                Write-Ok 'BinPath 指向当前 dist\AxonkeyService.exe'
            } else {
                Write-WarnLine "BinPath 与 dist 不一致（注册: $registeredFull）"
            }
        } elseif ($registered) {
            Write-WarnLine "注册的 BinPath 文件不存在: $registered"
        }
    }

    $logPath = Join-Path (Split-Path -Parent $exe) 'AxonkeyService.log'
    Write-Host ("日志路径      : {0}" -f $logPath)
    if (Test-Path -LiteralPath $logPath -PathType Leaf) {
        Write-Host '---- 日志末尾（最多 15 行）----' -ForegroundColor DarkGray
        try {
            Get-Content -LiteralPath $logPath -Encoding UTF8 -Tail 15 -ErrorAction Stop |
                ForEach-Object { Write-Host $_ -ForegroundColor DarkGray }
        } catch {
            Write-WarnLine "读取日志失败: $($_.Exception.Message)"
        }
    } else {
        Write-Host '日志文件      : 尚不存在' -ForegroundColor DarkGray
    }
    Write-Host '===================================='
    Write-Host ''
}

function Install-ServiceFromDist {
    $exe = Assert-DistExe
    $binaryPath = '"{0}"' -f $exe
    Write-Info "使用 dist 二进制注册服务: $exe"

    $record = Get-ServiceRecord
    if ($record) {
        Write-Info '服务已存在，更新 PathName / 启动类型 / 账户...'
        $service = Get-ServiceObject
        if ($service -and $service.Status -ne 'Stopped') {
            Write-Info '先停止正在运行的服务...'
            Stop-ServiceTarget
        }
        $result = Invoke-CimMethod -InputObject $record -MethodName Change -Arguments @{
            PathName  = $binaryPath
            StartMode = 'Automatic'
            StartName = 'LocalSystem'
        }
        if ($result.ReturnValue -ne 0) {
            throw "更新服务配置失败: Win32=$($result.ReturnValue)"
        }
        Write-Ok "已更新服务 '$ServiceName' 指向 dist 可执行文件。"
    } else {
        New-Service -Name $ServiceName `
            -DisplayName $script:ServiceDisplayName `
            -Description $script:ServiceDescription `
            -BinaryPathName $binaryPath `
            -StartupType Automatic | Out-Null
        Write-Ok "已创建服务 '$ServiceName'（自动启动, LocalSystem）。"
    }
    Show-Status
}

function Uninstall-ServiceRegistration {
    $record = Get-ServiceRecord
    if (-not $record) {
        Write-WarnLine "服务 '$ServiceName' 未安装，无需卸载。"
        return
    }
    Write-Info '停止服务...'
    Stop-ServiceTarget
    $record = Get-ServiceRecord
    if ($record) {
        $result = Invoke-CimMethod -InputObject $record -MethodName Delete
        if ($result.ReturnValue -ne 0) {
            throw "删除服务失败: Win32=$($result.ReturnValue)"
        }
    }
    Write-Ok "已卸载服务注册 '$ServiceName'（保留 dist 程序与日志）。"
    Show-Status
}

function Start-ServiceTarget {
    Assert-DistExe | Out-Null
    $service = Get-ServiceObject
    if (-not $service) {
        Write-WarnLine '服务尚未安装，先执行 Install...'
        Install-ServiceFromDist
        $service = Get-ServiceObject
        if (-not $service) { throw '安装后仍无法获取服务对象。' }
    }
    try {
        $service.Refresh()
        if ($service.Status -eq 'Running') {
            Write-Ok '服务已在运行。'
            return
        }
        Write-Info "正在启动 '$ServiceName'..."
        if ($service.Status -ne 'StartPending') { $service.Start() }
        Wait-ServiceState -Service $service -Desired 'Running'
        Write-Ok '服务已启动。'
    } finally {
        if ($service) { $service.Dispose() }
    }
    Show-Status
}

function Stop-ServiceTarget {
    $service = Get-ServiceObject
    if (-not $service) {
        Write-WarnLine "服务 '$ServiceName' 未安装。"
        return
    }
    try {
        $service.Refresh()
        if ($service.Status -eq 'Stopped') {
            Write-Ok '服务已停止。'
            return
        }
        Write-Info "正在停止 '$ServiceName'..."
        if ($service.Status -ne 'StopPending') { $service.Stop() }
        Wait-ServiceState -Service $service -Desired 'Stopped'
        Write-Ok '服务已停止。'
    } finally {
        if ($service) { $service.Dispose() }
    }
}

function Restart-ServiceTarget {
    Write-Info '重启服务...'
    Stop-ServiceTarget
    Start-ServiceTarget
}

function Invoke-Action {
    param([Parameter(Mandatory)][string]$Name)
    switch ($Name) {
        'Status' { Show-Status }
        'Install' {
            Request-Elevation -RequestedAction Install
            Install-ServiceFromDist
        }
        'Uninstall' {
            Request-Elevation -RequestedAction Uninstall
            Uninstall-ServiceRegistration
        }
        'Start' {
            Request-Elevation -RequestedAction Start
            Start-ServiceTarget
        }
        'Stop' {
            Request-Elevation -RequestedAction Stop
            Stop-ServiceTarget
            Show-Status
        }
        'Restart' {
            Request-Elevation -RequestedAction Restart
            Restart-ServiceTarget
        }
        default { throw "未知操作: $Name" }
    }
}

function Show-Menu {
    $exe = Get-DistServiceExe
    $admin = if (Test-IsAdministrator) { '是' } else { '否' }
    $exists = if (Test-Path -LiteralPath $exe) { '是' } else { '否' }
    Write-Host ''
    Write-Host '========================================' -ForegroundColor White
    Write-Host ' AxonkeyService dist 验证工具' -ForegroundColor White
    Write-Host '========================================' -ForegroundColor White
    Write-Host (" EXE     : {0}" -f $exe)
    Write-Host (" 存在    : {0}" -f $exists)
    Write-Host (" 服务名  : {0}" -f $ServiceName)
    Write-Host (" 管理员  : {0}" -f $admin)
    Write-Host '----------------------------------------'
    Write-Host '  1) 状态查询 (Status)'
    Write-Host '  2) 安装     (Install)   *需管理员'
    Write-Host '  3) 启动     (Start)     *需管理员'
    Write-Host '  4) 停止     (Stop)      *需管理员'
    Write-Host '  5) 重启     (Restart)   *需管理员'
    Write-Host '  6) 卸载     (Uninstall) *需管理员'
    Write-Host '  0) 退出'
    Write-Host '========================================'
}

function Start-Interactive {
    while ($true) {
        Show-Menu
        $choice = Read-Host '请选择操作编号'
        try {
            switch ($choice.Trim()) {
                '1' { Invoke-Action -Name Status }
                '2' { Invoke-Action -Name Install }
                '3' { Invoke-Action -Name Start }
                '4' { Invoke-Action -Name Stop }
                '5' { Invoke-Action -Name Restart }
                '6' { Invoke-Action -Name Uninstall }
                '0' { Write-Info '已退出。'; return }
                ''  { continue }
                default { Write-WarnLine "无效选项: $choice"; continue }
            }
        } catch {
            Write-ErrLine $_.Exception.Message
            if ($_.InvocationInfo -and $_.InvocationInfo.PositionMessage) {
                Write-Host $_.InvocationInfo.PositionMessage -ForegroundColor DarkRed
            }
        }
        Write-Host ''
        if ([Console]::IsInputRedirected) {
            # Non-interactive pipe: do not block on pause.
            continue
        }
        [void](Read-Host '按 Enter 返回菜单')
    }
}

try {
    if ($Action) {
        Invoke-Action -Name $Action
        exit 0
    }
    Start-Interactive
    exit 0
} catch {
    Write-ErrLine $_.Exception.Message
    exit 1
}