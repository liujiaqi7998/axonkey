# Third-party notices

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
