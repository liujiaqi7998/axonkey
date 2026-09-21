import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

// The service is a Windows-only native component. Keeping this as a no-op on
// other hosts lets the shared Tauri config continue to build macOS previews.
if (process.platform !== 'win32') process.exit(0)

const root = fileURLToPath(new URL('../', import.meta.url))

function run(command, args, env = process.env) {
  const result = spawnSync(command, args, {
    cwd: root,
    env,
    stdio: 'inherit',
    windowsHide: true,
  })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed with exit code ${result.status}`)
}

function msvcEnvironment() {
  const programFiles = process.env['ProgramFiles(x86)'] || 'C:/Program Files (x86)'
  const vswhere = join(programFiles, 'Microsoft Visual Studio/Installer/vswhere.exe')
  const installation = spawnSync(vswhere, [
    '-latest', '-products', '*',
    '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
    '-property', 'installationPath',
  ], { cwd: root, encoding: 'utf8', windowsHide: true })
  if (installation.error || installation.status !== 0 || !installation.stdout.trim()) {
    throw new Error('Building AxonkeyService requires Visual Studio C++ build tools and the Windows SDK.')
  }

  const devCommand = join(installation.stdout.trim(), 'Common7/Tools/VsDevCmd.bat')
  const environment = spawnSync(process.env.ComSpec || 'cmd.exe', [
    '/d', '/s', '/c', `""${devCommand}" -no_logo -arch=x64 -host_arch=x64 >nul && set"`,
  ], { cwd: root, encoding: 'utf8', windowsHide: true, windowsVerbatimArguments: true })
  if (environment.error || environment.status !== 0) throw new Error('Cannot initialize the MSVC build environment.')

  const env = { ...process.env, VSLANG: '1033' }
  for (const line of environment.stdout.split(/\r?\n/)) {
    const separator = line.indexOf('=')
    if (separator > 0) env[line.slice(0, separator)] = line.slice(separator + 1)
  }
  return env
}

try {
  const env = msvcEnvironment()
  // Keep the configure command aligned with the documented Windows service
  // build: cmake -S windows/service -B .build/service -G Ninja -DCMAKE_BUILD_TYPE=Release
  // CMake writes the Release executable into windows/service/dist.
  run('cmake', ['-S', 'windows/service', '-B', '.build/service', '-G', 'Ninja', '-DCMAKE_BUILD_TYPE=Release'], env)
  run('cmake', ['--build', '.build/service', '--config', 'Release', '--target', 'AxonkeyService'], env)
  const executable = join(root, 'windows/service/dist/AxonkeyService.exe')
  if (!existsSync(executable)) throw new Error(`The service build did not produce ${executable}.`)
} catch (error) {
  console.error(`Windows service build failed: ${error instanceof Error ? error.message : String(error)}`)
  process.exitCode = 1
}
