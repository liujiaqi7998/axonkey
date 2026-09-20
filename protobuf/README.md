# Axonkey service protocol

`axonkey_service.proto` is the wire contract between `AxonkeyService.exe` and
the desktop application. The transport is a local Windows named pipe named
`\\.\pipe\AxonkeyService.v1`. Every message is framed as a four byte little
endian payload length followed by a protobuf message.

## Codec

The Windows service uses [nanopb](https://github.com/nanopb/nanopb) (pinned to
0.4.9 via CMake `FetchContent`) as the protobuf encode/decode runtime.

| File | Role |
| --- | --- |
| `axonkey_service.proto` | Schema (includes pipe `Request` / `Response` envelopes) |
| `axonkey_service.options` | nanopb field options (callbacks for variable strings/bytes) |
| `generated/axonkey_service.pb.c/.h` | Committed nanopb output (regenerate when the schema changes) |
| `axonkey_rpc.h/.cpp` | C++ API used by the service (`Parse` / `Serialize`) |

The C++ surface keeps stable `axonkey::rpc::*` types so `RpcServer` and the rest
of the service do not depend on generated C structs directly.

## Regenerating nanopb sources

Requires `protoc` and nanopb 0.4.9's generator:

```powershell
$np = "<path-to-nanopb-0.4.9>"
$env:PYTHONPATH = "$np\generator;$np\generator\proto"
protoc --plugin=protoc-gen-nanopb="$np\generator\protoc-gen-nanopb.bat" `
  -I protobuf -I "$np\generator\proto" `
  --nanopb_out=protobuf/generated `
  protobuf/axonkey_service.proto
```

## Tests

```powershell
cmake -S windows/service -B .build/service
cmake --build .build/service --config Release
ctest --test-dir .build/service -C Release --output-on-failure
```
