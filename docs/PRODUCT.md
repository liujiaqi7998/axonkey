# Axonkey Product Scope

## Purpose

Axonkey gives one specific physical product, the Xiaomi RC003 Bluetooth
remote, a predictable button-mapping experience on Windows and macOS. It is a focused
local utility rather than a general keyboard automation platform.

## Current features

- Recognize only HID devices with Xiaomi vendor `0x2717` and product `0x32B8`.
- Keep ordinary keyboard scan codes outside RC003 mappings.
- Show editable RC003 buttons on Windows and macOS according to each platform's
  native input capabilities.
- Configure click, double-click, and long-press actions independently.
- Map buttons to keys, modifier keys, shortcuts, media controls, pasted text,
  or a sequence of actions and delays.
- Let the user preserve the original event or disable the button.
- Offer a curated "input text and press Enter" behavior, implemented as paste,
  a 30 ms wait, and Enter.
- Apply saved changes immediately without a reboot.
- Keep running in the Windows notification area or macOS menu bar when the
  main window is closed.
- Import and export mappings as JSON, restore defaults, and undo or redo edits.
- Store all settings and diagnostics locally.
- Guide Windows users through the Quarbor HID and virtual sound-card installer, and
  macOS users through Input Monitoring, Accessibility, and optional MiRemoteV
  2ch virtual-microphone setup.
- Keep Windows voice transport in AxonkeyService; the client does not connect to
  Windows Bluetooth audio or CPAL/CABLE output. macOS forwards voice to MiRemoteV
  2ch and exposes live audio levels.
- Adjust macOS voice gain from -30 dB to +30 dB and inspect live audio levels.
- Show device connection, battery, permissions, and driver status, with setup
  actions and access to local runtime logs.

Windows only exposes keys that Interception translates into RC003 scan codes.
macOS uses its native HID backend and can expose the full RC003 button set,
including the raw Back and Volume +/- usages.

## Defaults

| RC003 button | Default behavior |
| --- | --- |
| Voice / F5 | Right Alt |
| Power / extended `0x015E` | Escape |
| Back, Volume +/-, Home, TV, Menu, Enter and directions | Preserve original key |

## Usability rules

- The home page shows input readiness, RC003 connection and battery, audio
  status, and whether custom mappings are enabled. The mapping page provides
  per-button editing.
- Editing a mapping starts from a tabbed, directly visible list of common
  behaviors. Key and shortcut capture share one entry, and no extra add
  confirmation is required.
- Invalid custom shortcuts cannot be saved.
- Missing devices, missing platform drivers and missing macOS permissions have
  different messages and remedies.
- A mapping failure must not leave a replacement modifier held down.
- Closing the main window keeps mappings active; disabling custom mappings or
  quitting the application releases input capture.

## Outside the supported scope

- Other remote or keyboard models.
- Cloud accounts, configuration sync, telemetry, or remote control over a
  network.
- Linux support.
- User-authored macro scripts, application-specific profiles, or a general
  automation editor. The mapping editor supports sequences of the built-in
  behavior types.
