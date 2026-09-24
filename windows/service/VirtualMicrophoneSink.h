#pragma once

#include <windows.h>
#include <QuarborVirtualMicrophone.h>
#include <atomic>
#include <cstdint>
#include <memory>
#include <mutex>
#include <span>

namespace axonkey_service {

// VoiceAudioSession uses this interface so protocol tests need no installed driver.
class MicrophoneOutput {
public:
    virtual ~MicrophoneOutput() = default;
    virtual bool Start() = 0;
    virtual bool Reset() = 0;
    virtual bool Push(std::span<const std::int16_t> samples) = 0;
    virtual void Stop(bool drain = false) = 0;
    virtual DWORD LastError() const noexcept { return ERROR_GEN_FAILURE; }
};

// Isolates OS I/O for tests of the actual ring producer (including backpressure).
class MicrophoneTransport {
public:
    virtual ~MicrophoneTransport() = default;
    virtual bool Open() = 0;
    virtual void Close() = 0;
    virtual bool Control(DWORD code, const void* input, DWORD inputBytes,
        void* output, DWORD outputBytes, DWORD& returned) = 0;
};

class VirtualMicrophoneSink final : public MicrophoneOutput {
public:
    VirtualMicrophoneSink();
    explicit VirtualMicrophoneSink(std::unique_ptr<MicrophoneTransport> transport);
    ~VirtualMicrophoneSink() override;
    bool Start() override;
    bool Reset() override;
    bool Push(std::span<const std::int16_t> samples) override;
    void Stop(bool drain = false) override;
    DWORD LastError() const noexcept override { return lastError_.load(); }
    // Terminal cancellation: prevents new opens and interrupts waits before join.
    void Cancel() noexcept { cancelled_.store(true); }

private:
    bool Control(DWORD code, const void* input = nullptr, DWORD inputBytes = 0,
        void* output = nullptr, DWORD outputBytes = 0, DWORD* returned = nullptr);
    bool Query(QUARBOR_MIC_STATE& state);
    void CloseLocked();
    void SetError(DWORD error) noexcept { lastError_.store(error ? error : ERROR_GEN_FAILURE); }
    std::unique_ptr<MicrophoneTransport> transport_;
    std::atomic_bool cancelled_{false};
    std::atomic<DWORD> lastError_{ERROR_SUCCESS};
    std::mutex mutex_;
    std::uint8_t* ring_ = nullptr;
    ULONG bufferBytes_ = 0;
    bool opened_ = false;
    bool started_ = false;
    std::uint64_t committedBytes_ = 0;
};

} // namespace axonkey_service
