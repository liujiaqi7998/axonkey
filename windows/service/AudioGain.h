#pragma once

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <span>

namespace axonkey_service {

inline constexpr std::int32_t kDefaultAudioGainDb = 2;
inline constexpr std::int32_t kMinAudioGainDb = -30;
inline constexpr std::int32_t kMaxAudioGainDb = 30;

class AudioGain final {
public:
    explicit AudioGain(std::int32_t decibels = kDefaultAudioGainDb)
        : multiplier_(std::pow(10.0, static_cast<double>(
            decibels >= kMinAudioGainDb && decibels <= kMaxAudioGainDb
                ? decibels : kDefaultAudioGainDb) / 20.0)) {}

    void Apply(std::span<std::int16_t> samples) const {
        if (multiplier_ == 1.0) return;
        for (auto& sample : samples) {
            // Saturate before converting back to PCM16: amplification must never
            // wrap a positive peak into a negative sample (or vice versa).
            const auto amplified = std::clamp(static_cast<double>(sample) * multiplier_, -32768.0, 32767.0);
            sample = static_cast<std::int16_t>(std::lround(amplified));
        }
    }

private:
    double multiplier_;
};

} // namespace axonkey_service
