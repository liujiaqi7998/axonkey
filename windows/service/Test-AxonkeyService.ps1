[CmdletBinding()]
param(
    [ValidateSet('Install', 'Start', 'Stop', 'Restart', 'Status', 'Uninstall')]
    [string]$Action = 'Status'
)

$ErrorActionPreference = 'Stop'

Set-StrictMode -Version Latest

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Assert-Administrator {
    if (Test-IsAdministrator) { return }

    # Service changes require elevation. Re-run this exact script with the same
    # action so the user can copy it to another machine without extra steps.
    $arguments = @(
        '-NoProfile'
        '-ExecutionPolicy', 'Bypass'
        '-File', ('"' + $PSCommandPath + '"')
        '-Action', $Action
    )
    $process = Start-Process -FilePath 'powershell.exe' -Verb RunAs -ArgumentList $arguments -Wait -PassThru
    exit $process.ExitCode
}

function Get-ServiceRecord {
    return Get-CimInstance -ClassName Win32_Service -Filter "Name='AxonkeyService'" -ErrorAction SilentlyContinue
}

function Get-ServiceExecutable {
    $path = Join-Path -Path $PSScriptRoot -ChildPath 'AxonkeyService.exe'
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "找不到服务程序：$path`n请将 AxonkeyService.exe 放在本脚本相同目录。"
    }
    return (Resolve-Path -LiteralPath $path).Path
}

function Wait-ServiceState([string]$ExpectedState) {
    $service = Get-Service -Name AxonkeyService -ErrorAction Stop
    $service.WaitForStatus($ExpectedState, [TimeSpan]::FromSeconds(30))
}

function Install-AxonkeyService {
    $executable = Get-ServiceExecutable
    $binaryPath = '"{0}"' -f $executable
    $record = Get-ServiceRecord

    if (-not $record) {
        New-Service -Name AxonkeyService -DisplayName AxonkeyService `
            -Description 'RC003 HID and voice service for Axonkey.' `
            -BinaryPathName $binaryPath -StartupType Automatic | Out-Null
        Write-Host "已安装 AxonkeyService。"
        return
    }

    if ($record.State -ne 'Stopped') {
        Stop-Service -Name AxonkeyService -Force
        Wait-ServiceState 'Stopped'
    }

    if ($record.PathName.Trim('"') -ne $executable) {
        $result = Invoke-CimMethod -InputObject $record -MethodName Change -Arguments @{
            PathName = $binaryPath
            StartMode = 'Auto'
        }
        if ($result.ReturnValue -ne 0) {
            throw "更新服务程序路径失败，Win32 错误码：$($result.ReturnValue)"
        }
    } else {
        Set-Service -Name AxonkeyService -StartupType Automatic
    }
    Write-Host "AxonkeyService 已存在，已更新为：$executable"
}

function Start-AxonkeyService {
    if (-not (Get-ServiceRecord)) { Install-AxonkeyService }
    $service = Get-Service -Name AxonkeyService
    if ($service.Status -ne 'Running') {
        Start-Service -Name AxonkeyService
        Wait-ServiceState 'Running'
    }
    Write-Host 'AxonkeyService 已启动。'
}

function Stop-AxonkeyService {
    $service = Get-Service -Name AxonkeyService -ErrorAction SilentlyContinue
    if (-not $service) {
        Write-Host 'AxonkeyService 尚未安装。'
        return
    }
    if ($service.Status -ne 'Stopped') {
        Stop-Service -Name AxonkeyService -Force
        Wait-ServiceState 'Stopped'
    }
    Write-Host 'AxonkeyService 已停止。'
}

function Show-AxonkeyServiceStatus {
    $record = Get-ServiceRecord
    if (-not $record) {
        Write-Host 'AxonkeyService：未安装。'
        return
    }

    [pscustomobject]@{
        Name      = $record.Name
        State     = $record.State
        StartMode = $record.StartMode
        ProcessId = $record.ProcessId
        PathName  = $record.PathName
        Binary    = (Get-ServiceExecutable)
    } | Format-List
}

function Uninstall-AxonkeyService {
    $record = Get-ServiceRecord
    if (-not $record) {
        Write-Host 'AxonkeyService 尚未安装。'
        return
    }
    Stop-AxonkeyService
    $result = Invoke-CimMethod -InputObject $record -MethodName Delete
    if ($result.ReturnValue -ne 0) {
        throw "删除服务失败，Win32 错误码：$($result.ReturnValue)"
    }
    Write-Host 'AxonkeyService 已卸载。'
}

if ($Action -ne 'Status') { Assert-Administrator }

switch ($Action) {
    'Install'   { Install-AxonkeyService }
    'Start'     { Start-AxonkeyService }
    'Stop'      { Stop-AxonkeyService }
    'Restart'   { Stop-AxonkeyService; Start-AxonkeyService }
    'Status'    { Show-AxonkeyServiceStatus }
    'Uninstall' { Uninstall-AxonkeyService }
}
