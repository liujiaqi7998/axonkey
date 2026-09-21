[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Install', 'Uninstall', 'Start', 'Stop')]
    [string]$Action,
    [string]$ServiceExecutable
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$serviceName = 'AxonkeyService'
$logPath = Join-Path $env:ProgramData 'Axonkey\Logs\ServiceManagement.log'

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Request-Elevation {
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"{0}"' -f $PSCommandPath), '-Action', $Action)
    if ($ServiceExecutable) { $arguments += @('-ServiceExecutable', ('"{0}"' -f $ServiceExecutable)) }
    $process = Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -ArgumentList $arguments -Wait -PassThru
    exit $process.ExitCode
}

function Get-AxonkeyService {
    try { return Get-Service -Name $serviceName -ErrorAction Stop } catch { return $null }
}

function Stop-AxonkeyService {
    $service = Get-AxonkeyService
    if (-not $service) { return }
    try {
        $service.Refresh()
        if ($service.Status -ne 'Stopped') {
            if ($service.Status -ne 'StopPending') { $service.Stop() }
            $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        }
    } finally { $service.Dispose() }
}

function Write-ServiceLog([string]$message) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $logPath) -Force | Out-Null
    Add-Content -LiteralPath $logPath -Value "$(Get-Date -Format o) $message" -Encoding UTF8
}

try {
    if (-not (Test-IsAdministrator)) { Request-Elevation }
    Write-ServiceLog "$Action requested"

    switch ($Action) {
        'Install' {
            if (-not $ServiceExecutable -or -not (Test-Path -LiteralPath $ServiceExecutable -PathType Leaf)) {
                throw 'The bundled AxonkeyService.exe was not found.'
            }
            $programFilesRoot = $env:ProgramW6432
            if (-not $programFilesRoot) { $programFilesRoot = $env:ProgramFiles }
            $directory = Join-Path $programFilesRoot 'Axonkey\Service'
            New-Item -ItemType Directory -Path $directory -Force | Out-Null
            $destination = Join-Path $directory 'AxonkeyService.exe'
            $staged = Join-Path $directory 'AxonkeyService.new.exe'
            Copy-Item -LiteralPath $ServiceExecutable -Destination $staged -Force
            Stop-AxonkeyService
            Move-Item -LiteralPath $staged -Destination $destination -Force
            $binaryPath = '"{0}"' -f $destination
            $record = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
            if ($record) {
                $result = Invoke-CimMethod -InputObject $record -MethodName Change -Arguments @{
                    PathName = $binaryPath; StartMode = 'Automatic'; StartName = 'LocalSystem'
                }
                if ($result.ReturnValue -ne 0) { throw "Service configuration failed: Win32=$($result.ReturnValue)" }
            } else {
                New-Service -Name $serviceName -DisplayName 'Axonkey Service' `
                    -Description 'RC003 HID and voice service for Axonkey.' `
                    -BinaryPathName $binaryPath -StartupType Automatic | Out-Null
            }
        }
        'Start' {
            $service = Get-AxonkeyService
            if (-not $service) { throw 'Install AxonkeyService before starting it.' }
            try {
                $service.Refresh()
                if ($service.Status -ne 'Running') {
                    if ($service.Status -ne 'StartPending') { $service.Start() }
                    $service.WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
                }
            } finally { $service.Dispose() }
        }
        'Stop' { Stop-AxonkeyService }
        'Uninstall' {
            Stop-AxonkeyService
            $record = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
            if ($record) {
                $result = Invoke-CimMethod -InputObject $record -MethodName Delete
                if ($result.ReturnValue -ne 0) { throw "Service removal failed: Win32=$($result.ReturnValue)" }
            }
            $programFilesRoot = $env:ProgramW6432
            if (-not $programFilesRoot) { $programFilesRoot = $env:ProgramFiles }
            $directory = Join-Path $programFilesRoot 'Axonkey\Service'
            if (Test-Path -LiteralPath $directory -PathType Container) {
                Remove-Item -LiteralPath $directory -Recurse -Force
            }
        }
    }
    Write-ServiceLog "$Action completed"
    exit 0
} catch {
    try { Write-ServiceLog "ERROR: $($_.Exception.ToString())" } catch {}
    exit 1
}
