import { copyFileSync, existsSync, mkdirSync, readFileSync, statSync, utimesSync } from 'node:fs'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

// Rust service build shared by the default packaging entry and isolated builds.
if (process.platform !== 'win32') throw new Error('The Rust service requires Windows x64 with MSVC and the Windows SDK.')
const root = fileURLToPath(new URL('../', import.meta.url))
const crate = join(root, 'windows/service/rust')
const target = 'x86_64-pc-windows-msvc'
const pinned = readFileSync(join(crate, 'rust-toolchain.toml'), 'utf8').match(/^channel = "([^"]+)"/m)?.[1]
if (!pinned) throw new Error('Missing pinned service Rust toolchain')
const args = new Set(process.argv.slice(2))
for (const arg of args) if (!['--test', '--stage'].includes(arg)) throw new Error(`Unknown option: ${arg}`)

const env = { ...process.env, CARGO_TARGET_DIR: join(crate, 'target'), CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS: '-C target-feature=+crt-static' }
// Prevent inherited global flags from silently overriding the static CRT.
delete env.RUSTFLAGS
delete env.CARGO_ENCODED_RUSTFLAGS
// A toolchain alias is acceptable only when its compiler exactly matches the pin.
// This supports machines whose pinned release is installed under "stable".
const version = spawnSync('rustc', ['--version'], { cwd: root, env, encoding: 'utf8', windowsHide: true })
const cargoPrefix = !version.error && version.status === 0 && version.stdout.startsWith(`rustc ${pinned} `) ? [] : [`+${pinned}`]
function run(command, argv) {
  const result = spawnSync(command, argv, { cwd: root, env, stdio: 'inherit', windowsHide: true })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`${command} ${argv.join(' ')} failed (${result.status})`)
}
function cargo(command, extra = []) {
  run('cargo', [...cargoPrefix, command, '--locked', '--manifest-path', join(crate, 'Cargo.toml'), '--target', target, ...extra])
}
function copyArtifact(source, destination) {
  copyFileSync(source, destination)
  const info = statSync(source)
  utimesSync(destination, info.atime, info.mtime)
}

// Read the PE import table directly: verify x64 and reject missing VC/MinGW CRT
// deployment requirements without assuming dumpbin is on the caller's PATH.
function verifyImports(path) {
  const pe = readFileSync(path)
  const nt = pe.readUInt32LE(0x3c)
  if (pe.readUInt32LE(nt) !== 0x4550 || pe.readUInt16LE(nt + 4) !== 0x8664) throw new Error('Expected an x64 PE executable')
  const optional = nt + 24
  if (pe.readUInt16LE(optional) !== 0x20b) throw new Error('Expected PE32+')
  const sections = optional + pe.readUInt16LE(nt + 20)
  function offset(rva) {
    for (let i = 0; i < pe.readUInt16LE(nt + 6); i++) {
      const section = sections + i * 40
      const start = pe.readUInt32LE(section + 12)
      const size = Math.max(pe.readUInt32LE(section + 8), pe.readUInt32LE(section + 16))
      if (rva >= start && rva < start + size) return pe.readUInt32LE(section + 20) + rva - start
    }
    throw new Error(`Invalid import RVA ${rva}`)
  }
  const imports = []
  let descriptor = offset(pe.readUInt32LE(optional + 120))
  while (pe.readUInt32LE(descriptor + 12)) {
    const name = offset(pe.readUInt32LE(descriptor + 12))
    const end = pe.indexOf(0, name)
    if (end < 0) throw new Error('Unterminated DLL name')
    imports.push(pe.toString('ascii', name, end))
    descriptor += 20
  }
  if (imports.some(name => /^(vcruntime|msvcp|msvcr|libgcc|libstdc\+\+)/i.test(name))) throw new Error(`Unexpected dynamic compiler runtime: ${imports.join(', ')}`)
  console.log(`Verified x64/static compiler runtime; imports: ${imports.join(', ')}`)
}

try {
  if (args.has('--test')) {
    run('cargo', [...cargoPrefix, 'fmt', '--manifest-path', join(crate, 'Cargo.toml'), '--', '--check'])
    cargo('clippy', ['--all-targets', '--', '-D', 'warnings'])
    cargo('test')
    cargo('test', ['--release'])
  }
  // Only the small C/SEH boundary remains native; service logic is Rust.
  cargo('build', ['--release', '--bin', 'AxonkeyService'])
  const output = join(crate, 'target', target, 'release')
  const executable = join(output, 'AxonkeyService.exe')
  const symbols = join(output, 'AxonkeyService.pdb')
  if (!existsSync(symbols)) throw new Error('Missing matching PDB')
  verifyImports(executable)
  run(executable, ['--check'])
  const destination = join(root, '.build/service-rust/dist')
  mkdirSync(destination, { recursive: true })
  for (const name of ['AxonkeyService.exe', 'AxonkeyService.pdb']) copyArtifact(join(output, name), join(destination, name))
  console.log(`Rust service artifacts: ${destination}`)
  if (args.has('--stage')) {
    const staging = join(root, 'windows/service/dist')
    mkdirSync(staging, { recursive: true })
    for (const name of ['AxonkeyService.exe', 'AxonkeyService.pdb']) copyArtifact(join(output, name), join(staging, name))
    console.log('Staged Rust service for existing Tauri/install resource paths; no installed service changed.')
  }
} catch (error) {
  console.error(`Rust service build failed: ${error instanceof Error ? error.message : String(error)}`)
  process.exitCode = 1
}
