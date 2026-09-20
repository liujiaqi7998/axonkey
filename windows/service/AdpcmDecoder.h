#pragma once

#include <algorithm>
#include <array>
#include <cstdint>
#include <optional>
#include <utility>
#include <vector>

namespace axonkey_service {

class AdpcmDecoder final {
public:
    void Reset() {
        pending_.clear();
        resamplePending_.reset();
        predictor_ = 0;
        stepIndex_ = 0;
        pendingSync_.reset();
    }

    // Hold the final sample through the last interpolation interval at voice end.
    std::vector<std::int16_t> Flush() {
        std::vector<std::int16_t> tail;
        if (resamplePending_) tail.assign(3, *resamplePending_);
        resamplePending_.reset();
        return tail;
    }

    void SetFrameBytes(size_t bytes) { frameBytes_ = std::max<size_t>(1, bytes); }

    void Synchronize(int predictor, int stepIndex) {
        pending_.clear();
        resamplePending_.reset();
        pendingSync_ = std::make_pair(std::clamp(predictor, -32768, 32767), std::clamp(stepIndex, 0, 88));
    }

    std::vector<std::int16_t> Append(const std::vector<std::uint8_t>& packet) {
        pending_.insert(pending_.end(), packet.begin(), packet.end());
        std::vector<std::int16_t> samples;
        const size_t frames = pending_.size() / frameBytes_;
        if (!frames) return samples;
        const size_t consume = frames * frameBytes_;
        for (size_t offset = 0; offset < consume; offset += frameBytes_) {
            if (pendingSync_) {
                predictor_ = pendingSync_->first;
                stepIndex_ = pendingSync_->second;
                pendingSync_.reset();
            }
            std::vector<std::int16_t> frame;
            frame.reserve(frameBytes_ * 2);
            for (size_t index = 0; index < frameBytes_; ++index) {
                const auto byte = pending_[offset + index];
                frame.push_back(DecodeNibble(byte >> 4));
                frame.push_back(DecodeNibble(byte & 0x0f));
            }
            if (frame.size() >= 3) {
                const auto source = frame;
                for (size_t index = 1; index + 1 < frame.size(); ++index)
                    frame[index] = static_cast<std::int16_t>((static_cast<int>(source[index - 1]) +
                        2 * static_cast<int>(source[index]) + static_cast<int>(source[index + 1])) >> 2);
            }
            // The original Axonkey output path linearly interpolates the 16 kHz
            // source at the 48 kHz device clock. Keep one sample as look-ahead
            // so interpolation also remains continuous across GATT packets.
            samples.reserve(samples.size() + frame.size() * 3);
            for (const auto sample : frame) {
                if (!resamplePending_) {
                    resamplePending_ = sample;
                    continue;
                }
                const auto current = *resamplePending_;
                samples.push_back(current);
                samples.push_back(Interpolate(current, sample, 1));
                samples.push_back(Interpolate(current, sample, 2));
                resamplePending_ = sample;
            }
        }
        pending_.erase(pending_.begin(), pending_.begin() + static_cast<std::ptrdiff_t>(consume));
        return samples;
    }

private:
    static std::int16_t Interpolate(std::int16_t current, std::int16_t next, int step) {
        const int value = (static_cast<int>(current) * (3 - step) + static_cast<int>(next) * step) / 3;
        return static_cast<std::int16_t>(std::clamp(value, -32768, 32767));
    }

    std::int16_t DecodeNibble(std::uint8_t nibble) {
        static constexpr std::array<int, 89> steps = {
            7,8,9,10,11,12,13,14,16,17,19,21,23,25,28,31,34,37,41,45,50,55,60,66,73,80,88,97,107,118,
            130,143,157,173,190,209,230,253,279,307,337,371,408,449,494,544,598,658,724,796,876,963,1060,
            1166,1282,1411,1552,1707,1878,2066,2272,2499,2749,3024,3327,3660,4026,4428,4871,5358,5894,
            6484,7132,7845,8630,9493,10442,11487,12635,13899,15289,16818,18500,20350,22385,24623,27086,
            29794,32767};
        static constexpr std::array<int, 8> indices = {-1,-1,-1,-1,2,4,6,8};
        const int step = steps[stepIndex_];
        int difference = step >> 3;
        if (nibble & 1) difference += step >> 2;
        if (nibble & 2) difference += step >> 1;
        if (nibble & 4) difference += step;
        predictor_ = std::clamp(predictor_ + ((nibble & 8) ? -difference : difference), -32768, 32767);
        stepIndex_ = std::clamp(stepIndex_ + indices[nibble & 7], 0, 88);
        return static_cast<std::int16_t>(predictor_);
    }

    std::vector<std::uint8_t> pending_;
    std::optional<std::int16_t> resamplePending_;
    std::optional<std::pair<int, int>> pendingSync_;
    size_t frameBytes_ = 120;
    int predictor_ = 0;
    int stepIndex_ = 0;
};

} // namespace axonkey_service
