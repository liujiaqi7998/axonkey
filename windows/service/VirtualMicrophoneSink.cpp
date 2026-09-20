#include "VirtualMicrophoneSink.h"
#include "ServiceLog.h"
#include <algorithm>
#include <chrono>
#include <cstring>
#include <string>
#include <thread>

namespace axonkey_service {
namespace {
using Clock = std::chrono::steady_clock;
// Same 10 ms maximum commit as AxonkeyVirtualMicrophoneDriverTest. BLE already
// supplies the audio clock, so do not add its synthetic-tone producer's Sleep(9).
constexpr ULONG kChunkBytes = 960;
constexpr auto kStallTimeout = std::chrono::milliseconds(250);

void LogFailure(const wchar_t* operation, DWORD error) {
    LogMessage(std::wstring(L"Quarbor Virtual Microphone: ") + operation +
        L" failed; Win32=" + std::to_wstring(error), LogLevel::Error);
}

class Win32MicrophoneTransport final : public MicrophoneTransport {
public:
    ~Win32MicrophoneTransport() override { Close(); }
    bool Open() override {
        handle_ = CreateFileW(QUARBOR_MIC_DEVICE_PATH, GENERIC_READ | GENERIC_WRITE,
            0, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        return handle_ != INVALID_HANDLE_VALUE;
    }
    void Close() override {
        if (handle_ != INVALID_HANDLE_VALUE) CloseHandle(handle_);
        handle_ = INVALID_HANDLE_VALUE;
    }
    bool Control(DWORD code, const void* input, DWORD inputBytes, void* output,
        DWORD outputBytes, DWORD& returned) override {
        return DeviceIoControl(handle_, code, const_cast<void*>(input), inputBytes,
            output, outputBytes, &returned, nullptr) != FALSE;
    }
private:
    HANDLE handle_ = INVALID_HANDLE_VALUE;
};
}

VirtualMicrophoneSink::VirtualMicrophoneSink()
    : VirtualMicrophoneSink(std::make_unique<Win32MicrophoneTransport>()) {}
VirtualMicrophoneSink::VirtualMicrophoneSink(std::unique_ptr<MicrophoneTransport> transport)
    : transport_(std::move(transport)) {}
VirtualMicrophoneSink::~VirtualMicrophoneSink() { Stop(); }

bool VirtualMicrophoneSink::Control(DWORD code, const void* input, DWORD inputBytes,
    void* output, DWORD outputBytes, DWORD* returned) {
    DWORD bytes = 0;
    if (!transport_->Control(code, input, inputBytes, output, outputBytes, bytes)) {
        const auto error = GetLastError();
        LogMessage(L"Quarbor Virtual Microphone: IOCTL=" + std::to_wstring(code) +
            L" failed; Win32=" + std::to_wstring(error), LogLevel::Error);
        return false;
    }
    if (returned) *returned = bytes;
    return true;
}

bool VirtualMicrophoneSink::Start() {
    std::lock_guard lock(mutex_);
    if (cancelled_) return false;
    if (started_) return true;
    if (!transport_->Open()) {
        LogFailure(L"open QuarborVirtualMicrophone (exclusive producer)", GetLastError());
        return false;
    }
    opened_ = true;
    QUARBOR_MIC_RING_MAPPING mapping{};
    DWORD returned = 0;
    if (!Control(IOCTL_QUARBOR_MIC_MAP_RING, nullptr, 0, &mapping, sizeof(mapping), &returned)) {
        CloseLocked();
        return false;
    }
    if (returned != sizeof(mapping) || mapping.Size != sizeof(mapping) ||
        mapping.Version != QUARBOR_MIC_API_VERSION || !mapping.UserAddress ||
        !mapping.BufferBytes || mapping.BufferBytes > QUARBOR_MIC_RING_BYTES ||
        mapping.BufferBytes % QUARBOR_MIC_BLOCK_ALIGN ||
        mapping.SampleRate != QUARBOR_MIC_SAMPLE_RATE || mapping.Channels != 1 ||
        mapping.BitsPerSample != 16 || mapping.BlockAlign != 2) {
        LogMessage(L"Quarbor Virtual Microphone: incompatible MAP_RING response; expected 48000 Hz mono PCM16", LogLevel::Error);
        CloseLocked();
        return false;
    }
    ring_ = reinterpret_cast<std::uint8_t*>(static_cast<std::uintptr_t>(mapping.UserAddress));
    bufferBytes_ = mapping.BufferBytes;
    if (cancelled_ || !Control(IOCTL_QUARBOR_MIC_RESET) || !Control(IOCTL_QUARBOR_MIC_START)) {
        CloseLocked();
        return false;
    }
    committedBytes_ = 0;
    started_ = true;
    LogMessage(L"Quarbor Virtual Microphone: PCM ingress started (48000 Hz, mono, 16 bit)");
    return true;
}

bool VirtualMicrophoneSink::Query(QUARBOR_MIC_STATE& state) {
    DWORD returned = 0;
    if (!Control(IOCTL_QUARBOR_MIC_QUERY_STATE, nullptr, 0, &state, sizeof(state), &returned)) return false;
    constexpr ULONG required = QUARBOR_MIC_STATE_STARTED | QUARBOR_MIC_STATE_MAPPED | QUARBOR_MIC_STATE_ONLINE;
    if (returned != sizeof(state) || state.Size != sizeof(state) ||
        state.Version != QUARBOR_MIC_API_VERSION || !state.Generation ||
        state.BufferBytes != bufferBytes_ || state.SampleRate != QUARBOR_MIC_SAMPLE_RATE ||
        state.Channels != 1 || state.BitsPerSample != 16 || state.BlockAlign != 2 ||
        (state.Flags & required) != required || state.WritePosition < state.ReadPosition ||
        state.WritePosition - state.ReadPosition > bufferBytes_ ||
        state.AvailableBytes != state.WritePosition - state.ReadPosition ||
        state.FreeBytes != bufferBytes_ - state.AvailableBytes ||
        (state.WritePosition | state.ReadPosition | state.FreeBytes) % 2) {
        LogMessage(L"Quarbor Virtual Microphone: invalid/offline QUERY_STATE response", LogLevel::Error);
        return false;
    }
    return true;
}

bool VirtualMicrophoneSink::Reset() {
    std::lock_guard lock(mutex_);
    return !cancelled_ && started_ && Control(IOCTL_QUARBOR_MIC_RESET);
}

bool VirtualMicrophoneSink::Push(std::span<const std::int16_t> samples) {
    std::lock_guard lock(mutex_);
    if (cancelled_ || !started_) return false;
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(samples.data());
    size_t offset = 0;
    auto deadline = Clock::now() + kStallTimeout;
    while (offset < samples.size_bytes()) {
        if (cancelled_) return false;
        QUARBOR_MIC_STATE state{};
        if (!Query(state)) return false;
        const auto count = static_cast<ULONG>(std::min<size_t>(
            std::min(kChunkBytes, state.FreeBytes), samples.size_bytes() - offset));
        if (!count) {
            if (Clock::now() >= deadline) {
                LogMessage(L"Quarbor Virtual Microphone: ring stalled for 250 ms; check that an application is recording from this microphone", LogLevel::Warning);
                return false;
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
            continue;
        }
        const auto position = static_cast<ULONG>(state.WritePosition % bufferBytes_);
        const auto first = std::min(count, bufferBytes_ - position);
        std::memcpy(ring_ + position, bytes + offset, first);
        if (count > first) std::memcpy(ring_, bytes + offset + first, count - first);
        MemoryBarrier();
        QUARBOR_MIC_COMMIT commit{sizeof(commit), QUARBOR_MIC_API_VERSION,
            state.Generation, state.WritePosition, count, 0};
        if (!Control(IOCTL_QUARBOR_MIC_COMMIT, &commit, sizeof(commit))) return false;
        offset += count;
        committedBytes_ += count;
        deadline = Clock::now() + kStallTimeout;
    }
    return true;
}

void VirtualMicrophoneSink::Stop(bool drain) {
    std::lock_guard lock(mutex_);
    if (!opened_) return;
    QUARBOR_MIC_STATE state{};
    bool queried = started_ && Query(state);
    // STOP resets the driver's ring. On normal voice end, allow queued tail PCM
    // to reach the capture client first. Shutdown and stalls never wait forever.
    const auto deadline = Clock::now() + kStallTimeout;
    while (drain && !cancelled_ && queried && state.AvailableBytes && Clock::now() < deadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
        queried = Query(state);
    }
    if (queried) {
        LogMessage(L"Quarbor Virtual Microphone: stop; committed=" + std::to_wstring(committedBytes_) +
            L", forwarded=" + std::to_wstring(state.ForwardedBytes) +
            L", queued=" + std::to_wstring(state.AvailableBytes) +
            L", silence=" + std::to_wstring(state.SilenceBytes) + L" bytes");
    }
    Control(IOCTL_QUARBOR_MIC_STOP);
    CloseLocked();
}

void VirtualMicrophoneSink::CloseLocked() {
    ring_ = nullptr;
    bufferBytes_ = 0;
    started_ = false;
    if (opened_) transport_->Close();
    opened_ = false;
}

} // namespace axonkey_service
