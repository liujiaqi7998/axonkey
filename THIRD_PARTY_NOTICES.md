# Third-party notices

## Windows Rust service

The independent `windows/service/rust` crate preserves the RC003 attribution
below. Its exact dependency graph is recorded in its own `Cargo.lock`.
Direct dependencies declare the following licenses in their package metadata:

| Package | Version | License | Source |
| --- | --- | --- | --- |
| tokio | 1.53.2 | MIT | https://github.com/tokio-rs/tokio |
| prost / prost-build | 0.13.5 | Apache-2.0 | https://github.com/tokio-rs/prost |
| windows | 0.61.3 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-future | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| winreg | 0.55.0 | MIT | https://github.com/gentoo90/winreg-rs |
| log | 0.4.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/log |
| cc (build only) | 1.6.0 | MIT OR Apache-2.0 | https://github.com/rust-lang/cc-rs |
| sha2 (tests only) | 0.10.9 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| protoc-bin-vendored (build only) | 3.3.0 | MIT | https://github.com/stepancheg/rust-protoc-bin-vendored/ |

The vendored protoc executable is a build tool and is not shipped inside the
service executable. Upstream license texts remain in the corresponding Cargo
packages. This direct-dependency inventory does not replace collecting the
complete transitive license texts for the eventual release package.

## remote-bridge-hub

- Project: `xxb26553663-star/remote-bridge-hub`
- Source: https://github.com/xxb26553663-star/remote-bridge-hub
- Reference revision: `8a93f321ac71a602300c6cd77f7256fa4b63068e`
- License: GNU General Public License v3.0 only (`GPL-3.0-only`)

The Xiaomi RC003 ATVV UUIDs, microphone commands, capability parsing, and
IMA/DVI ADPCM decoding order were adapted from this project. Axonkey implements
the transport with native platform frameworks.

## BlackHole

- Project: `ExistentialAudio/BlackHole`
- Source: https://github.com/ExistentialAudio/BlackHole
- Pinned source: `v0.7.1` / `e2b22aaaba4e507a097131704bf96dabc004d9cf`
- License: GNU General Public License v3.0 (`GPL-3.0`)

Axonkey builds the separately identified `MiRemoteV2ch.driver` from this pinned
source. `third_party/blackhole/blackhole-device-usb.patch` changes the reported
Core Audio transport to USB and assigns an independent CFPlugIn factory UUID.
The build settings use bundle identifier `com.hd838a.MiRemoteV2ch`, device UID
`MiRemoteV2ch_UID`, and two channels. The driver coexists with BlackHole and is
distributed as a separate macOS Installer component inside Axonkey.app.

The exact build recipe and corresponding-source pointer are retained at
`third_party/blackhole/README.md`.
