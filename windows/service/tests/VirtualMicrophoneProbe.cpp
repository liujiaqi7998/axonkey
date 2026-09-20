// Hardware diagnostic. Explicitly run as administrator; never part of CTest.
// Injects a generated tone using the service's real producer, then verifies
// capture from Quarbor Virtual Microphone only (never the default microphone).
#include "../VirtualMicrophoneSink.h"
#include <mmdeviceapi.h>
#include <audioclient.h>
#include <functiondiscoverykeys_devpkey.h>
#include <wrl/client.h>
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstring>
#include <cwctype>
#include <iostream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

using Microsoft::WRL::ComPtr;
using namespace axonkey_service;
namespace {
void Check(HRESULT result, const char* operation) {
    if (FAILED(result)) {
        std::cerr << operation << " failed: HRESULT=0x" << std::hex <<
            static_cast<unsigned long>(result) << std::dec << '\n';
        throw std::runtime_error(operation);
    }
}
struct ComScope {
    ComScope() { Check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "CoInitializeEx"); }
    ~ComScope() { CoUninitialize(); }
};
struct CaptureScope {
    ComPtr<IAudioClient> client;
    ~CaptureScope() { if (client) client->Stop(); }
};

ComPtr<IMMDevice> FindQuarborMicrophone() {
    ComPtr<IMMDeviceEnumerator> enumerator;
    Check(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL,
        IID_PPV_ARGS(&enumerator)), "Create audio enumerator");
    ComPtr<IMMDeviceCollection> devices;
    Check(enumerator->EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE, &devices), "Enumerate capture endpoints");
    UINT count = 0;
    Check(devices->GetCount(&count), "Endpoint count");
    ComPtr<IMMDevice> target;
    for (UINT i = 0; i < count; ++i) {
        ComPtr<IMMDevice> device;
        ComPtr<IPropertyStore> properties;
        Check(devices->Item(i, &device), "Get endpoint");
        Check(device->OpenPropertyStore(STGM_READ, &properties), "Endpoint properties");
        PROPVARIANT name{};
        Check(properties->GetValue(PKEY_Device_FriendlyName, &name), "Endpoint name");
        std::wstring text = name.vt == VT_LPWSTR ? name.pwszVal : L"";
        PropVariantClear(&name);
        std::transform(text.begin(), text.end(), text.begin(),
            [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });
        if (text.find(L"quarbor virtual microphone") == std::wstring::npos) continue;
        if (target) throw std::runtime_error("Multiple Quarbor capture endpoints; disable the duplicate before testing");
        target = device;
    }
    if (!target) throw std::runtime_error("No active Quarbor Virtual Microphone capture endpoint");
    return target;
}

std::int16_t Tone(size_t frame) {
    return static_cast<std::int16_t>(std::sin(6.283185307179586 *
        static_cast<double>(frame % 48) / 48.0) * 12000.0);
}
}

int main() {
    try {
        ComScope apartment;
        auto device = FindQuarborMicrophone();
        VirtualMicrophoneSink sink;
        // Verify the privileged ingress first and emit its Win32 diagnostic.
        if (!sink.Start()) throw std::runtime_error(
            "Cannot open/start PCM ingress. Run as administrator; close other producers. See diagnostic above");
        CaptureScope captureScope;
        Check(device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr,
            reinterpret_cast<void**>(captureScope.client.GetAddressOf())), "Activate Quarbor capture");
        auto& client = captureScope.client;
        WAVEFORMATEX format{WAVE_FORMAT_PCM, 1, 48000, 96000, 2, 16, 0};
        Check(client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 1000000, 0, &format, nullptr),
            "Initialize 48 kHz mono PCM16 capture");
        ComPtr<IAudioCaptureClient> capture;
        Check(client->GetService(IID_PPV_ARGS(&capture)), "Capture client");
        Check(client->Start(), "Start capture");

        using Clock = std::chrono::steady_clock;
        constexpr size_t totalFrames = 96000; // Two seconds at 48 kHz.
        size_t injected = 0;
        unsigned discontinuities = 0;
        std::vector<std::int16_t> recorded;
        recorded.reserve(totalFrames + 24000);
        const auto started = Clock::now();
        auto nextWrite = started;
        while (Clock::now() - started < std::chrono::milliseconds(2300)) {
            // Windows Sleep(1) may wake much later than 1 ms. Catch up with the
            // elapsed audio clock instead of losing one chunk on every wakeup.
            while (injected < totalFrames && Clock::now() >= nextWrite) {
                std::vector<std::int16_t> pcm(480);
                for (size_t i = 0; i < pcm.size(); ++i) pcm[i] = Tone(injected + i);
                if (!sink.Push(pcm)) throw std::runtime_error("PCM injection failed; see diagnostic above");
                injected += pcm.size();
                nextWrite += std::chrono::milliseconds(10);
            }
            UINT available = 0;
            Check(capture->GetNextPacketSize(&available), "Capture packet size");
            while (available) {
                BYTE* data = nullptr;
                UINT frames = 0;
                DWORD flags = 0;
                Check(capture->GetBuffer(&data, &frames, &flags, nullptr, nullptr), "Capture buffer");
                // Some devices report a discontinuity on their first packet.
                if ((flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY) && !recorded.empty()) ++discontinuities;
                const auto offset = recorded.size();
                recorded.resize(offset + frames, 0);
                if (!(flags & AUDCLNT_BUFFERFLAGS_SILENT) && data)
                    std::memcpy(recorded.data() + offset, data, static_cast<size_t>(frames) * 2);
                Check(capture->ReleaseBuffer(frames), "Release capture buffer");
                Check(capture->GetNextPacketSize(&available), "Next capture packet");
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
        sink.Stop(true);
        Check(client->Stop(), "Stop capture");
        // Allow a phase offset caused by capture startup latency. Silence cannot
        // pass: the known tone has nonzero amplitude over most of each period.
        size_t matched = 0;
        for (size_t phase = 0; phase < 48; ++phase) {
            size_t count = 0;
            for (size_t i = 0; i < recorded.size(); ++i)
                if (std::abs(static_cast<int>(recorded[i]) - Tone(i + phase)) < 800) ++count;
            matched = std::max(matched, count);
        }
        const auto nonzero = std::count_if(recorded.begin(), recorded.end(), [](auto s) { return s != 0; });
        const bool passed = injected == totalFrames && recorded.size() >= totalFrames &&
            nonzero >= totalFrames * 8 / 10 && matched >= totalFrames * 8 / 10 && !discontinuities;
        std::cout << (passed ? "PASS" : "FAIL") << ": Quarbor Virtual Microphone; injected=" << injected <<
            ", captured=" << recorded.size() << ", matched=" << matched <<
            ", nonzero=" << nonzero << ", discontinuities=" << discontinuities << '\n';
        return passed ? 0 : 1;
    } catch (const std::exception& error) {
        std::cerr << "FAIL: " << error.what() << '\n';
        return 1;
    }
}
