import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { resolve, join } from 'node:path'
import { fileURLToPath } from 'node:url'

if (process.platform === 'win32') {
  const root = fileURLToPath(new URL('../', import.meta.url))
  const vswhere = join(process.env['ProgramFiles(x86)'] || 'C:/Program Files (x86)', 'Microsoft Visual Studio/Installer/vswhere.exe')
  const installation = spawnSync(vswhere, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'], { encoding: 'utf8', windowsHide: true })
  if (installation.error || installation.status !== 0 || !installation.stdout.trim()) throw new Error('Building AxonkeyService requires Visual Studio C++ build tools and the Windows SDK.')
  const devcmd = join(installation.stdout.trim(), 'Common7/Tools/VsDevCmd.bat')
  const environment = spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', `""${devcmd}" -no_logo -arch=x64 -host_arch=x64 >nul && set"`], { encoding: 'utf8', windowsHide: true, windowsVerbatimArguments: true })
  if (environment.error || environment.status !== 0) throw new Error('Cannot initialize the MSVC build environment.')
  const env = { ...process.env }
  for (const line of environment.stdout.split(/\r?\n/)) {
    const separator = line.indexOf('=')
    if (separator > 0) env[line.slice(0, separator)] = line.slice(separator + 1)
  }
  env.VSLANG = '1033'
  const build = resolve(root, '.build/service-bundle')
  const run = args => {
    const result = spawnSync('cmake', args, { cwd: root, env, stdio: 'inherit', windowsHide: true })
    if (result.error || result.status !== 0) throw new Error(`Service build failed: ${result.error?.message || result.status}`)
  }
  run(['-S', 'windows/service', '-B', build, '-G', 'Ninja', '-DCMAKE_CXX_COMPILER=cl', '-DCMAKE_BUILD_TYPE=Release', '-DBUILD_TESTING=OFF', '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded'])
  run(['--build', build, '--target', 'AxonkeyService'])
  if (!existsSync(join(build, 'AxonkeyService.exe'))) throw new Error('The service build did not produce AxonkeyService.exe.')
}
