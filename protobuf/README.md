# Axonkey service protocol

`axonkey_service.proto` is the shared wire contract between the Rust Windows
service and the desktop application. Both generate Rust types with prost during
Cargo builds. Generated code lives in Cargo OUT_DIR, not in this directory.

The old service-only C++ wrapper, nanopb generated files, generator options and
CMake tests were removed with the C++ service. The schema and wire format remain
unchanged. Historical implementations are available at Git revision
`795ecd98e867ca0403ccce404608226f259022c7`.

## Transport

The local Windows named pipe is `\\.\pipe\AxonkeyService.v1`. Every frame is a
four-byte little-endian length followed by a protobuf message, up to 1 MiB.
Request/Response carry a method/request ID and serialized payload. Events use a
stable type string and serialized payload. This is not gRPC.

The service exposes all methods declared in the schema and supports keyboard,
audio_level, voice_status and service_issue subscriptions. Missing optional
battery_level differs from a present value of zero. Negative audio gain uses
the existing int32 protobuf representation.

## Generation and tests

Edit only the shared schema when changing the protocol; both
`windows/service/rust/build.rs` and `src-tauri/build.rs` regenerate their types.
Each crate locks its prost/prost-build and vendored protoc dependencies.

From the repository root on Windows:

```powershell
npm run test:windows-service
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib service_rpc
```

The service suite covers actual named-pipe framing, subscription order, malformed
messages, slow readers and reconnects. Desktop tests retain a historical nanopb
wire fixture to check compatibility with older installed services. The optional
running-service test requires a real local service and remains explicitly ignored
during ordinary tests.
