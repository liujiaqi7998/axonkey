#include "VoiceAudioSession.h"
#include "ServiceLog.h"
#include <algorithm>
#include <cmath>

namespace axonkey_service {

bool VoiceAudioSession::PushSamples(std::vector<std::int16_t> samples) {
    if (samples.empty()) return true;
    // Apply once, after decoding/resampling and before the driver, including
    // the resampler's final samples emitted at normal voice end.
    gain_.Apply(samples);
    if (level_) {
        float peak = 0;
        double sum = 0;
        for (const auto sample : samples) {
            const auto normalized = static_cast<float>(sample) / 32768.0f;
            peak = std::max(peak, std::abs(normalized));
            sum += static_cast<double>(normalized) * normalized;
        }
        level_(peak, static_cast<float>(std::sqrt(sum / samples.size())));
    }
    if (!microphone_.Push(samples)) {
        if (failure_) failure_("virtual_microphone_write_failed", microphone_.LastError());
        return false;
    }
    return true;
}

bool VoiceAudioSession::Open() {
    if (rejected_) return false;
    if (microphoneOpen_) return true;
    if (!microphone_.Start()) {
        // Never retry on every AUDIO notification: a second remote must not
        // take over halfway through its utterance when the first owner stops.
        rejected_ = true;
        if (failure_) failure_("virtual_microphone_unavailable", microphone_.LastError());
        LogMessage(L"RC003 voice session rejected: virtual microphone unavailable; discard until next voice session", LogLevel::Warning);
        return false;
    }
    microphoneOpen_ = true;
    return true;
}

void VoiceAudioSession::Fail() {
    rejected_ = true;
    microphoneOpen_ = false;
    microphone_.Stop();
    LogMessage(L"RC003 voice output failed; remaining audio for this session will be discarded", LogLevel::Error);
}

void VoiceAudioSession::End() {
    if (microphoneOpen_) {
        if (!PushSamples(decoder_.Flush())) Fail();
        else microphone_.Stop(true);
    }
    active_ = microphoneOpen_ = rejected_ = false;
    ended_ = true;
    decoder_.Reset();
    LogMessage(L"RC003 voice session ended");
}

void VoiceAudioSession::Control(const std::vector<std::uint8_t>& bytes) {
    if (bytes.empty()) return;
    switch (bytes[0]) {
    case 0x0b: {
        if (bytes.size() < 7) return;
        protocolVersion_ = static_cast<std::uint16_t>((bytes[1] << 8) | bytes[2]);
        auto codecs = bytes[3];
        if (protocolVersion_ >= 0x0100 && codecs == 0) codecs = bytes[4];
        capabilities_ = (codecs & 0x02) != 0;
        const auto frameBytes = static_cast<size_t>((bytes[5] << 8) | bytes[6]);
        decoder_.SetFrameBytes(frameBytes ? frameBytes : 120);
        LogMessage(capabilities_ ? L"RC003 capabilities: 16 kHz ADPCM ready" :
            L"RC003 capabilities: unsupported codec (16 kHz ADPCM required)", capabilities_ ? LogLevel::Info : LogLevel::Warning);
        break;
    }
    case 0x08:
        if (!capabilities_ || (active_ && !rejected_)) return;
        // A fresh button request can follow a rejection without AUDIO_STOP,
        // since the remote never received an acknowledgement to start audio.
        active_ = true;
        ended_ = rejected_ = false;
        sessionId_ = 0;
        decoder_.Reset();
        if (Open()) {
            if (protocolVersion_ >= 0x0100) send_({0x0c, 0x00});
            else send_({0x0c, 0x00, 0x02});
        }
        break;
    case 0x04:
        if (!capabilities_ || (bytes.size() >= 3 && bytes[2] != 0x02)) return;
        // An AUDIO_START following a rejected REQUEST_START belongs to the
        // same rejected session; it must not reopen the driver's producer.
        if (!active_) rejected_ = false;
        active_ = true;
        ended_ = false;
        sessionId_ = bytes.size() >= 4 ? bytes[3] : 0;
        decoder_.Reset();
        if (microphoneOpen_) {
            if (!microphone_.Reset()) {
                if (failure_) failure_("virtual_microphone_reset_failed", microphone_.LastError());
                Fail();
            }
        } else Open();
        break;
    case 0x0a:
        if (bytes.size() >= 7 && !rejected_)
            decoder_.Synchronize(static_cast<std::int16_t>((bytes[4] << 8) | bytes[5]), bytes[6]);
        break;
    case 0x00:
        End();
        break;
    default:
        break;
    }
}

void VoiceAudioSession::Audio(const std::vector<std::uint8_t>& bytes) {
    if (bytes.empty() || !capabilities_ || rejected_ || ended_) return;
    active_ = true;
    // Opening is synchronous on the worker, not on a GATT callback. Decode
    // this same packet after opening instead of discarding its ADPCM state.
    if (!Open()) return;
    if (!PushSamples(decoder_.Append(bytes))) Fail();
}

void VoiceAudioSession::CloseRemote() {
    if (!active_) return;
    if (protocolVersion_ >= 0x0100) send_({0x0d, sessionId_});
    else send_({0x0d});
}

} // namespace axonkey_service
