#include "RpcServer.h"
#include "ServiceLog.h"
#include <sddl.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <new>

namespace axonkey_service {
namespace {
using axonkey::rpc::Bytes;
constexpr std::uint32_t kMaxFrame = 1024 * 1024;
constexpr std::size_t kMaxQueuedFrames = 256;
constexpr std::size_t kMaxQueuedBytes = 4 * 1024 * 1024;
constexpr auto kWriteTimeout = std::chrono::seconds(2);

struct FrameHeader { std::uint32_t size; };

bool TransferExact(HANDLE pipe, void* buffer, DWORD size, bool write) {
    auto event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!event) return false;
    auto* bytes = static_cast<std::uint8_t*>(buffer);
    while (size) {
        OVERLAPPED operation{};
        operation.hEvent = event;
        ResetEvent(event);
        const auto started = write
            ? WriteFile(pipe, bytes, size, nullptr, &operation)
            : ReadFile(pipe, bytes, size, nullptr, &operation);
        const auto pending = !started && GetLastError() == ERROR_IO_PENDING;
        DWORD transferred = 0;
        if ((!started && !pending) ||
                !GetOverlappedResult(pipe, &operation, &transferred, pending) || !transferred) {
            CloseHandle(event);
            return false;
        }
        bytes += transferred; size -= transferred;
    }
    CloseHandle(event);
    return true;
}

bool ReadExact(HANDLE pipe, void* destination, DWORD size) {
    return TransferExact(pipe, destination, size, false);
}

bool WriteExact(HANDLE pipe, const void* source, DWORD size) {
    return TransferExact(pipe, const_cast<void*>(source), size, true);
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

    ~Client() {
        // A Client is normally destroyed after ClientLoop has joined both
        // worker threads. Keep this defensive for failed thread startup too.
        Close();
        JoinWorkers();
        ClosePipeHandles();
    }

    bool Start() noexcept {
        try {
            senderThread = std::thread(&Client::SenderLoop, this);
            watchdogThread = std::thread(&Client::WatchdogLoop, this);
            return true;
        } catch (...) {
            Close();
            JoinWorkers();
            return false;
        }
    }

    bool Enqueue(Bytes frame) {
        try {
            if (frame.size() > kMaxFrame) return false;
            bool overLimit = false;
            {
                std::lock_guard lock(queueMutex);
                if (closed.load()) return false;
                overLimit = outbound.size() >= kMaxQueuedFrames ||
                    queuedBytes > kMaxQueuedBytes - frame.size();
                if (!overLimit) {
                    queuedBytes += frame.size();
                    outbound.push_back(std::move(frame));
                }
            }
            if (overLimit) {
                LogMessage(L"Axonkey RPC client outbound queue limit reached; disconnecting",
                    LogLevel::Warning);
                Close();
                return false;
            }
            queueCv.notify_one();
            return true;
        } catch (const std::bad_alloc&) {
            LogMessage(L"Axonkey RPC client outbound allocation failed; disconnecting", LogLevel::Warning);
            Close();
            return false;
        } catch (...) {
            LogMessage(L"Axonkey RPC client enqueue failed; disconnecting", LogLevel::Warning);
            Close();
            return false;
        }
    }

    void Close() {
        closed.store(true);
        {
            std::lock_guard lock(queueMutex);
            outbound.clear();
            queuedBytes = 0;
        }
        queueCv.notify_all();
        writeStateCv.notify_all();
        if (pipe != INVALID_HANDLE_VALUE) {
            CancelIoEx(pipe, nullptr);
            DisconnectNamedPipe(pipe);
        }
    }

    void JoinWorkers() {
        if (senderThread.joinable()) senderThread.join();
        if (watchdogThread.joinable()) watchdogThread.join();
    }

    void ClosePipeHandles() {
        auto handle = pipe;
        pipe = INVALID_HANDLE_VALUE;
        if (handle != INVALID_HANDLE_VALUE) {
            CancelIoEx(handle, nullptr);
            DisconnectNamedPipe(handle);
            CloseHandle(handle);
        }
    }

private:
    void SenderLoop() {
        for (;;) {
            Bytes frame;
            {
                std::unique_lock lock(queueMutex);
                queueCv.wait(lock, [this] { return closed.load() || !outbound.empty(); });
                if (outbound.empty()) break;
                frame = std::move(outbound.front());
                outbound.pop_front();
                queuedBytes -= frame.size();
            }

            if (closed.load()) break;
            {
                std::lock_guard lock(writeStateMutex);
                if (closed.load()) break;
                writeInProgress = true;
                writeDeadline = std::chrono::steady_clock::now() + kWriteTimeout;
                writeTimedOut.store(false);
            }
            writeStateCv.notify_all();
            const bool written = WriteFrame(pipe, frame);
            {
                std::lock_guard lock(writeStateMutex);
                writeInProgress = false;
            }
            writeStateCv.notify_all();
            if (!written) {
                if (writeTimedOut.exchange(false)) {
                    LogMessage(L"Axonkey RPC client write timed out; disconnecting",
                        LogLevel::Warning);
                }
                Close();
                break;
            }
        }
        {
            std::lock_guard lock(writeStateMutex);
            writeInProgress = false;
        }
        writeStateCv.notify_all();
    }

    void WatchdogLoop() {
        std::unique_lock lock(writeStateMutex);
        while (!closed.load()) {
            if (!writeInProgress) {
                writeStateCv.wait(lock, [this] {
                    return closed.load() || writeInProgress;
                });
                continue;
            }
            const auto deadline = writeDeadline;
            if (writeStateCv.wait_until(lock, deadline, [this, deadline] {
                    return closed.load() || !writeInProgress || writeDeadline != deadline;
                })) continue;
            if (closed.load() || !writeInProgress) continue;

            // Keep writeStateMutex while cancelling so this targets the
            // current write, not a subsequent frame.
            writeTimedOut.store(true);
            CancelIoEx(pipe, nullptr);
            writeStateCv.wait(lock, [this] {
                return closed.load() || !writeInProgress;
            });
        }
    }

public:
    HANDLE pipe = INVALID_HANDLE_VALUE;
    std::atomic_bool closed = false;
    std::atomic_bool keyboard = false, audioLevel = false, voiceStatus = false, serviceIssues = false;

private:
    std::mutex queueMutex;
    std::condition_variable queueCv;
    std::deque<Bytes> outbound;
    std::size_t queuedBytes = 0;
    std::thread senderThread;
    std::thread watchdogThread;
    std::mutex writeStateMutex;
    std::condition_variable writeStateCv;
    std::chrono::steady_clock::time_point writeDeadline{};
    bool writeInProgress = false;
    std::atomic_bool writeTimedOut = false;
};

RpcServer::RpcServer(RpcHandlers handlers, std::wstring pipeName)
    : handlers_(std::move(handlers)), pipeName_(std::move(pipeName)) {}
RpcServer::~RpcServer() { Stop(); }

HANDLE RpcServer::CreatePipe() const {
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    SECURITY_ATTRIBUTES attributes{sizeof(attributes), nullptr, FALSE};
    if (ConvertStringSecurityDescriptorToSecurityDescriptorW(
            L"D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)(A;;GA;;;IU)(A;;GA;;;WD)", SDDL_REVISION_1,
            &descriptor, nullptr)) attributes.lpSecurityDescriptor = descriptor;
    auto pipe = CreateNamedPipeW(pipeName_.c_str(), PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES,
        kMaxFrame + sizeof(FrameHeader), kMaxFrame + sizeof(FrameHeader), 1000,
        &attributes);
    const auto error = (pipe == INVALID_HANDLE_VALUE) ? GetLastError() : ERROR_SUCCESS;
    if (descriptor) LocalFree(descriptor);
    if (pipe == INVALID_HANDLE_VALUE) {
        LogMessage(L"Creating Axonkey RPC named pipe instance failed; Win32=" +
            std::to_wstring(error), LogLevel::Error);
    }
    return pipe;
}

bool RpcServer::Start() {
    if (acceptThread_.joinable()) return true;
    stopEvent_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!stopEvent_) return false;
    stopping_.store(false);
    // Fail synchronously if the first named-pipe instance cannot be created;
    // otherwise the service could advertise a healthy RPC endpoint while its
    // accept thread had already exited.
    auto initialPipe = CreatePipe();
    if (initialPipe == INVALID_HANDLE_VALUE) {
        CloseHandle(stopEvent_);
        stopEvent_ = nullptr;
        return false;
    }
    CloseHandle(initialPipe);
    try {
        acceptThread_ = std::thread(&RpcServer::AcceptLoop, this);
    } catch (...) {
        CloseHandle(stopEvent_);
        stopEvent_ = nullptr;
        stopping_.store(true);
        LogMessage(L"Starting Axonkey RPC accept thread failed", LogLevel::Error);
        return false;
    }
    LogMessage(L"Axonkey protobuf RPC endpoint started: " + pipeName_);
    return true;
}

void RpcServer::Stop() {
    if (!acceptThread_.joinable()) return;
    stopping_.store(true);
    if (stopEvent_) SetEvent(stopEvent_);
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
    for (const auto& client : clients) {
        client->JoinWorkers();
        client->ClosePipeHandles();
    }
    if (stopEvent_) CloseHandle(stopEvent_);
    stopEvent_ = nullptr;
}

void RpcServer::AcceptLoop() noexcept {
    try {
        AcceptLoopImpl();
    } catch (const std::bad_alloc&) {
        LogMessage(L"Axonkey RPC accept loop ran out of memory; stopping accepts", LogLevel::Error);
    } catch (...) {
        LogMessage(L"Axonkey RPC accept loop stopped after an unknown exception", LogLevel::Error);
    }
}

void RpcServer::AcceptLoopImpl() {
    auto pipe = CreatePipe();
    while (!stopping_.load()) {
        if (pipe == INVALID_HANDLE_VALUE) {
            LogMessage(L"Creating Axonkey RPC named pipe failed; Win32=" + std::to_wstring(GetLastError()), LogLevel::Error);
            break;
        }
        auto event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!event) {
            CloseHandle(pipe);
            pipe = INVALID_HANDLE_VALUE;
            break;
        }
        OVERLAPPED operation{};
        operation.hEvent = event;
        bool connected = ConnectNamedPipe(pipe, &operation) != FALSE;
        DWORD error = connected ? ERROR_SUCCESS : GetLastError();
        if (error == ERROR_PIPE_CONNECTED) connected = true;
        if (error == ERROR_IO_PENDING) {
            const HANDLE waits[] = {stopEvent_, event};
            const auto wait = WaitForMultipleObjects(2, waits, FALSE, INFINITE);
            if (wait == WAIT_OBJECT_0 + 1) {
                DWORD transferred = 0;
                connected = GetOverlappedResult(pipe, &operation, &transferred, FALSE) != FALSE;
                if (!connected) error = GetLastError();
            } else {
                CancelIoEx(pipe, &operation);
                DWORD transferred = 0;
                GetOverlappedResult(pipe, &operation, &transferred, TRUE);
                error = wait == WAIT_OBJECT_0 ? ERROR_OPERATION_ABORTED : GetLastError();
            }
        }
        CloseHandle(event);
        if (!connected || stopping_.load()) {
            if (!stopping_.load()) LogMessage(L"Accepting Axonkey RPC client failed; Win32=" +
                std::to_wstring(error), LogLevel::Error);
            CloseHandle(pipe);
            pipe = INVALID_HANDLE_VALUE;
            break;
        }

        // Create the next listening instance before taking locks or starting
        // the client thread. This removes the small gap in which a second
        // client could observe ERROR_PIPE_BUSY after the current instance was
        // accepted but before the old loop created its replacement.
        auto nextPipe = CreatePipe();
        if (nextPipe == INVALID_HANDLE_VALUE) {
            LogMessage(L"Creating replacement Axonkey RPC named pipe failed; Win32=" +
                std::to_wstring(GetLastError()), LogLevel::Error);
            CloseHandle(pipe);
            pipe = INVALID_HANDLE_VALUE;
            break;
        }
        auto client = std::make_shared<Client>(pipe);
        if (!client->Start()) {
            LogMessage(L"Starting Axonkey RPC client workers failed; disconnecting",
                LogLevel::Error);
            client->ClosePipeHandles();
            pipe = nextPipe;
            continue;
        }
        {
            std::lock_guard lock(clientsMutex_);
            clients_.push_back(client);
            clientThreads_.emplace_back(&RpcServer::ClientLoop, this, client);
        }
        pipe = nextPipe;
    }
    if (pipe != INVALID_HANDLE_VALUE) CloseHandle(pipe);
}

void RpcServer::RemoveClient(const std::shared_ptr<Client>& client) {
    {
        std::lock_guard lock(clientsMutex_);
        clients_.erase(std::remove(clients_.begin(), clients_.end(), client), clients_.end());
    }
    client->Close();
}

void RpcServer::ClientLoop(const std::shared_ptr<Client>& client) noexcept {
    try {
        ClientLoopImpl(client);
    } catch (const std::bad_alloc&) {
        LogMessage(L"Axonkey RPC client loop ran out of memory; disconnecting", LogLevel::Warning);
    } catch (...) {
        LogMessage(L"Axonkey RPC client loop stopped after an unknown exception", LogLevel::Warning);
    }
    RemoveClient(client);
    client->JoinWorkers();
    client->ClosePipeHandles();
}

void RpcServer::ClientLoopImpl(const std::shared_ptr<Client>& client) {
    Bytes frame;
    while (!stopping_.load() && !client->closed.load() && ReadFrame(client->pipe, frame)) {
        axonkey::rpc::Request request;
        if (!axonkey::rpc::Parse(frame, request)) break;
        axonkey::rpc::Response response;
        response.requestId = request.requestId;
        bool updateSubscription = false;
        bool subscribeKeyboard = false;
        bool subscribeAudioLevel = false;
        bool subscribeVoiceStatus = false;
        bool subscribeServiceIssues = false;
        try {
        if (request.method == "GetServiceInfo") {
            response.success = true; response.payload = axonkey::rpc::Serialize(handlers_.serviceInfo());
        } else if (request.method == "GetServiceStatus") {
            response.success = true; response.payload = axonkey::rpc::Serialize(handlers_.serviceStatus());
        } else if (request.method == "SetServiceStatus") {
            axonkey::rpc::SetServiceStatus status;
            response.success = axonkey::rpc::Parse(request.payload, status);
            if (response.success) {
                auto result = handlers_.setServiceStatus(status.enabled);
                response.success = result.success;
                response.error = result.error;
                response.payload = axonkey::rpc::Serialize(result);
            } else response.error = "invalid SetServiceStatus protobuf payload";
        } else if (request.method == "SetServiceEnable") {
            axonkey::rpc::SetServiceEnable enable;
            response.success = axonkey::rpc::Parse(request.payload, enable);
            if (response.success) {
                auto result = handlers_.setServiceEnable(enable.enabled);
                response.success = result.success;
                response.error = result.error;
                response.payload = axonkey::rpc::Serialize(result);
            } else response.error = "invalid SetServiceEnable protobuf payload";
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
            if (response.success) {
                // Do not expose the subscription to publishers until its
                // acknowledgement is on the pipe. Otherwise a report can
                // race the response and be mistaken for the handshake.
                updateSubscription = true;
                subscribeKeyboard = subscription.keyboard;
                subscribeAudioLevel = subscription.audioLevel;
                subscribeVoiceStatus = subscription.voiceStatus;
                subscribeServiceIssues = subscription.serviceIssues;
                response.payload = axonkey::rpc::Serialize(axonkey::rpc::OperationResult{true, {}});
            }
            else response.error = "invalid Subscribe protobuf payload";
        } else { response.error = "unknown Axonkey RPC method"; }
        } catch (const std::exception& error) {
            response.success = false;
            response.error = std::string("request handler failed: ") + error.what();
            LogMessage(response.error, LogLevel::Error);
        } catch (...) {
            response.success = false;
            response.error = "request handler failed with an unknown exception";
            LogMessage(L"Axonkey RPC request handler failed with an unknown exception", LogLevel::Error);
        }
        if (!response.success && response.error.empty()) response.error = "request failed";
        if (!client->Enqueue(axonkey::rpc::Serialize(response))) break;
        if (updateSubscription) {
            client->keyboard = subscribeKeyboard;
            client->audioLevel = subscribeAudioLevel;
            client->voiceStatus = subscribeVoiceStatus;
            client->serviceIssues = subscribeServiceIssues;
        }
    }
}

void RpcServer::Publish(const axonkey::rpc::EventEnvelope& event, int kind) {
    try {
        const auto bytes = axonkey::rpc::Serialize(event);
        if (bytes.empty() && !event.payload.empty()) {
            LogMessage(L"Axonkey RPC event serialization failed; dropping event", LogLevel::Warning);
            return;
        }
        std::vector<std::shared_ptr<Client>> targets;
        {
            std::lock_guard lock(clientsMutex_);
            for (const auto& client : clients_) {
                if ((kind == 1 && !client->keyboard) ||
                        (kind == 2 && !client->audioLevel) ||
                        (kind == 3 && !client->voiceStatus) ||
                        (kind == 4 && !client->serviceIssues)) continue;
                targets.push_back(client);
            }
        }
        for (const auto& client : targets) {
            // Enqueue is bounded and never performs pipe I/O. A slow or stuck
            // client is disconnected by the client worker without blocking the
            // service thread that produced this event.
            client->Enqueue(bytes);
        }
    } catch (const std::bad_alloc&) {
        LogMessage(L"Axonkey RPC event allocation failed; dropping event", LogLevel::Warning);
    } catch (...) {
        LogMessage(L"Axonkey RPC event publication failed; dropping event", LogLevel::Warning);
    }
}

void RpcServer::PublishKeyboard(const std::string& device, const std::vector<std::uint8_t>& report) {
    axonkey::rpc::KeyboardEvent value{device, report, NowMs()};
    Publish({"keyboard", axonkey::rpc::Serialize(value)}, 1);
}
void RpcServer::PublishAudioLevel(const axonkey::rpc::AudioLevel& level) { Publish({"audio_level", axonkey::rpc::Serialize(level)}, 2); }
void RpcServer::PublishVoiceStatus(const axonkey::rpc::VoiceStatus& status) { Publish({"voice_status", axonkey::rpc::Serialize(status)}, 3); }
void RpcServer::PublishServiceIssue(const axonkey::rpc::ServiceIssue& issue) {
    auto payload = axonkey::rpc::Serialize(issue);
    if (payload.empty() && (!issue.code.empty() || !issue.message.empty() ||
        !issue.deviceInstanceId.empty())) {
        LogMessage(L"Axonkey RPC service issue serialization failed; dropping issue", LogLevel::Warning);
        return;
    }
    Publish({"service_issue", std::move(payload)}, 4);
}

} // namespace axonkey_service
