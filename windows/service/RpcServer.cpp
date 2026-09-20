#include "RpcServer.h"
#include "ServiceLog.h"
#include <sddl.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <cstring>

namespace axonkey_service {
namespace {
using axonkey::rpc::Bytes;
constexpr std::uint32_t kMaxFrame = 1024 * 1024;

struct FrameHeader { std::uint32_t size; };

bool ReadExact(HANDLE pipe, void* destination, DWORD size) {
    auto* bytes = static_cast<std::uint8_t*>(destination);
    while (size) {
        DWORD read = 0;
        if (!ReadFile(pipe, bytes, size, &read, nullptr) || !read) return false;
        bytes += read; size -= read;
    }
    return true;
}

bool WriteExact(HANDLE pipe, const void* source, DWORD size) {
    auto* bytes = static_cast<const std::uint8_t*>(source);
    while (size) {
        DWORD written = 0;
        if (!WriteFile(pipe, bytes, size, &written, nullptr) || !written) return false;
        bytes += written; size -= written;
    }
    return true;
}

bool ReadFrame(HANDLE pipe, Bytes& frame) {
    FrameHeader header{};
    if (!ReadExact(pipe, &header, sizeof(header)) || header.size > kMaxFrame) return false;
    frame.resize(header.size);
    return header.size == 0 || ReadExact(pipe, frame.data(), header.size);
}

bool WriteFrame(HANDLE pipe, const Bytes& frame) {
    FrameHeader header{static_cast<std::uint32_t>(frame.size())};
    return frame.size() <= kMaxFrame && WriteExact(pipe, &header, sizeof(header)) &&
        (frame.empty() || WriteExact(pipe, frame.data(), static_cast<DWORD>(frame.size())));
}

std::uint64_t NowMs() {
    return static_cast<std::uint64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::system_clock::now().time_since_epoch()).count());
}
}

struct RpcServer::Client {
    explicit Client(HANDLE value) : pipe(value) {}
    void Close() {
        if (closed.exchange(true)) return;
        if (pipe != INVALID_HANDLE_VALUE) {
            CancelIoEx(pipe, nullptr);
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
            pipe = INVALID_HANDLE_VALUE;
        }
    }
    HANDLE pipe = INVALID_HANDLE_VALUE;
    std::atomic_bool closed = false;
    std::mutex writeMutex;
    std::atomic_bool keyboard = false, audioLevel = false, voiceStatus = false;
};

RpcServer::RpcServer(RpcHandlers handlers) : handlers_(std::move(handlers)) {}
RpcServer::~RpcServer() { Stop(); }

HANDLE RpcServer::CreatePipe() const {
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    SECURITY_ATTRIBUTES attributes{sizeof(attributes), nullptr, FALSE};
    if (ConvertStringSecurityDescriptorToSecurityDescriptorW(
            L"D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)", SDDL_REVISION_1,
            &descriptor, nullptr)) attributes.lpSecurityDescriptor = descriptor;
    auto pipe = CreateNamedPipeW(kPipeName, PIPE_ACCESS_DUPLEX,
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES,
        kMaxFrame + sizeof(FrameHeader), kMaxFrame + sizeof(FrameHeader), 1000,
        &attributes);
    if (descriptor) LocalFree(descriptor);
    return pipe;
}

bool RpcServer::Start() {
    if (acceptThread_.joinable()) return true;
    stopEvent_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!stopEvent_) return false;
    stopping_.store(false);
    acceptThread_ = std::thread(&RpcServer::AcceptLoop, this);
    LogMessage(L"Axonkey protobuf RPC endpoint started: " + std::wstring(kPipeName));
    return true;
}

void RpcServer::Stop() {
    if (!acceptThread_.joinable()) return;
    stopping_.store(true);
    if (stopEvent_) SetEvent(stopEvent_);
    // Connect once to wake a synchronous ConnectNamedPipe call.
    auto wake = CreateFileW(kPipeName, GENERIC_READ | GENERIC_WRITE, 0, nullptr,
        OPEN_EXISTING, 0, nullptr);
    if (wake != INVALID_HANDLE_VALUE) CloseHandle(wake);
    if (acceptThread_.joinable()) acceptThread_.join();
    std::vector<std::shared_ptr<Client>> clients;
    {
        std::lock_guard lock(clientsMutex_);
        clients.swap(clients_);
    }
    for (const auto& client : clients) {
        client->Close();
    }
    std::vector<std::thread> clientThreads;
    clientThreads.swap(clientThreads_);
    for (auto& thread : clientThreads) if (thread.joinable()) thread.join();
    if (stopEvent_) CloseHandle(stopEvent_);
    stopEvent_ = nullptr;
}

void RpcServer::AcceptLoop() {
    while (!stopping_.load()) {
        auto pipe = CreatePipe();
        if (pipe == INVALID_HANDLE_VALUE) {
            LogMessage(L"Creating Axonkey RPC named pipe failed; Win32=" + std::to_wstring(GetLastError()), LogLevel::Error);
            break;
        }
        auto connected = ConnectNamedPipe(pipe, nullptr) != FALSE || GetLastError() == ERROR_PIPE_CONNECTED;
        if (!connected || stopping_.load()) { CloseHandle(pipe); break; }
        auto client = std::make_shared<Client>(pipe);
        {
            std::lock_guard lock(clientsMutex_);
            clients_.push_back(client);
        }
        std::lock_guard lock(clientsMutex_);
        clientThreads_.emplace_back(&RpcServer::ClientLoop, this, client);
    }
}

void RpcServer::RemoveClient(const std::shared_ptr<Client>& client) {
    std::lock_guard lock(clientsMutex_);
    clients_.erase(std::remove(clients_.begin(), clients_.end(), client), clients_.end());
    client->Close();
}

void RpcServer::ClientLoop(const std::shared_ptr<Client>& client) {
    Bytes frame;
    while (!stopping_.load() && ReadFrame(client->pipe, frame)) {
        axonkey::rpc::Request request;
        if (!axonkey::rpc::Parse(frame, request)) break;
        axonkey::rpc::Response response;
        response.requestId = request.requestId;
        if (request.method == "GetServiceInfo") {
            response.success = true; response.payload = axonkey::rpc::Serialize(handlers_.serviceInfo());
        } else if (request.method == "SetAudioGain") {
            axonkey::rpc::SetAudioGain gain;
            response.success = axonkey::rpc::Parse(request.payload, gain);
            if (response.success) { auto result = handlers_.setAudioGain(gain.gainDb); response.success = result.success; response.error = result.error; response.payload = axonkey::rpc::Serialize(result); }
            else response.error = "invalid SetAudioGain protobuf payload";
        } else if (request.method == "GetDevices") {
            response.success = true; response.payload = axonkey::rpc::Serialize(handlers_.devices());
        } else if (request.method == "GetVoiceStatus") {
            response.success = true; response.payload = axonkey::rpc::Serialize(handlers_.voiceStatus());
        } else if (request.method == "GetAudioLevel") {
            response.success = true; response.payload = axonkey::rpc::Serialize(handlers_.audioLevel());
        } else if (request.method == "Subscribe") {
            axonkey::rpc::Subscribe subscription;
            response.success = axonkey::rpc::Parse(request.payload, subscription);
            if (response.success) { client->keyboard = subscription.keyboard; client->audioLevel = subscription.audioLevel; client->voiceStatus = subscription.voiceStatus; response.payload = axonkey::rpc::Serialize(axonkey::rpc::OperationResult{true, {}}); }
            else response.error = "invalid Subscribe protobuf payload";
        } else { response.error = "unknown Axonkey RPC method"; }
        if (!response.success && response.error.empty()) response.error = "request failed";
        std::lock_guard lock(client->writeMutex);
        if (!WriteFrame(client->pipe, axonkey::rpc::Serialize(response))) break;
    }
    RemoveClient(client);
}

void RpcServer::Publish(const axonkey::rpc::EventEnvelope& event, int kind) {
    const auto bytes = axonkey::rpc::Serialize(event);
    std::lock_guard lock(clientsMutex_);
    for (const auto& client : clients_) {
        if ((kind == 1 && !client->keyboard) || (kind == 2 && !client->audioLevel) || (kind == 3 && !client->voiceStatus)) continue;
        std::lock_guard writeLock(client->writeMutex);
        if (!WriteFrame(client->pipe, bytes)) { DisconnectNamedPipe(client->pipe); }
    }
}

void RpcServer::PublishKeyboard(const std::string& device, const std::vector<std::uint8_t>& report) {
    axonkey::rpc::KeyboardEvent value{device, report, NowMs()};
    Publish({"keyboard", axonkey::rpc::Serialize(value)}, 1);
}
void RpcServer::PublishAudioLevel(const axonkey::rpc::AudioLevel& level) { Publish({"audio_level", axonkey::rpc::Serialize(level)}, 2); }
void RpcServer::PublishVoiceStatus(const axonkey::rpc::VoiceStatus& status) { Publish({"voice_status", axonkey::rpc::Serialize(status)}, 3); }

} // namespace axonkey_service
