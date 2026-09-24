#pragma once

#include "AdpcmDecoder.h"
#include "AudioGain.h"
#include "VirtualMicrophoneSink.h"
#include <functional>

namespace axonkey_service {

// Only the receiver worker calls this state machine. GATT callbacks enqueue
// packets, preserving decoder/control ordering and retaining the first packet.
class VoiceAudioSession final {
public:
    using SendCommand = std::function<void(const std::vector<std::uint8_t>&)>;
    using LevelCallback = std::function<void(float, float)>;
    using FailureCallback = std::function<void(const char*, DWORD)>;
    VoiceAudioSession(MicrophoneOutput& microphone, SendCommand send,
        std::int32_t audioGainDb = kDefaultAudioGainDb, LevelCallback level = {},
        FailureCallback failure = {})
        : microphone_(microphone), send_(std::move(send)), level_(std::move(level)),
          failure_(std::move(failure)), gain_(audioGainDb) {}
    void Control(const std::vector<std::uint8_t>& bytes);
    void Audio(const std::vector<std::uint8_t>& bytes);
    void CloseRemote();
    void SetGain(std::int32_t audioGainDb) { gain_ = AudioGain(audioGainDb); }
    bool Active() const { return active_; }
    bool MicrophoneOpen() const { return microphoneOpen_; }
    std::uint16_t ProtocolVersion() const { return protocolVersion_; }
    std::uint8_t SessionId() const { return sessionId_; }

private:
    bool Open();
    void Fail();
    void End();
    bool PushSamples(std::vector<std::int16_t> samples);
    MicrophoneOutput& microphone_;
    SendCommand send_;
    LevelCallback level_;
    FailureCallback failure_;
    AdpcmDecoder decoder_;
    AudioGain gain_;
    bool capabilities_ = false;
    bool active_ = false;
    bool microphoneOpen_ = false;
    bool rejected_ = false;
    bool ended_ = false;
    std::uint16_t protocolVersion_ = 0x0100;
    std::uint8_t sessionId_ = 0;
};

} // namespace axonkey_service
