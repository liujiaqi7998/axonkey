# Axonkey service protocol

`axonkey_service.proto` is the wire contract between `AxonkeyService.exe` and
the desktop application. The transport is a local Windows named pipe named
`\\.\pipe\AxonkeyService.v1`. Every message is framed as a four byte little
endian payload length followed by a protobuf message.

The repository keeps the small wire codec used by the native service in
`axonkey_rpc.h/.cpp`. It is intentionally dependency free so the signed
Windows service does not need a global protobuf runtime. The codec follows the
proto3 wire format and is covered by the service build; generated clients can
use the schema directly.
