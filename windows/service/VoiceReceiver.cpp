#include "VoiceReceiver.h"

#include <windows.h>
#include "VoiceAudioSession.h"

#include <winrt/base.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Devices.Enumeration.h>
#include <winrt/Windows.Devices.Bluetooth.h>
#include <winrt/Windows.Devices.Bluetooth.GenericAttributeProfile.h>
#include <winrt/Windows.Storage.Streams.h>
#include "GattAccess.h"
#include "ServiceLog.h"

#include <algorithm>
#include <array>
#include <condition_variable>
#include <cstdint>
#include <cwctype>
#include <cstring>
#include <deque>
#include <mutex>
#include <chrono>
#include <optional>
#include <thread>
#include <utility>
#include <vector>
#include <new>

namespace axonkey_service {
namespace {
using namespace winrt;
using namespace Windows::Devices::Bluetooth;
using namespace Windows::Devices::Bluetooth::GenericAttributeProfile;
using namespace Windows::Devices::Enumeration;
using namespace Windows::Foundation;
using namespace Windows::Storage::Streams;

constexpr wchar_t kVoiceServiceUuid[] = L"{AB5E0001-5A21-4F05-BC7D-AF01F617B664}";
constexpr wchar_t kTransmitUuid[] = L"{AB5E0002-5A21-4F05-BC7D-AF01F617B664}";
constexpr wchar_t kAudioUuid[] = L"{AB5E0003-5A21-4F05-BC7D-AF01F617B664}";
constexpr wchar_t kControlUuid[] = L"{AB5E0004-5A21-4F05-BC7D-AF01F617B664}";
constexpr wchar_t kBatteryServiceUuid[] = L"{0000180F-0000-1000-8000-00805F9B34FB}";
constexpr wchar_t kBatteryLevelUuid[] = L"{00002A19-0000-1000-8000-00805F9B34FB}";

const guid Guid(const wchar_t* text) { return guid{std::wstring_view(text)}; }

void LogError(const wchar_t* prefix, const hresult_error& error) noexcept {
    try {
        wchar_t code[32]{};
        swprintf_s(code, L" (HRESULT=0x%08X)", static_cast<unsigned int>(error.code().value));
        LogMessage(std::wstring(prefix) + L": " + error.message().c_str() + code, LogLevel::Error);
    } catch (...) {}
}

std::wstring Lower(std::wstring value) {
    std::transform(value.begin(), value.end(), value.begin(), towlower);
    return value;
}

bool IsTargetServiceId(const std::wstring& serviceId, const std::wstring& instanceId) {
    const auto candidate = Lower(serviceId);
    const auto target = Lower(instanceId);
    // Windows GATT instance IDs usually preserve the BTHLE hardware ID. Use
    // the supplied HID instance as the primary identity and accept the
    // canonical Bluetooth form when Windows omits the HID prefix.
    const bool targetHardware = target.find(L"vid_2717") != std::wstring::npos ||
        target.find(L"vid&012717") != std::wstring::npos;
    const bool targetProduct = target.find(L"pid_32b8") != std::wstring::npos ||
        target.find(L"pid&32b8") != std::wstring::npos;
    if (!targetHardware || !targetProduct) return false;
    return candidate.find(L"vid&012717_pid&32b8") != std::wstring::npos ||
        candidate.find(L"vid_2717&pid_32b8") != std::wstring::npos;
}

struct VoiceEvent {
    bool audio = false;
    std::vector<std::uint8_t> bytes;
};

} // namespace

struct VoiceReceiver::State {
    std::mutex mutex;
    std::condition_variable wake;
    std::deque<VoiceEvent> events;
    size_t queuedBytes = 0;
    bool overflow = false;
    bool stop = false;
    bool finished = false;
    bool connected = false;
    bool active = false;
    bool microphoneOpen = false;
    std::optional<std::uint8_t> batteryLevel;
    std::uint16_t protocolVersion = 0;
    std::uint8_t sessionId = 0;
    VirtualMicrophoneSink microphone;
};

namespace {

void EnqueueEvent(const std::shared_ptr<VoiceReceiver::State>& state,
    bool audio, std::vector<std::uint8_t> bytes) {
    if (bytes.empty()) return;
    std::lock_guard lock(state->mutex);
    if (state->stop || state->overflow) return;
    // Bound memory if Bluetooth outpaces the driver or the consumer stops.
    // Losing ADPCM bytes corrupts subsequent decode: fail the connection rather
    // than silently continuing with a damaged predictor or dropping CTL_STOP.
    if (state->queuedBytes + bytes.size() > 65536 || state->events.size() >= 1024) {
        state->overflow = true;
    } else {
        const auto size = bytes.size();
        state->events.push_back({audio, std::move(bytes)});
        state->queuedBytes += size;
    }
    state->wake.notify_one();
}

std::vector<std::uint8_t> EventBytes(const GattValueChangedEventArgs& args) {
    const auto buffer = args.CharacteristicValue();
    const auto reader = DataReader::FromBuffer(buffer);
    std::vector<std::uint8_t> bytes(reader.UnconsumedBufferLength());
    reader.ReadBytes(bytes);
    return bytes;
}

void EnableNotifications(const GattCharacteristic& characteristic) {
    const auto properties = characteristic.CharacteristicProperties();
    const auto mode = (properties & GattCharacteristicProperties::Notify) != GattCharacteristicProperties::None
        ? GattClientCharacteristicConfigurationDescriptorValue::Notify
        : GattClientCharacteristicConfigurationDescriptorValue::Indicate;
    if ((properties & (GattCharacteristicProperties::Notify | GattCharacteristicProperties::Indicate)) == GattCharacteristicProperties::None)
        throw hresult_error(E_NOTIMPL, L"RC003 characteristic does not support notifications");
    if (characteristic.WriteClientCharacteristicConfigurationDescriptorAsync(mode).get() != GattCommunicationStatus::Success)
        throw hresult_error(E_FAIL, L"RC003 notification subscription failed");
}

void WriteCharacteristic(const GattCharacteristic& characteristic, const std::vector<std::uint8_t>& bytes) {
    DataWriter writer;
    writer.WriteBytes(bytes);
    const auto buffer = writer.DetachBuffer();
    const auto properties = characteristic.CharacteristicProperties();
    const auto option = (properties & GattCharacteristicProperties::WriteWithoutResponse) != GattCharacteristicProperties::None
        ? GattWriteOption::WriteWithoutResponse : GattWriteOption::WriteWithResponse;
    if (characteristic.WriteValueAsync(buffer, option).get() != GattCommunicationStatus::Success)
        throw hresult_error(E_FAIL, L"RC003 command write failed");
}

struct GattConnection {
    BluetoothLEDevice device{nullptr};
    GattDeviceService service{nullptr};
    GattCharacteristic transmit{nullptr};
    GattCharacteristic audio{nullptr};
    GattCharacteristic control{nullptr};
    event_token audioToken{};
    event_token controlToken{};
    bool audioSubscribed = false;
    bool controlSubscribed = false;
};

std::optional<std::uint8_t> ReadBatteryLevel(const BluetoothLEDevice& device) {
    if (!device) return {};
    auto readWithMode = [&](BluetoothCacheMode mode) -> std::optional<std::uint8_t> {
        const auto servicesResult = device.GetGattServicesForUuidAsync(
            Guid(kBatteryServiceUuid), mode).get();
        if (!servicesResult || servicesResult.Status() != GattCommunicationStatus::Success)
            return {};
        const auto services = servicesResult.Services();
        if (!services) return {};
        for (std::uint32_t index = 0; index < services.Size(); ++index) {
            const auto service = services.GetAt(index);
            if (!service) continue;
            const auto characteristicResult = service.GetCharacteristicsForUuidAsync(
                Guid(kBatteryLevelUuid), mode).get();
            if (!characteristicResult ||
                    characteristicResult.Status() != GattCommunicationStatus::Success)
                continue;
            const auto characteristics = characteristicResult.Characteristics();
            if (!characteristics || characteristics.Size() == 0) continue;
            const auto characteristic = characteristics.GetAt(0);
            if (!characteristic) continue;
            const auto read = characteristic.ReadValueAsync(mode).get();
            if (!read || read.Status() != GattCommunicationStatus::Success || !read.Value())
                continue;
            const auto reader = DataReader::FromBuffer(read.Value());
            if (!reader || reader.UnconsumedBufferLength() == 0) continue;
            const auto level = reader.ReadByte();
            if (level <= 100) return level;
        }
        return {};
    };

    // An active voice GATT connection makes an uncached read reliable. Keep a
    // cached retry for devices that expose the value only after discovery.
    for (const auto mode : {BluetoothCacheMode::Uncached, BluetoothCacheMode::Cached}) {
        try {
            if (const auto level = readWithMode(mode)) return level;
        } catch (const hresult_error& error) {
            LogError(L"RC003 battery read", error);
        } catch (...) {
            LogMessage(L"RC003 battery read failed", LogLevel::Error);
        }
    }
    return {};
}

void CloseGatt(GattConnection& connection) noexcept {
    if (connection.audioSubscribed) {
        try { connection.audio.ValueChanged(connection.audioToken); } catch (...) {
            LogMessage(L"RC003 AUDIO subscription cleanup failed", LogLevel::Warning);
        }
        connection.audioSubscribed = false;
    }
    if (connection.controlSubscribed) {
        try { connection.control.ValueChanged(connection.controlToken); } catch (...) {
            LogMessage(L"RC003 CTL subscription cleanup failed", LogLevel::Warning);
        }
        connection.controlSubscribed = false;
    }
    CloseGattObject(connection.service);
    CloseGattObject(connection.device);
}

void MarkFinished(const std::shared_ptr<VoiceReceiver::State>& state) {
    std::lock_guard lock(state->mutex);
    state->finished = true;
    state->wake.notify_all();
}

GattConnection ConnectGatt(const std::wstring& instanceId) {
    const auto selector = GattDeviceService::GetDeviceSelectorFromUuid(Guid(kVoiceServiceUuid));
    const auto services = DeviceInformation::FindAllAsync(selector).get();
    for (uint32_t index = 0; index < services.Size(); ++index) {
        const auto info = services.GetAt(index);
        // Own the hstring: info.Id().c_str() alone dangles after the statement.
        const auto serviceId = info.Id();
        if (!IsTargetServiceId(std::wstring(serviceId), instanceId)) continue;
        GattConnection connection;
        try {
            connection.service = GattDeviceService::FromIdAsync(serviceId).get();
            const auto session = RequireGattSession(connection.service);
            const auto deviceId = session.DeviceId();
            if (!deviceId)
                throw hresult_error(HRESULT_FROM_WIN32(ERROR_NOT_READY), L"RC003 Bluetooth device ID is unavailable");
            connection.device = BluetoothLEDevice::FromIdAsync(deviceId.Id()).get();
            if (!connection.device)
                throw hresult_error(HRESULT_FROM_WIN32(ERROR_NOT_READY), L"Opening RC003 Bluetooth device returned null");
            connection.transmit = FindCharacteristic(connection.service, Guid(kTransmitUuid));
            connection.audio = FindCharacteristic(connection.service, Guid(kAudioUuid));
            connection.control = FindCharacteristic(connection.service, Guid(kControlUuid));
            return connection;
        } catch (const hresult_error& error) {
            LogError(L"RC003 ATVV discovery", error);
            CloseGattObject(connection.service);
            CloseGattObject(connection.device);
        }
    }
    throw hresult_error(HRESULT_FROM_WIN32(ERROR_NOT_FOUND), L"RC003 ATVV service was not found");
}

} // namespace

VoiceReceiver::VoiceReceiver(std::wstring deviceInstanceId, std::int32_t audioGainDb,
    VoiceAudioSession::LevelCallback level, StatusCallback status, IssueCallback issue)
    : deviceInstanceId_(std::move(deviceInstanceId)), audioGainDb_(audioGainDb),
      level_(std::move(level)), statusCallback_(std::move(status)), issueCallback_(std::move(issue)),
      state_(std::make_shared<State>()) {}

VoiceReceiver::~VoiceReceiver() { Stop(); }

void VoiceReceiver::Start() {
    std::lock_guard lock(lifecycleMutex_);
    if (worker_.joinable()) return;
    {
        std::lock_guard stateLock(state_->mutex);
        state_->stop = false;
        state_->finished = false;
        state_->connected = false;
        state_->active = false;
        state_->microphoneOpen = false;
        state_->batteryLevel.reset();
    }
    worker_ = std::thread(&VoiceReceiver::Run, this);
}

void VoiceReceiver::Stop() {
    auto state = state_;
    {
        std::lock_guard lock(state->mutex);
        state->stop = true;
        state->wake.notify_all();
    }
    state->microphone.Cancel();
    state->microphone.Stop();
    std::lock_guard lock(lifecycleMutex_);
    if (worker_.joinable()) worker_.join();
}

bool VoiceReceiver::Finished() const {
    std::lock_guard lock(state_->mutex);
    return state_->finished;
}

std::optional<std::uint8_t> VoiceReceiver::BatteryLevel() const {
    std::lock_guard lock(state_->mutex);
    return state_->batteryLevel;
}

axonkey::rpc::VoiceStatus VoiceReceiver::Status() const {
    std::lock_guard lock(state_->mutex);
    axonkey::rpc::VoiceStatus result;
    result.state = state_->finished ? "stopped" : (state_->connected ? "connected" : "connecting");
    result.connected = state_->connected;
    result.active = state_->active;
    result.microphoneOpen = state_->microphoneOpen;
    result.protocolVersion = state_->protocolVersion;
    result.sessionId = state_->sessionId;
    for (const auto ch : deviceInstanceId_) if (ch < 0x80) result.deviceInstanceId.push_back(static_cast<char>(ch));
    return result;
}

void VoiceReceiver::Run() {
    auto state = state_;
    std::optional<GattConnection> connection;
    std::unique_ptr<VoiceAudioSession> session;
    bool apartmentInitialized = false;
    bool connected = false;
    auto publishIssue = [this](const char* code, const char* message, DWORD nativeError,
                               bool recoverable = true) noexcept {
        if (!issueCallback_) return;
        try {
            axonkey::rpc::ServiceIssue issue;
            issue.code = code;
            issue.message = message;
            issue.nativeError = nativeError;
            issue.recoverable = recoverable;
            issue.timestampMs = static_cast<std::uint64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(
                std::chrono::system_clock::now().time_since_epoch()).count());
            for (const auto ch : deviceInstanceId_) {
                if (ch < 0x80) issue.deviceInstanceId.push_back(static_cast<char>(ch));
            }
            issueCallback_(issue);
        } catch (...) {
            LogMessage(L"RC003 issue notification failed", LogLevel::Warning);
        }
    };
    try {
        init_apartment(apartment_type::multi_threaded);
        apartmentInitialized = true;
        connection = ConnectGatt(deviceInstanceId_);
        connected = true;
        const auto initialBattery = ReadBatteryLevel(connection->device);
        {
            std::lock_guard lock(state->mutex);
            state->connected = true;
            state->batteryLevel = initialBattery;
        }
        if (initialBattery) {
            LogMessage(L"RC003 battery level read: " + std::to_wstring(*initialBattery) +
                L"%; device=" + deviceInstanceId_);
        } else {
            LogMessage(L"RC003 battery level unavailable; device=" + deviceInstanceId_, LogLevel::Warning);
        }
        if (statusCallback_) statusCallback_(Status());
        session = std::make_unique<VoiceAudioSession>(state->microphone,
            [&](const auto& bytes) { WriteCharacteristic(connection->transmit, bytes); }, audioGainDb_.load(), level_,
            [&](const char* code, DWORD error) {
                publishIssue(code, std::strcmp(code, "virtual_microphone_unavailable") == 0
                    ? "VirtualMicrophone driver is unavailable"
                    : "VirtualMicrophone write failed", error);
            });
        auto weak = std::weak_ptr<State>(state);
        auto audioHandler = TypedEventHandler<GattCharacteristic, GattValueChangedEventArgs>(
            [weak, publishIssue](auto const&, auto const& args) {
                if (auto locked = weak.lock()) {
                    try { EnqueueEvent(locked, true, EventBytes(args)); }
                    catch (const hresult_error& error) { LogError(L"RC003 AUDIO callback", error); }
                    catch (const std::bad_alloc&) {
                        publishIssue("memory_allocation_failed", "RC003 AUDIO event allocation failed", ERROR_NOT_ENOUGH_MEMORY);
                    } catch (...) { LogMessage(L"RC003 AUDIO callback failed", LogLevel::Error); }
                }
            });
        auto controlHandler = TypedEventHandler<GattCharacteristic, GattValueChangedEventArgs>(
            [weak, publishIssue](auto const&, auto const& args) {
                if (auto locked = weak.lock()) {
                    try { EnqueueEvent(locked, false, EventBytes(args)); }
                    catch (const hresult_error& error) { LogError(L"RC003 CTL callback", error); }
                    catch (const std::bad_alloc&) {
                        publishIssue("memory_allocation_failed", "RC003 CTL event allocation failed", ERROR_NOT_ENOUGH_MEMORY);
                    } catch (...) { LogMessage(L"RC003 CTL callback failed", LogLevel::Error); }
                }
            });
        connection->audioToken = connection->audio.ValueChanged(audioHandler);
        connection->audioSubscribed = true;
        connection->controlToken = connection->control.ValueChanged(controlHandler);
        connection->controlSubscribed = true;
        EnableNotifications(connection->audio);
        EnableNotifications(connection->control);
        WriteCharacteristic(connection->transmit, {0x0a, 0x01, 0x00, 0x00, 0x03, 0x03});
        LogMessage(L"RC003 ATVV notifications ready; GET_CAPS sent; device=" + deviceInstanceId_);
        auto nextBatteryRefresh = std::chrono::steady_clock::now() + std::chrono::seconds(30);

        while (true) {
            VoiceEvent event;
            {
                std::unique_lock lock(state->mutex);
                state->wake.wait_for(lock, std::chrono::milliseconds(100), [&] {
                    return state->stop || state->overflow || !state->events.empty();
                });
                if (state->stop) break;
                if (state->overflow)
                    throw hresult_error(HRESULT_FROM_WIN32(ERROR_BUFFER_OVERFLOW), L"RC003 voice event queue overflow");
                if (!state->events.empty()) {
                    event = std::move(state->events.front());
                    state->events.pop_front();
                    state->queuedBytes -= event.bytes.size();
                }
            }
            const auto now = std::chrono::steady_clock::now();
            if (now >= nextBatteryRefresh) {
                if (const auto battery = ReadBatteryLevel(connection->device)) {
                    std::lock_guard lock(state->mutex);
                    state->batteryLevel = battery;
                }
                nextBatteryRefresh = now + std::chrono::seconds(30);
            }
            if (connection->device.ConnectionStatus() == BluetoothConnectionStatus::Disconnected)
                throw hresult_error(HRESULT_FROM_WIN32(ERROR_DEVICE_NOT_CONNECTED),
                    L"RC003 voice channel disconnected");
            session->SetGain(audioGainDb_.load());
            if (event.audio) session->Audio(event.bytes);
            else session->Control(event.bytes);
            {
                std::lock_guard lock(state->mutex);
                state->active = session->Active();
                state->microphoneOpen = session->MicrophoneOpen();
                state->protocolVersion = session->ProtocolVersion();
                state->sessionId = session->SessionId();
            }
            if (statusCallback_) statusCallback_(Status());
        }
    } catch (const hresult_error& error) {
        LogError(L"RC003 voice receiver", error);
        publishIssue(connected ? "bluetooth_runtime_failed" : "bluetooth_initialization_failed",
            connected ? "RC003 Bluetooth voice channel stopped" : "RC003 Bluetooth initialization failed",
            static_cast<DWORD>(error.code()));
    } catch (const std::bad_alloc&) {
        LogMessage(L"RC003 voice receiver ran out of memory", LogLevel::Error);
        publishIssue("memory_allocation_failed", "RC003 voice receiver ran out of memory", ERROR_NOT_ENOUGH_MEMORY);
    } catch (...) {
        LogMessage(L"RC003 voice receiver stopped by an unknown error", LogLevel::Error);
        publishIssue(connected ? "bluetooth_runtime_failed" : "bluetooth_initialization_failed",
            connected ? "RC003 Bluetooth voice channel stopped" : "RC003 Bluetooth initialization failed",
            ERROR_GEN_FAILURE);
    }

    // Reject any in-flight callback before releasing the producer mapping. The
    // worker is the sole protocol consumer; shutdown cannot reopen the sink.
    {
        std::lock_guard lock(state->mutex);
        state->stop = true;
    }
    state->microphone.Cancel();
    state->microphone.Stop();
    if (session) {
        try { session->CloseRemote(); }
        catch (const hresult_error& error) { LogError(L"RC003 voice close command", error); }
        catch (...) { LogMessage(L"RC003 voice close command failed", LogLevel::Error); }
        session.reset();
    }
    if (connection) {
        CloseGatt(*connection);
        connection.reset();
    }
    {
        std::lock_guard lock(state->mutex);
        state->connected = false;
        state->active = false;
        state->microphoneOpen = false;
        state->batteryLevel.reset();
        state->events.clear();
        state->queuedBytes = 0;
    }
    if (apartmentInitialized) {
        try { uninit_apartment(); } catch (...) {}
    }
    LogMessage(L"RC003 voice receiver stopped; device=" + deviceInstanceId_);
    try {
        if (statusCallback_) statusCallback_(Status());
    } catch (const std::bad_alloc&) {
        publishIssue("memory_allocation_failed", "RC003 status notification allocation failed", ERROR_NOT_ENOUGH_MEMORY);
    } catch (...) {
        LogMessage(L"RC003 final status notification failed", LogLevel::Warning);
    }
    try { MarkFinished(state); }
    catch (...) { LogMessage(L"RC003 worker completion state update failed", LogLevel::Warning); }
}

} // namespace axonkey_service
