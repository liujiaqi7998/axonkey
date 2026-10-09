[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Install', 'Uninstall', 'Upgrade')]
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

function Start-AxonkeyService {
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

function Wait-AxonkeyServicePipe {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ([DateTime]::UtcNow -lt $deadline) {
        $pipe = $null
        try {
            $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
                '.',
                'AxonkeyService.v1',
                [System.IO.Pipes.PipeDirection]::InOut,
                [System.IO.Pipes.PipeOptions]::None
            )
            $pipe.Connect(1000)
            if ($pipe.IsConnected) { return }
        } catch {
            Start-Sleep -Milliseconds 250
        } finally {
            if ($pipe) { $pipe.Dispose() }
        }
    }
    throw 'AxonkeyService is running but its RPC pipe did not become available.'
}

function Wait-AxonkeyServiceDeleted {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while (Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop) {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Timed out waiting for AxonkeyService removal.' }
        Start-Sleep -Milliseconds 250
    }
}

function Write-ServiceLog([string]$message) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $logPath) -Force | Out-Null
    Add-Content -LiteralPath $logPath -Value "$(Get-Date -Format o) $message" -Encoding UTF8
}

function Install-AxonkeyService {
    if (-not $ServiceExecutable -or -not (Test-Path -LiteralPath $ServiceExecutable -PathType Leaf)) {
        throw 'The bundled AxonkeyService.exe was not found.'
    }
    $serviceDirectory = Join-Path $env:ProgramData 'Axonkey\service'
    $destination = Join-Path $serviceDirectory 'AxonkeyService.exe'
    $source = [System.IO.Path]::GetFullPath($ServiceExecutable)
    $record = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
    if ($record) { Stop-AxonkeyService }
    New-Item -ItemType Directory -Path $serviceDirectory -Force | Out-Null
    $buildTime = (Get-Item -LiteralPath $source -ErrorAction Stop).LastWriteTimeUtc
    Copy-Item -LiteralPath $source -Destination $destination -Force
    (Get-Item -LiteralPath $destination -ErrorAction Stop).LastWriteTimeUtc = $buildTime
    Write-ServiceLog "Service binary copied to $destination"
    $binaryPath = '"{0}"' -f ([System.IO.Path]::GetFullPath($destination))
    if ($record) {
        $result = Invoke-CimMethod -InputObject $record -MethodName Change -Arguments @{
            PathName  = $binaryPath
            StartMode = 'Automatic'
            StartName = 'LocalSystem'
        }
        if ($result.ReturnValue -ne 0) { throw "Service configuration update failed: Win32=$($result.ReturnValue)" }
    } else {
        New-Service -Name $serviceName -DisplayName 'Axonkey Service' `
            -Description 'RC003 HID and voice service for Axonkey.' `
            -BinaryPathName $binaryPath -StartupType Automatic | Out-Null
    }
    Set-Service -Name $serviceName -StartupType Automatic
    $installed = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
    if (-not $installed) { throw 'AxonkeyService registration disappeared after installation.' }
    $registeredPath = $installed.PathName.Trim().Trim('"')
    if ([System.IO.Path]::GetFullPath($registeredPath) -ine [System.IO.Path]::GetFullPath($destination)) {
        throw "AxonkeyService BinPath was not updated: $($installed.PathName)"
    }
    if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
        throw "AxonkeyService was not copied to $destination"
    }
    Start-AxonkeyService
    Wait-AxonkeyServicePipe
    Write-ServiceLog "Service registered at $binaryPath; RPC pipe ready"
}

function Uninstall-AxonkeyService {
    Stop-AxonkeyService
    $record = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
    if ($record) {
        $result = Invoke-CimMethod -InputObject $record -MethodName Delete
        if ($result.ReturnValue -ne 0) { throw "Service removal failed: Win32=$($result.ReturnValue)" }
        Wait-AxonkeyServiceDeleted
    }
    $serviceDirectory = Join-Path $env:ProgramData 'Axonkey\service'
    if (Test-Path -LiteralPath $serviceDirectory -PathType Container) {
        Remove-Item -LiteralPath $serviceDirectory -Recurse -Force
    }
}

try {
    if (-not (Test-IsAdministrator)) { Request-Elevation }
    Write-ServiceLog "$Action requested"

    switch ($Action) {
        'Install' { Install-AxonkeyService }
        'Uninstall' { Uninstall-AxonkeyService }
        'Upgrade' {
            Uninstall-AxonkeyService
            Install-AxonkeyService
        }
    }
    Write-ServiceLog "$Action completed"
    exit 0
} catch {
    try { Write-ServiceLog "ERROR: $($_.Exception.ToString())" } catch {}
    exit 1
}
