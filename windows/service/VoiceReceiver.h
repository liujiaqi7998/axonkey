#pragma once

#include <memory>
#include <atomic>
#include <cstdint>
#include <functional>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include "AudioGain.h"
#include "VoiceAudioSession.h"
#include "../../protobuf/axonkey_rpc.h"

namespace axonkey_service {

/**
 * One independent RC003 ATVV receiver. The receiver owns its GATT event
 * subscriptions and its optional exclusive virtual-microphone handle.
 */
class VoiceReceiver final {
public:
    struct State;
    using StatusCallback = std::function<void(const axonkey::rpc::VoiceStatus&)>;
    using IssueCallback = std::function<void(const axonkey::rpc::ServiceIssue&)>;

    explicit VoiceReceiver(std::wstring deviceInstanceId, std::int32_t audioGainDb = kDefaultAudioGainDb,
        VoiceAudioSession::LevelCallback level = {}, StatusCallback status = {},
        IssueCallback issue = {});
    ~VoiceReceiver();

    VoiceReceiver(const VoiceReceiver&) = delete;
    VoiceReceiver& operator=(const VoiceReceiver&) = delete;

    void Start();
    void Stop();
    bool Finished() const;
    void SetAudioGain(std::int32_t audioGainDb) { audioGainDb_.store(audioGainDb); }
    std::optional<std::uint8_t> BatteryLevel() const;
    axonkey::rpc::VoiceStatus Status() const;

private:
    void Run();

    std::wstring deviceInstanceId_;
    std::atomic<std::int32_t> audioGainDb_;
    VoiceAudioSession::LevelCallback level_;
    StatusCallback statusCallback_;
    IssueCallback issueCallback_;
    std::shared_ptr<State> state_;
    std::mutex lifecycleMutex_;
    std::thread worker_;
};

} // namespace axonkey_service
