#include "RpcServer.h"
#include "ServiceLog.h"
#include <sddl.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <deque>

namespace axonkey_service {
namespace {
using axonkey::rpc::Bytes;
constexpr std::uint32_t kMaxFrame = 1024 * 1024;
constexpr std::size_t kMaxQueuedFrames = 256;
constexpr std::size_t kMaxQueuedBytes = 4 * 1024 * 1024;
constexpr auto kWriteTimeout = std::chrono::seconds(2);

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
        HANDLE writerHandle = nullptr;
        {
            std::lock_guard lock(writeStateMutex);
            writerHandle = writerThreadHandle;
            writerThreadHandle = nullptr;
        }
        if (writerHandle) CloseHandle(writerHandle);
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
        HANDLE duplicatedThread = nullptr;
        if (!DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(),
                &duplicatedThread, 0, FALSE, DUPLICATE_SAME_ACCESS)) {
            LogMessage(L"Axonkey RPC sender thread handle could not be duplicated; disconnecting",
                LogLevel::Error);
            Close();
            return;
        }
        {
            std::lock_guard lock(writeStateMutex);
            writerThreadHandle = duplicatedThread;
        }
        writeStateCv.notify_all();

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
            if (!writerThreadHandle || !writeInProgress) {
                writeStateCv.wait(lock, [this] {
                    return closed.load() || (writerThreadHandle && writeInProgress);
                });
                continue;
            }
            const auto deadline = writeDeadline;
            if (writeStateCv.wait_until(lock, deadline, [this, deadline] {
                    return closed.load() || !writeInProgress || writeDeadline != deadline;
                })) continue;
            if (closed.load() || !writeInProgress) continue;

            // Keep writeStateMutex while cancelling. SenderLoop cannot mark
            // this write complete or start another one until cancellation is
            // targeted at the same operation.
            writeTimedOut.store(true);
            CancelSynchronousIo(writerThreadHandle);
            CancelIoEx(pipe, nullptr);
            writeStateCv.wait(lock, [this] {
                return closed.load() || !writeInProgress;
            });
        }
    }

public:
    HANDLE pipe = INVALID_HANDLE_VALUE;
    std::atomic_bool closed = false;
    std::atomic_bool keyboard = false, audioLevel = false, voiceStatus = false;

private:
    std::mutex queueMutex;
    std::condition_variable queueCv;
    std::deque<Bytes> outbound;
    std::size_t queuedBytes = 0;
    std::thread senderThread;
    std::thread watchdogThread;
    std::mutex writeStateMutex;
    std::condition_variable writeStateCv;
    HANDLE writerThreadHandle = nullptr;
    std::chrono::steady_clock::time_point writeDeadline{};
    bool writeInProgress = false;
    std::atomic_bool writeTimedOut = false;
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
    for (const auto& client : clients) {
        client->JoinWorkers();
        client->ClosePipeHandles();
    }
    if (stopEvent_) CloseHandle(stopEvent_);
    stopEvent_ = nullptr;
}

void RpcServer::AcceptLoop() {
    auto pipe = CreatePipe();
    while (!stopping_.load()) {
        if (pipe == INVALID_HANDLE_VALUE) {
            LogMessage(L"Creating Axonkey RPC named pipe failed; Win32=" + std::to_wstring(GetLastError()), LogLevel::Error);
            break;
        }
        auto connected = ConnectNamedPipe(pipe, nullptr) != FALSE || GetLastError() == ERROR_PIPE_CONNECTED;
        if (!connected || stopping_.load()) {
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

void RpcServer::ClientLoop(const std::shared_ptr<Client>& client) {
    Bytes frame;
    while (!stopping_.load() && ReadFrame(client->pipe, frame)) {
        axonkey::rpc::Request request;
        if (!axonkey::rpc::Parse(frame, request)) break;
        axonkey::rpc::Response response;
        response.requestId = request.requestId;
        bool updateSubscription = false;
        bool subscribeKeyboard = false;
        bool subscribeAudioLevel = false;
        bool subscribeVoiceStatus = false;
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
            if (response.success) {
                // Do not expose the subscription to publishers until its
                // acknowledgement is on the pipe. Otherwise a report can
                // race the response and be mistaken for the handshake.
                updateSubscription = true;
                subscribeKeyboard = subscription.keyboard;
                subscribeAudioLevel = subscription.audioLevel;
                subscribeVoiceStatus = subscription.voiceStatus;
                response.payload = axonkey::rpc::Serialize(axonkey::rpc::OperationResult{true, {}});
            }
            else response.error = "invalid Subscribe protobuf payload";
        } else { response.error = "unknown Axonkey RPC method"; }
        if (!response.success && response.error.empty()) response.error = "request failed";
        if (!client->Enqueue(axonkey::rpc::Serialize(response))) break;
        if (updateSubscription) {
            client->keyboard = subscribeKeyboard;
            client->audioLevel = subscribeAudioLevel;
            client->voiceStatus = subscribeVoiceStatus;
        }
    }
    RemoveClient(client);
    client->JoinWorkers();
    client->ClosePipeHandles();
}

void RpcServer::Publish(const axonkey::rpc::EventEnvelope& event, int kind) {
    const auto bytes = axonkey::rpc::Serialize(event);
    std::vector<std::shared_ptr<Client>> targets;
    {
        std::lock_guard lock(clientsMutex_);
        for (const auto& client : clients_) {
            if ((kind == 1 && !client->keyboard) ||
                    (kind == 2 && !client->audioLevel) ||
                    (kind == 3 && !client->voiceStatus)) continue;
            targets.push_back(client);
        }
    }
    for (const auto& client : targets) {
        // Enqueue is bounded and never performs pipe I/O. A slow or stuck
        // client is disconnected by the client worker without blocking the
        // service thread that produced this event.
        client->Enqueue(bytes);
    }
}

void RpcServer::PublishKeyboard(const std::string& device, const std::vector<std::uint8_t>& report) {
    axonkey::rpc::KeyboardEvent value{device, report, NowMs()};
    Publish({"keyboard", axonkey::rpc::Serialize(value)}, 1);
}
void RpcServer::PublishAudioLevel(const axonkey::rpc::AudioLevel& level) { Publish({"audio_level", axonkey::rpc::Serialize(level)}, 2); }
void RpcServer::PublishVoiceStatus(const axonkey::rpc::VoiceStatus& status) { Publish({"voice_status", axonkey::rpc::Serialize(status)}, 3); }

} // namespace axonkey_service
