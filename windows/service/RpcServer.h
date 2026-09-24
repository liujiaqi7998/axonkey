#pragma once

#include "../../protobuf/axonkey_rpc.h"
#include <windows.h>
#include <atomic>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace axonkey_service {

struct RpcHandlers {
    std::function<axonkey::rpc::ServiceInfo()> serviceInfo;
    std::function<axonkey::rpc::ServiceStatus()> serviceStatus;
    std::function<axonkey::rpc::OperationResult(bool)> setServiceStatus;
    std::function<axonkey::rpc::OperationResult(bool)> setServiceEnable;
    std::function<axonkey::rpc::OperationResult(std::int32_t)> setAudioGain;
    std::function<axonkey::rpc::DeviceList()> devices;
    std::function<axonkey::rpc::VoiceStatus()> voiceStatus;
    std::function<axonkey::rpc::AudioLevel()> audioLevel;
};

// Named-pipe protobuf transport for the Windows service. Schema and nanopb
// codec live under /protobuf (axonkey_rpc + generated pb); this class owns
// framing, client sessions, and request dispatch only.
class RpcServer final {
public:
    static constexpr wchar_t kPipeName[] = L"\\\\.\\pipe\\AxonkeyService.v1";
    explicit RpcServer(RpcHandlers handlers, std::wstring pipeName = kPipeName);
    ~RpcServer();
    RpcServer(const RpcServer&) = delete;
    RpcServer& operator=(const RpcServer&) = delete;

    bool Start();
    void Stop();
    void PublishKeyboard(const std::string& deviceInstanceId, const std::vector<std::uint8_t>& report);
    void PublishAudioLevel(const axonkey::rpc::AudioLevel& level);
    void PublishVoiceStatus(const axonkey::rpc::VoiceStatus& status);
    void PublishServiceIssue(const axonkey::rpc::ServiceIssue& issue);

private:
    struct Client;
    void AcceptLoop() noexcept;
    void AcceptLoopImpl();
    void ClientLoop(const std::shared_ptr<Client>& client) noexcept;
    void ClientLoopImpl(const std::shared_ptr<Client>& client);
    void RemoveClient(const std::shared_ptr<Client>& client);
    void Publish(const axonkey::rpc::EventEnvelope& event, int kind);
    HANDLE CreatePipe() const;

    RpcHandlers handlers_;
    std::wstring pipeName_;
    std::atomic_bool stopping_{false};
    HANDLE stopEvent_ = nullptr;
    std::thread acceptThread_;
    std::mutex clientsMutex_;
    std::vector<std::shared_ptr<Client>> clients_;
    std::vector<std::thread> clientThreads_;
};

} // namespace axonkey_service
