#include "../RpcServer.h"

#include <array>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <string>
#include <thread>

namespace {
using axonkey::rpc::Bytes;

bool WriteExact(HANDLE pipe, const void* data, DWORD size) {
    auto* bytes = static_cast<const std::uint8_t*>(data);
    while (size) {
        DWORD written = 0;
        if (!WriteFile(pipe, bytes, size, &written, nullptr) || !written) return false;
        bytes += written;
        size -= written;
    }
    return true;
}

bool ReadExact(HANDLE pipe, void* data, DWORD size) {
    auto* bytes = static_cast<std::uint8_t*>(data);
    while (size) {
        DWORD read = 0;
        if (!ReadFile(pipe, bytes, size, &read, nullptr) || !read) return false;
        bytes += read;
        size -= read;
    }
    return true;
}

bool WriteFrame(HANDLE pipe, const Bytes& bytes) {
    const auto size = static_cast<std::uint32_t>(bytes.size());
    std::array<std::uint8_t, 4> header{};
    std::memcpy(header.data(), &size, header.size());
    return WriteExact(pipe, header.data(), static_cast<DWORD>(header.size())) &&
        WriteExact(pipe, bytes.data(), size);
}

bool ReadFrame(HANDLE pipe, Bytes& bytes) {
    std::array<std::uint8_t, 4> header{};
    if (!ReadExact(pipe, header.data(), static_cast<DWORD>(header.size()))) return false;
    std::uint32_t size = 0;
    std::memcpy(&size, header.data(), header.size());
    if (size > 1024 * 1024) return false;
    bytes.resize(size);
    return !size || ReadExact(pipe, bytes.data(), size);
}

HANDLE OpenPipe(const std::wstring& name) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (std::chrono::steady_clock::now() < deadline) {
        auto pipe = CreateFileW(name.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr,
            OPEN_EXISTING, 0, nullptr);
        if (pipe != INVALID_HANDLE_VALUE) return pipe;
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    return INVALID_HANDLE_VALUE;
}

bool ExercisePipe(HANDLE pipe, axonkey_service::RpcServer& server, const std::string& name) {
    for (std::uint64_t id = 1; id <= 5; ++id) {
        if (!WriteFrame(pipe, axonkey::rpc::Serialize(axonkey::rpc::Request{
                id, "GetServiceInfo", {}}))) return false;
        // Force the server's request thread to enter its next read before
        // consuming the response. A synchronous pipe handle deadlocks here.
        std::this_thread::sleep_for(std::chrono::milliseconds(30));
        Bytes frame;
        axonkey::rpc::Response response;
        axonkey::rpc::ServiceInfo info;
        if (!ReadFrame(pipe, frame) || !axonkey::rpc::Parse(frame, response) ||
                response.requestId != id || !response.success ||
                !axonkey::rpc::Parse(response.payload, info) || info.pipeName != name) return false;
    }

    if (!WriteFrame(pipe, axonkey::rpc::Serialize(axonkey::rpc::Request{
            6, "Subscribe", axonkey::rpc::Serialize(axonkey::rpc::Subscribe{true, false, false})})))
        return false;
    Bytes frame;
    axonkey::rpc::Response acknowledgement;
    if (!ReadFrame(pipe, frame) || !axonkey::rpc::Parse(frame, acknowledgement) ||
            acknowledgement.requestId != 6 || !acknowledgement.success) return false;
    server.PublishKeyboard("rc003", {0x01, 0x02});
    axonkey::rpc::EventEnvelope event;
    return ReadFrame(pipe, frame) && axonkey::rpc::Parse(frame, event) && event.type == "keyboard";
}
}

int main() {
    const auto pipeName = std::wstring(axonkey_service::RpcServer::kPipeName) + L".test." +
        std::to_wstring(GetCurrentProcessId()) + L"." + std::to_wstring(GetTickCount64());
    std::string name;
    for (const auto character : pipeName) name.push_back(static_cast<char>(character));
    axonkey_service::RpcServer server({
        [name] { return axonkey::rpc::ServiceInfo{"AxonkeyService", "test", "axonkey.service.v1", name}; },
        [] { return axonkey::rpc::ServiceStatus{true}; },
        [](bool) { return axonkey::rpc::OperationResult{true, {}}; },
        [](std::int32_t) { return axonkey::rpc::OperationResult{true, {}}; },
        [] { return axonkey::rpc::DeviceList{}; },
        [] { return axonkey::rpc::VoiceStatus{}; },
        [] { return axonkey::rpc::AudioLevel{}; },
    }, pipeName);
    if (!server.Start()) {
        std::fprintf(stderr, "RPC server could not start\n");
        return 1;
    }
    auto pipe = OpenPipe(pipeName);
    bool ok = pipe != INVALID_HANDLE_VALUE && ExercisePipe(pipe, server, name);
    if (pipe != INVALID_HANDLE_VALUE) CloseHandle(pipe);
    // Keep a client idle during Stop to exercise cancellation of its pending
    // overlapped read and the accept thread's pending connection.
    auto idle = OpenPipe(pipeName);
    ok = ok && idle != INVALID_HANDLE_VALUE;
    server.Stop();
    if (idle != INVALID_HANDLE_VALUE) CloseHandle(idle);
    if (!ok) std::fprintf(stderr, "RPC response/event was not delivered while the server was reading\n");
    return ok ? 0 : 1;
}
