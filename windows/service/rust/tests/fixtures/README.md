# Audio compatibility baseline

`audio.sha256` contains 107 immutable SHA-256 digests captured from the original
C++ service at Git revision `795ecd98e867ca0403ccce404608226f259022c7`, on Windows
x64 with MSVC 14.51, on 2026-10-08. Capture ran the C++ `AdpcmDecoder`, `AudioGain`
and `VoiceAudioSession` through the temporary C ABI adapter, while asserting
every Rust PCM sample, session state, command, issue and level matched. All
three differential tests passed before the C++ implementation was removed.

The input sequences remain in `../audio_regression.rs`; do not regenerate the
expected digests from the implementation under test.

- Decoder: 6 frame sizes × 6 packet sizes, reset/sync and two flush operations.
  Append each operation's PCM output to its trace, including empty outputs from
  frame-size/reset/sync calls. Each PCM block is a little-endian u64 sample count
  followed by each little-endian i16 sample.
- Gain: all 65,536 PCM16 values in ascending order, for each gain from -30 to 30
  and invalid gains -100/100. Hash one PCM block as above per gain.
- Session: old/new protocol versions × gains -30/0/2/30. Each operation contributes
  14 little-endian u32 values, then a PCM block: active, microphone open, protocol
  version, session ID, cumulative start/reset/push/stop/drain counts, issue code,
  packed command bytes, command length, IEEE-754 peak bits and RMS bits. Issue
  codes are 0=none, 1=open, 2=reset, 3=write failure. Input scripts include all
  three injected microphone failures, retries, close, late audio and malformed
  control packets.

Future deliberate protocol changes should add separately reviewed expectations
and document why they differ from this historical baseline. To re-run the old
implementation, retrieve the C++ source from the recorded Git revision in a
separate checkout; the active service build no longer requires C++ sources.
