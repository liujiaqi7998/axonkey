[CmdletBinding()]
param(
    [switch]$BuildOnly,
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$sourcePaths = @(
    (Join-Path $repoRoot 'tools\keycode-demo\KeycodeDemo.cs'),
    (Join-Path $repoRoot 'tools\keycode-demo\InterceptionCapture.cs')
)
$fingerprintPaths = @($sourcePaths)
$sourceFingerprint = ($fingerprintPaths | ForEach-Object { (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash }) -join ''
$fingerprintHasher = [System.Security.Cryptography.SHA256]::Create()
try { $buildId = ([BitConverter]::ToString($fingerprintHasher.ComputeHash([Text.Encoding]::UTF8.GetBytes($sourceFingerprint)))).Replace('-', '').Substring(0, 12).ToLowerInvariant() }
finally { $fingerprintHasher.Dispose() }
# Separate revisions so an open diagnostic window never prevents compiling a fix.
$outputDirectory = Join-Path $repoRoot ".build\keycode-demo\$buildId"
$executablePath = Join-Path $outputDirectory 'Axonkey-KeycodeDemo.exe'
$compilerPath = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (-not (Test-Path -LiteralPath $compilerPath)) {
    throw 'This demo requires 64-bit Windows with .NET Framework 4.x.'
}

New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
if (-not (Test-Path -LiteralPath $executablePath)) {
    & $compilerPath /nologo /target:winexe /platform:x64 /optimize+ /codepage:65001 `
        /reference:System.Windows.Forms.dll /reference:System.Drawing.dll `
        "/out:$executablePath" $sourcePaths
    if ($LASTEXITCODE -ne 0) { throw 'Keycode demo compilation failed.' }
}
$runtimePath = Join-Path $repoRoot 'vendor\interception\interception.dll'
if ((Get-FileHash -LiteralPath $runtimePath -Algorithm SHA256).Hash -ne 'AB88164C11B1B48488772D4C3BFAA4509D5B0AE9DBC5A691DC4F96F0260443C8') {
    throw 'The bundled Interception runtime hash does not match the reviewed version.'
}
$runtimeCopy = Join-Path $outputDirectory 'interception.dll'
if (-not (Test-Path -LiteralPath $runtimeCopy)) { Copy-Item -LiteralPath $runtimePath -Destination $runtimeCopy }
Write-Output "Built: $executablePath"

if ($SelfTest) {
    $testProcess = Start-Process -FilePath $executablePath -ArgumentList '--self-test' -WindowStyle Hidden -Wait -PassThru
    Get-Content -LiteralPath (Join-Path $outputDirectory 'self-test.txt') -Encoding UTF8
    if ($testProcess.ExitCode -ne 0) { throw "Keycode demo self-test failed ($($testProcess.ExitCode))." }
} elseif (-not $BuildOnly) {
    Start-Process -FilePath $executablePath
}
