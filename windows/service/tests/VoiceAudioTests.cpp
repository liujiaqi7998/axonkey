#include "../VoiceAudioSession.h"
#include "../ServiceConfig.h"
#include <chrono>
#include <cstring>
#include <iostream>
#include <stdexcept>
#include <thread>

using namespace axonkey_service;
namespace {
void Require(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}

class FakeMicrophone final : public MicrophoneOutput {
public:
    explicit FakeMicrophone(bool& busy) : busy_(busy) {}
    bool Start() override {
        ++starts;
        if (busy_) return false;
        busy_ = owned = true;
        return true;
    }
    bool Reset() override { ++resets; return owned; }
    bool Push(std::span<const std::int16_t> input) override {
        if (!owned || failPush) return false;
        samples.insert(samples.end(), input.begin(), input.end());
        return true;
    }
    void Stop(bool drain) override {
        drained = drain;
        if (owned) busy_ = owned = false;
    }
    bool& busy_;
    bool owned = false, failPush = false, drained = false;
    int starts = 0, resets = 0;
    std::vector<std::int16_t> samples;
};

class FakeTransport final : public MicrophoneTransport {
public:
    std::vector<std::uint8_t> ring = std::vector<std::uint8_t>(2048);
    std::vector<std::uint8_t> committed;
    QUARBOR_MIC_STATE state{sizeof(state), QUARBOR_MIC_API_VERSION,
        QUARBOR_MIC_STATE_ONLINE | QUARBOR_MIC_STATE_MAPPED, 2048, 48000, 1, 16, 2, 1};
    bool opened = false, failCommit = false, shortMapping = false;
    int releaseOnQuery = 0, queries = 0, closes = 0;
    std::vector<DWORD> operations;

    bool Open() override { opened = true; return true; }
    void Close() override { opened = false; ++closes; }
    bool Control(DWORD code, const void* input, DWORD inputBytes, void* output,
        DWORD outputBytes, DWORD& returned) override {
        Require(opened, "I/O after handle close");
        operations.push_back(code);
        returned = 0;
        switch (code) {
        case IOCTL_QUARBOR_MIC_MAP_RING: {
            Require(outputBytes == sizeof(QUARBOR_MIC_RING_MAPPING), "map output size");
            QUARBOR_MIC_RING_MAPPING mapping{sizeof(mapping), QUARBOR_MIC_API_VERSION,
                reinterpret_cast<std::uintptr_t>(ring.data()), 2048, 48000, 1, 16, 2, 0, 1};
            std::memcpy(output, &mapping, sizeof(mapping));
            returned = shortMapping ? sizeof(mapping) - 1 : sizeof(mapping);
            break;
        }
        case IOCTL_QUARBOR_MIC_START:
            state.Flags |= QUARBOR_MIC_STATE_STARTED;
            break;
        case IOCTL_QUARBOR_MIC_STOP:
            state.Flags &= ~QUARBOR_MIC_STATE_STARTED;
            [[fallthrough]];
        case IOCTL_QUARBOR_MIC_RESET:
            ++state.Generation;
            state.WritePosition = state.ReadPosition = state.ForwardedBytes = 0;
            break;
        case IOCTL_QUARBOR_MIC_QUERY_STATE:
            ++queries;
            if (releaseOnQuery && queries >= releaseOnQuery) {
                state.ForwardedBytes += state.WritePosition - state.ReadPosition;
                state.ReadPosition = state.WritePosition;
            }
            state.AvailableBytes = static_cast<ULONG>(state.WritePosition - state.ReadPosition);
            state.FreeBytes = state.BufferBytes - state.AvailableBytes;
            Require(outputBytes == sizeof(state), "query output size");
            std::memcpy(output, &state, sizeof(state));
            returned = sizeof(state);
            break;
        case IOCTL_QUARBOR_MIC_COMMIT: {
            if (failCommit) { SetLastError(ERROR_DEVICE_NOT_CONNECTED); return false; }
            const auto& commit = *static_cast<const QUARBOR_MIC_COMMIT*>(input);
            Require(inputBytes == sizeof(commit) && commit.Size == sizeof(commit), "commit size");
            Require(commit.Version == 1 && !commit.Reserved, "commit ABI");
            Require(commit.Generation == state.Generation && commit.WritePosition == state.WritePosition,
                "stale commit generation/position");
            Require(commit.ByteCount && commit.ByteCount <= 960 && !(commit.ByteCount % 2), "commit chunk/alignment");
            Require(commit.ByteCount <= state.BufferBytes - (state.WritePosition - state.ReadPosition),
                "producer overwrote unread PCM");
            for (ULONG i = 0; i < commit.ByteCount; ++i)
                committed.push_back(ring[(state.WritePosition + i) % ring.size()]);
            state.WritePosition += commit.ByteCount;
            break;
        }
        default: throw std::runtime_error("unexpected IOCTL");
        }
        return true;
    }
};

void TestVoiceSessions() {
    const std::vector<std::uint8_t> caps{0x0b, 1, 0, 0, 3, 0, 120};
    const std::vector<std::uint8_t> packet(120, 0x11);
    bool busy = false;
    FakeMicrophone mic(busy);
    std::vector<std::vector<std::uint8_t>> commands;
    VoiceAudioSession session(mic, [&](const auto& bytes) { commands.push_back(bytes); });
    session.Control(caps);
    session.Audio(packet);
    Require(mic.starts == 1 && mic.samples.size() == 717, "first implicit AUDIO packet was lost");
    Require(mic.samples.front() == 1, "ADPCM high nibble decode");
    session.Control({0, 2});
    Require(!busy && mic.drained && mic.samples.size() == 720, "voice tail not flushed/drained");
    session.Audio(packet);
    Require(mic.starts == 1, "late audio reopened ended session");
    session.Control({8});
    Require(mic.starts == 2 && commands.back() == std::vector<std::uint8_t>({12, 0}), "open acknowledgement");
    session.Control({4, 0, 2, 7});
    Require(mic.starts == 2 && mic.resets == 1 && commands.size() == 1, "AUDIO_START reopened/acknowledged twice");

    FakeMicrophone other(busy);
    std::vector<std::vector<std::uint8_t>> replies;
    VoiceAudioSession second(other, [&](const auto& bytes) { replies.push_back(bytes); });
    second.Control(caps);
    second.Control({8});
    second.Control({4, 0, 2, 8});
    second.Audio(packet);
    Require(other.starts == 1 && replies.empty() && other.samples.empty(), "busy session was accepted");
    session.Control({0, 2});
    second.Audio(packet);
    Require(other.starts == 1 && !busy, "rejected session took over mid-utterance");
    second.Control({0, 2});
    second.Control({8});
    Require(other.starts == 2 && busy && replies.size() == 1, "new session could not acquire microphone");
    second.Control({4, 0, 2, 9});
    other.failPush = true;
    second.Audio(packet);
    Require(!busy, "write failure retained exclusive microphone");
    second.Audio(packet);
    Require(other.starts == 2, "failed write retried midway through ADPCM stream");
    second.CloseRemote();
    Require(replies.back() == std::vector<std::uint8_t>({13, 9}), "remote close session ID");

    AdpcmDecoder decoder;
    decoder.SetFrameBytes(1);
    auto decoded = decoder.Append({0x17});
    Require(decoded == std::vector<std::int16_t>({1, 4, 8}), "ADPCM/interpolation known vector");
    Require(decoder.Flush() == std::vector<std::int16_t>({12, 12, 12}), "resampler tail length");
    AdpcmDecoder whole, split;
    auto expected = whole.Append(packet);
    Require(split.Append(std::vector<std::uint8_t>(packet.begin(), packet.begin() + 47)).empty(),
        "partial ADPCM frame decoded prematurely");
    Require(split.Append(std::vector<std::uint8_t>(packet.begin() + 47, packet.end())) == expected,
        "fragmented AUDIO decode differs from whole frame");
}

void TestRingProducer() {
    auto transport = std::make_unique<FakeTransport>();
    auto* fake = transport.get();
    VirtualMicrophoneSink sink(std::move(transport));
    Require(sink.Start(), "sink start");
    Require(fake->operations == std::vector<DWORD>({IOCTL_QUARBOR_MIC_MAP_RING,
        IOCTL_QUARBOR_MIC_RESET, IOCTL_QUARBOR_MIC_START}), "driver initialization order");
    // Start close to the ring boundary to exercise both wrap and 960-byte chunks.
    fake->state.WritePosition = fake->state.ReadPosition = 2044;
    std::vector<std::int16_t> pcm(700);
    for (size_t i = 0; i < pcm.size(); ++i) pcm[i] = static_cast<std::int16_t>(i * 7 - 2000);
    Require(sink.Push(pcm), "wrapped PCM write");
    Require(fake->committed.size() == pcm.size() * 2 &&
        !std::memcmp(fake->committed.data(), pcm.data(), fake->committed.size()), "PCM changed/reordered at wrap");
    fake->state.WritePosition = fake->state.ReadPosition + fake->ring.size();
    fake->releaseOnQuery = fake->queries + 3;
    Require(sink.Push(pcm), "full ring dropped PCM instead of waiting");
    Require(fake->committed.size() == pcm.size() * 4, "backpressure lost samples");
    sink.Stop(true);
    Require(fake->closes == 1 && !fake->opened, "driver not closed");
    sink.Stop();
    Require(fake->closes == 1, "duplicate close");
    Require(sink.Start(), "sink reopen");
    fake->failCommit = true;
    Require(!sink.Push(pcm), "COMMIT failure ignored");
    sink.Stop();

    auto bad = std::make_unique<FakeTransport>();
    auto* badView = bad.get();
    bad->shortMapping = true;
    VirtualMicrophoneSink invalid(std::move(bad));
    Require(!invalid.Start() && badView->closes == 1, "short mapping used as PCM memory");

    auto full = std::make_unique<FakeTransport>();
    auto* fullView = full.get();
    VirtualMicrophoneSink cancellable(std::move(full));
    Require(cancellable.Start(), "cancellation setup");
    fullView->state.WritePosition = fullView->ring.size();
    std::jthread cancel([&] {
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
        cancellable.Cancel();
    });
    Require(!cancellable.Push(pcm), "cancel did not interrupt full ring");
    cancel.join();
    cancellable.Stop();
    Require(!cancellable.Start() && fullView->closes == 1, "shutdown reopened producer");
}

void TestAudioGain() {
    std::vector<std::int16_t> samples{0, 1000, -1000, 30000, -30000, 32767, -32768};
    const auto original = samples;
    AudioGain(0).Apply(samples);
    Require(samples == original, "0 dB changed PCM");
    AudioGain().Apply(samples);
    Require(samples == std::vector<std::int16_t>({0, 1259, -1259, 32767, -32768, 32767, -32768}),
        "2 dB amplitude/rounding/saturation incorrect");
    samples = {1000, -1000};
    AudioGain(6).Apply(samples);
    Require(samples == std::vector<std::int16_t>({1995, -1995}), "configured gain ignored");
    samples = {1000, -1000};
    AudioGain(-30).Apply(samples);
    Require(samples == std::vector<std::int16_t>({32, -32}), "-30 dB attenuation incorrect");
    samples = {1000, -1000};
    AudioGain(30).Apply(samples);
    Require(samples == std::vector<std::int16_t>({31623, -31623}), "+30 dB amplification incorrect");

    // Verify gain on the real decode -> resample -> output path, including tail.
    for (const auto gain : {-6, 0, 2}) {
        bool busy = false;
        FakeMicrophone mic(busy);
        VoiceAudioSession session(mic, [](const auto&) {}, gain);
        session.Control({0x0b, 1, 0, 0, 3, 0, 1});
        session.Audio({0x17});
        session.Control({0, 2});
        const auto expected = gain == -6 ? std::vector<std::int16_t>{1, 2, 4, 6, 6, 6} :
            (gain == 0 ? std::vector<std::int16_t>{1, 4, 8, 12, 12, 12} :
                std::vector<std::int16_t>{1, 5, 10, 15, 15, 15});
        Require(mic.samples == expected, "session gain/tail applied incorrectly");
    }
}

void TestGainRegistry() {
    // Use an isolated per-run HKCU key; tests never modify service configuration.
    struct TestKey {
        std::wstring path = L"Software\\Axonkey\\Tests\\AudioGain-" +
            std::to_wstring(GetCurrentProcessId()) + L"-" + std::to_wstring(GetTickCount64());
        HKEY key = nullptr;
        TestKey() {
            Require(RegCreateKeyExW(HKEY_CURRENT_USER, path.c_str(), 0, nullptr, 0,
                KEY_ALL_ACCESS, nullptr, &key, nullptr) == ERROR_SUCCESS, "test registry open");
        }
        ~TestKey() { RegCloseKey(key); RegDeleteKeyW(HKEY_CURRENT_USER, path.c_str()); }
        void Write(std::int32_t value) {
            Require(RegSetValueExW(key, L"AudioGainDb", 0, REG_DWORD,
                reinterpret_cast<const BYTE*>(&value), sizeof(value)) == ERROR_SUCCESS, "write test gain");
        }
        std::int32_t Read() {
            std::int32_t value = 0;
            DWORD type = 0, size = sizeof(value);
            Require(RegQueryValueExW(key, L"AudioGainDb", nullptr, &type,
                reinterpret_cast<BYTE*>(&value), &size) == ERROR_SUCCESS && type == REG_DWORD && size == 4,
                "gain not stored as DWORD");
            return value;
        }
    } test;
    Require(LoadServiceConfigFromKey(test.key).audioGainDb == 2 && test.Read() == 2, "default gain not persisted");
    for (const std::int32_t value : {-30, -6, 0, 6, 30}) {
        test.Write(value);
        Require(LoadServiceConfigFromKey(test.key).audioGainDb == value && test.Read() == value,
            "valid configured gain overwritten");
    }
    for (const auto value : {-31, 31}) {
        test.Write(value);
        Require(LoadServiceConfigFromKey(test.key).audioGainDb == 2 && test.Read() == 2, "invalid gain not repaired");
    }
    const wchar_t wrongType[] = L"6";
    Require(RegSetValueExW(test.key, L"AudioGainDb", 0, REG_SZ,
        reinterpret_cast<const BYTE*>(wrongType), sizeof(wrongType)) == ERROR_SUCCESS, "set wrong type");
    Require(LoadServiceConfigFromKey(test.key).audioGainDb == 2 && test.Read() == 2, "wrong gain type accepted");
}
}

int main() {
    try {
        TestVoiceSessions();
        TestRingProducer();
        TestAudioGain();
        TestGainRegistry();
        std::cout << "PASS: first packet, ADPCM framing, tail, ownership, failures, ring wrap/backpressure/cancellation\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
