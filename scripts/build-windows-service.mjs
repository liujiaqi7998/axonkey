// Shared Tauri entry: non-Windows previews have no Windows service resource.
if (process.platform !== 'win32') process.exit(0)

if (process.env.AXONKEY_SERVICE_IMPLEMENTATION && process.env.AXONKEY_SERVICE_IMPLEMENTATION !== 'rust') {
  throw new Error('The C++ service was removed. Unset AXONKEY_SERVICE_IMPLEMENTATION or set it to rust.')
}

// Keep the existing installer/resource path while using the Rust build only.
process.argv.push('--stage')
await import('./build-windows-service-rust.mjs')
