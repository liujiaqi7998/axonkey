[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Install', 'Uninstall', 'Start', 'Stop')]
    [string]$Action,
    [string]$ServiceExecutable
)

# Called only through the app's elevated launcher. Never prompt or self-elevate.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$serviceName = 'AxonkeyService'
$logPath = Join-Path $env:ProgramData 'Axonkey\Logs\ServiceManagement.log'

function Get-AxonkeyService {
    # An enumeration failure must not be mistaken for an uninstalled service.
    Get-Service -ErrorAction Stop | Where-Object { $_.Name -eq $serviceName }
}

function Stop-AxonkeyService {
    $service = Get-AxonkeyService
    if (-not $service) { return }
    try {
        if ($service.Status -ne 'Stopped') {
            if ($service.Status -ne 'StopPending') { $service.Stop() }
            $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        }
    } finally { $service.Dispose() }
}

try {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Administrator privileges are required.'
    }
    New-Item -ItemType Directory -Path (Split-Path -Parent $logPath) -Force | Out-Null
    Set-Content -LiteralPath $logPath -Value "$(Get-Date -Format o) $Action requested" -Encoding UTF8

    switch ($Action) {
        'Install' {
            if (-not $ServiceExecutable -or -not (Test-Path -LiteralPath $ServiceExecutable -PathType Leaf)) {
                throw 'The bundled AxonkeyService.exe was not found.'
            }
            # LocalSystem must not run from a user-writable checkout or per-user app folder.
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
            $record = Get-CimInstance -ClassName Win32_Service -Filter "Name='AxonkeyService'" -ErrorAction Stop
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
                if ($service.Status -ne 'Running') {
                    if ($service.Status -ne 'StartPending') { $service.Start() }
                    $service.WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
                }
            } finally { $service.Dispose() }
        }
        'Stop' { Stop-AxonkeyService }
        'Uninstall' {
            Stop-AxonkeyService
            $record = Get-CimInstance -ClassName Win32_Service -Filter "Name='AxonkeyService'" -ErrorAction Stop
            if ($record) {
                $result = Invoke-CimMethod -InputObject $record -MethodName Delete
                if ($result.ReturnValue -ne 0) { throw "Service removal failed: Win32=$($result.ReturnValue)" }
            }
            # Keep the program, logs and registry settings for troubleshooting/reinstall.
        }
    }
    Add-Content -LiteralPath $logPath -Value "$(Get-Date -Format o) $Action completed" -Encoding UTF8
    exit 0
} catch {
    try { Add-Content -LiteralPath $logPath -Value "$(Get-Date -Format o) ERROR: $($_.Exception.ToString())" -Encoding UTF8 } catch {}
    exit 1
}
