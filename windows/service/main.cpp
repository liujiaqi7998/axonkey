#include <windows.h>
#include <dbt.h>
#include <cfgmgr32.h>
#include <setupapi.h>
#include <hidsdi.h>
#include <QuarborDeviceSetup.h>
#include <QuarborHidFilter.h>
#include "ServiceConfig.h"
#include "VoiceReceiver.h"
#include "RpcServer.h"
#include "ServiceLog.h"
#include "GattAccess.h"

#include <winrt/base.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Devices.Enumeration.h>
#include <winrt/Windows.Devices.Bluetooth.h>
#include <winrt/Windows.Devices.Bluetooth.GenericAttributeProfile.h>
#include <winrt/Windows.Storage.Streams.h>

#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <chrono>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>
#include <functional>
#include <cstdint>
#include <optional>

namespace {
using axonkey_service::IsRc003;
using axonkey_service::LogMessage;
using axonkey_service::LogLevel;
void LogWin32Error(const wchar_t* operation, DWORD error) {
    LogMessage(std::wstring(operation) + L"; Win32=" + std::to_wstring(error), LogLevel::Error);
}
constexpr wchar_t kServiceName[] = L"AxonkeyService";
// GUID_DEVINTERFACE_KEYBOARD from ntddkbd.h. Defining it locally keeps the
// service buildable with the Windows SDK as well as the MinGW SDK.
constexpr GUID kKeyboardInterfaceGuid =
    {0x884b96c3, 0x56ef, 0x11d1, {0xbc, 0x8c, 0x00, 0xa0, 0xc9, 0x14, 0x05, 0xdd}};
struct EndpointInfo { std::wstring path; std::wstring instanceId; };

// RC003 exposes its model identity through the voice GATT service. Starting
// from that service also gives us a BluetoothLEDevice even when the HID node
// has no Bluetooth address in its instance ID.
constexpr wchar_t kVoiceServiceUuid[] = L"{AB5E0001-5A21-4F05-BC7D-AF01F617B664}";
constexpr wchar_t kBatteryServiceUuid[] = L"{0000180F-0000-1000-8000-00805F9B34FB}";
constexpr wchar_t kBatteryLevelUuid[] = L"{00002A19-0000-1000-8000-00805F9B34FB}";

struct BluetoothDeviceMetadata {
    std::optional<std::uint8_t> batteryLevel;
    std::wstring descriptionName;
};

winrt::guid BluetoothGuid(const wchar_t* value) { return winrt::guid{std::wstring_view(value)}; }

bool IsTargetBluetoothServiceId(const std::wstring& serviceId) {
    std::wstring value = serviceId;
    std::transform(value.begin(), value.end(), value.begin(), towlower);
    return value.find(L"vid&012717_pid&32b8") != std::wstring::npos ||
        value.find(L"vid_2717&pid_32b8") != std::wstring::npos;
}

class WinRtApartment final {
public:
    WinRtApartment() {
        try {
            winrt::init_apartment(winrt::apartment_type::multi_threaded);
            initialized_ = true;
        } catch (const winrt::hresult_error& error) {
            // The RPC worker normally has no apartment. If a caller already
            // initialized one, WinRT objects can still be used on that thread.
            if (error.code() != RPC_E_CHANGED_MODE) throw;
        }
    }
    ~WinRtApartment() {
        if (initialized_) {
            try { winrt::uninit_apartment(); } catch (...) {}
        }
    }
    WinRtApartment(const WinRtApartment&) = delete;
    WinRtApartment& operator=(const WinRtApartment&) = delete;
private:
    bool initialized_ = false;
};

std::wstring BluetoothDescriptionName(
    const winrt::Windows::Devices::Bluetooth::BluetoothLEDevice& device) {
    try {
        const auto name = std::wstring(device.Name());
        if (!name.empty()) return name;
    } catch (...) {}
    try {
        const auto information = device.DeviceInformation();
        if (information) return std::wstring(information.Name());
    } catch (...) {}
    return {};
}

std::optional<std::uint8_t> BluetoothBatteryLevel(
    const winrt::Windows::Devices::Bluetooth::BluetoothLEDevice& device) {
    namespace gatt = winrt::Windows::Devices::Bluetooth::GenericAttributeProfile;
    using winrt::Windows::Devices::Bluetooth::BluetoothCacheMode;
    using winrt::Windows::Storage::Streams::DataReader;
    try {
        const auto servicesResult = device.GetGattServicesForUuidAsync(
            BluetoothGuid(kBatteryServiceUuid), BluetoothCacheMode::Cached).get();
        if (!servicesResult || servicesResult.Status() != gatt::GattCommunicationStatus::Success)
            return {};
        const auto services = servicesResult.Services();
        if (!services) return {};
        for (std::uint32_t index = 0; index < services.Size(); ++index) {
            const auto service = services.GetAt(index);
            if (!service) continue;
            const auto characteristicResult = service.GetCharacteristicsForUuidAsync(
                BluetoothGuid(kBatteryLevelUuid), BluetoothCacheMode::Cached).get();
            if (!characteristicResult ||
                    characteristicResult.Status() != gatt::GattCommunicationStatus::Success)
                continue;
            const auto characteristics = characteristicResult.Characteristics();
            if (!characteristics || characteristics.Size() == 0) continue;
            const auto characteristic = characteristics.GetAt(0);
            if (!characteristic) continue;
            const auto read = characteristic.ReadValueAsync(BluetoothCacheMode::Cached).get();
            if (!read || read.Status() != gatt::GattCommunicationStatus::Success || !read.Value())
                continue;
            const auto reader = DataReader::FromBuffer(read.Value());
            if (!reader || reader.UnconsumedBufferLength() == 0) continue;
            const auto level = reader.ReadByte();
            if (level <= 100) return level;
        }
    } catch (...) {}
    return {};
}

BluetoothDeviceMetadata ReadBluetoothDeviceMetadata() {
    BluetoothDeviceMetadata result;
    try {
        WinRtApartment apartment;
        namespace gatt = winrt::Windows::Devices::Bluetooth::GenericAttributeProfile;
        using winrt::Windows::Devices::Enumeration::DeviceInformation;
        using winrt::Windows::Devices::Bluetooth::BluetoothLEDevice;
        const auto selector = gatt::GattDeviceService::GetDeviceSelectorFromUuid(
            BluetoothGuid(kVoiceServiceUuid));
        const auto services = DeviceInformation::FindAllAsync(selector).get();
        if (!services) return result;
        for (std::uint32_t index = 0; index < services.Size(); ++index) {
            const auto information = services.GetAt(index);
            if (!information || !IsTargetBluetoothServiceId(std::wstring(information.Id()))) continue;
            try {
                const auto service = gatt::GattDeviceService::FromIdAsync(information.Id()).get();
                const auto session = axonkey_service::RequireGattSession(service);
                const auto deviceId = session.DeviceId();
                if (!deviceId) continue;
                const auto device = BluetoothLEDevice::FromIdAsync(deviceId.Id()).get();
                if (!device) continue;
                result.descriptionName = BluetoothDescriptionName(device);
                result.batteryLevel = BluetoothBatteryLevel(device);
                return result;
            } catch (...) {
                // A stale service entry should not prevent trying the next one.
            }
        }
    } catch (...) {
        // Bluetooth discovery is best effort for GetDevices.
    }
    return result;
}

std::vector<EndpointInfo> EnumerateEndpoints() {
    std::vector<EndpointInfo> result;
    HDEVINFO set = SetupDiGetClassDevsW(&GUID_DEVINTERFACE_QUARBOR_HID, nullptr, nullptr,
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if (set == INVALID_HANDLE_VALUE) {
        LogWin32Error(L"HID endpoint enumeration failed", GetLastError());
        return result;
    }
    for (DWORD index = 0;; ++index) {
        SP_DEVICE_INTERFACE_DATA iface{}; iface.cbSize = sizeof(iface);
        if (!SetupDiEnumDeviceInterfaces(set, nullptr, &GUID_DEVINTERFACE_QUARBOR_HID, index, &iface)) {
            if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
            continue;
        }
        DWORD bytes = 0;
        SetupDiGetDeviceInterfaceDetailW(set, &iface, nullptr, 0, &bytes, nullptr);
        if (!bytes) continue;
        std::vector<BYTE> buffer(bytes + sizeof(wchar_t));
        auto* detail = reinterpret_cast<SP_DEVICE_INTERFACE_DETAIL_DATA_W*>(buffer.data());
        detail->cbSize = sizeof(*detail);
        SP_DEVINFO_DATA info{}; info.cbSize = sizeof(info);
        if (!SetupDiGetDeviceInterfaceDetailW(set, &iface, detail, bytes, nullptr, &info)) continue;
        wchar_t directId[MAX_DEVICE_ID_LEN]{};
        if (!SetupDiGetDeviceInstanceIdW(set, &info, directId, ARRAYSIZE(directId), nullptr)) continue;
        std::wstring selected = directId;
        DEVINST parent = 0;
        wchar_t parentId[MAX_DEVICE_ID_LEN]{};
        if (CM_Get_Parent(&parent, info.DevInst, 0) == CR_SUCCESS &&
                CM_Get_Device_IDW(parent, parentId, ARRAYSIZE(parentId), 0) == CR_SUCCESS &&
                IsRc003(parentId)) selected = parentId;
        result.push_back({detail->DevicePath, std::move(selected)});
    }
    SetupDiDestroyDeviceInfoList(set);
    return result;
}

class Endpoint final {
public:
    using ReportCallback = std::function<void(const std::wstring&, const std::vector<std::uint8_t>&)>;
    Endpoint(std::wstring path, std::wstring instance, ReportCallback reportCallback)
        : path_(std::move(path)), instance_(std::move(instance)), reportCallback_(std::move(reportCallback)) {
        handle_ = CreateFileW(path_.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr,
            OPEN_EXISTING, FILE_FLAG_OVERLAPPED, nullptr);
        if (handle_ == INVALID_HANDLE_VALUE) {
            LogWin32Error(L"Opening HID endpoint failed", GetLastError());
            return;
        }
        event_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        controlEvent_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        stop_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!event_ || !controlEvent_ || !stop_) return;
        QUARBOR_IDENTITY identity{};
        DWORD returned = 0;
        if (!Ioctl(IOCTL_QUARBOR_QUERY_IDENTITY, nullptr, 0, &identity, sizeof(identity), returned) ||
                returned != sizeof(identity) || identity.Size != sizeof(identity) ||
                identity.Version != QUARBOR_API_VERSION ||
                identity.MaxReportSize == 0 || identity.MaxReportSize > QUARBOR_MAX_REPORT_SIZE) return;
        report_.resize(identity.MaxReportSize);
        QUARBOR_SWITCH_CONTROL enabled{1};
        if (!identity.DataForwardEnabled &&
                !Ioctl(IOCTL_QUARBOR_SET_DATA_FORWARD, &enabled, sizeof(enabled), nullptr, 0, returned)) return;
        // Forward the untouched report to the service while suppressing Windows
        // input. Blocking takes priority over any remap table left in the driver.
        if (!identity.InputBlocked &&
                !Ioctl(IOCTL_QUARBOR_SET_INPUT_BLOCK, &enabled, sizeof(enabled), nullptr, 0, returned)) return;
        dataForwardEnabled_.store(true);
        inputBlocked_.store(true);
        valid_ = true;
        LogMessage(L"HID endpoint connected; device=" + instance_);
    }
    ~Endpoint() { Stop(); }
    bool Valid() const { return valid_.load(); }
    const std::wstring& InstanceId() const { return instance_; }
    const std::wstring& Path() const { return path_; }
    bool InputBlocked() const { return inputBlocked_.load(); }
    bool DataForwardEnabled() const { return dataForwardEnabled_.load(); }
    void Start() { reader_ = std::thread(&Endpoint::ReadLoop, this); }
    void Stop() {
        if (!handle_ || handle_ == INVALID_HANDLE_VALUE) return;
        if (stop_) SetEvent(stop_);
        // The reader owns its OVERLAPPED and drains cancellation before exiting.
        if (reader_.joinable()) reader_.join();
        QUARBOR_SWITCH_CONTROL disabled{0}; DWORD ignored = 0;
        if (controlEvent_) {
            Ioctl(IOCTL_QUARBOR_SET_INPUT_BLOCK, &disabled, sizeof(disabled), nullptr, 0, ignored);
            Ioctl(IOCTL_QUARBOR_SET_DATA_FORWARD, &disabled, sizeof(disabled), nullptr, 0, ignored);
        }
        if (event_) CloseHandle(event_);
        if (controlEvent_) CloseHandle(controlEvent_);
        if (stop_) CloseHandle(stop_);
        CloseHandle(handle_);
        event_ = controlEvent_ = stop_ = nullptr; handle_ = INVALID_HANDLE_VALUE;
    }
private:
    void CancelAndDrain(OVERLAPPED& pending) {
        // CancelIoEx only requests cancellation. The buffers and event must stay
        // alive until the driver completes the request, even on a timeout.
        CancelIoEx(handle_, &pending);
        DWORD ignored = 0;
        GetOverlappedResult(handle_, &pending, &ignored, TRUE);
    }
    bool Ioctl(DWORD code, void* input, DWORD inputSize, void* output, DWORD outputSize, DWORD& returned) {
        if (!handle_ || handle_ == INVALID_HANDLE_VALUE) return false;
        ResetEvent(controlEvent_); ZeroMemory(&controlOverlapped_, sizeof(controlOverlapped_)); controlOverlapped_.hEvent = controlEvent_;
        if (DeviceIoControl(handle_, code, input, inputSize, output, outputSize, &returned, &controlOverlapped_)) return true;
        if (GetLastError() != ERROR_IO_PENDING) return false;
        if (WaitForSingleObject(controlEvent_, 5000) != WAIT_OBJECT_0) {
            CancelAndDrain(controlOverlapped_);
            return false;
        }
        return GetOverlappedResult(handle_, &controlOverlapped_, &returned, FALSE) != FALSE;
    }
    void ReadLoop() {
        while (WaitForSingleObject(stop_, 0) != WAIT_OBJECT_0) {
            ResetEvent(event_); ZeroMemory(&overlapped_, sizeof(overlapped_)); overlapped_.hEvent = event_;
            DWORD bytes = 0;
            BOOL ok = DeviceIoControl(handle_, IOCTL_QUARBOR_HID_DATA, nullptr, 0,
                report_.data(), static_cast<DWORD>(report_.size()), &bytes, &overlapped_);
            if (ok) { if (bytes) OnHidReport(report_.data(), bytes); continue; }
            if (GetLastError() != ERROR_IO_PENDING) {
                LogWin32Error(L"HID read failed", GetLastError());
                break;
            }
            HANDLE waits[] = {event_, stop_};
            DWORD wait = WaitForMultipleObjects(2, waits, FALSE, INFINITE);
            if (wait != WAIT_OBJECT_0) {
                CancelAndDrain(overlapped_);
                break;
            }
            if (!GetOverlappedResult(handle_, &overlapped_, &bytes, FALSE)) {
                LogWin32Error(L"HID read completion failed", GetLastError());
                break;
            }
            if (bytes) OnHidReport(report_.data(), bytes);
        }
        // A failed reader must not keep suppressing input indefinitely. The
        // service will close and retry this endpoint during reconciliation.
        QUARBOR_SWITCH_CONTROL disabled{0}; DWORD ignored = 0;
        Ioctl(IOCTL_QUARBOR_SET_INPUT_BLOCK, &disabled, sizeof(disabled), nullptr, 0, ignored);
        valid_ = false;
        LogMessage(L"HID reader stopped; device=" + instance_);
    }
    void OnHidReport(const UCHAR* report, DWORD bytes) {
        if (reportCallback_ && bytes) reportCallback_(instance_, std::vector<std::uint8_t>(report, report + bytes));
    }
    std::wstring path_, instance_;
    HANDLE handle_ = INVALID_HANDLE_VALUE, event_ = nullptr, stop_ = nullptr;
    OVERLAPPED overlapped_{}, controlOverlapped_{};
    HANDLE controlEvent_ = nullptr;
    std::vector<UCHAR> report_;
    ReportCallback reportCallback_;
    std::thread reader_;
    std::atomic<bool> valid_{false};
    std::atomic<bool> inputBlocked_{false}, dataForwardEnabled_{false};
};

class Service final {
public:
    static Service& Instance() { static Service value; return value; }
    DWORD Run() {
        axonkey_service::LogMessage(L"AxonkeyService starting");
        statusHandle_ = RegisterServiceCtrlHandlerExW(kServiceName, Handler, this);
        if (!statusHandle_) {
            const auto error = GetLastError();
            LogWin32Error(L"Service control handler registration failed", error);
            return error;
        }
        UpdateStatus(SERVICE_START_PENDING, 3000);
        stopEvent_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!stopEvent_) {
            const auto error = GetLastError();
            LogWin32Error(L"Service stop event creation failed", error);
            UpdateStatus(SERVICE_STOPPED);
            return error;
        }
        config_ = axonkey_service::LoadServiceConfig();
        enabled_.store(config_.enabled, std::memory_order_release);
        rpc_ = std::make_unique<axonkey_service::RpcServer>(axonkey_service::RpcHandlers{
            [this] { return ServiceInfo(); },
            [this] { return ServiceStatus(); },
            [this](bool enabled) { return SetServiceEnabled(enabled); },
            [this](std::int32_t gain) { return SetAudioGain(gain); },
            [this] { return DeviceList(); },
            [this] { return VoiceStatus(); },
            [this] { return AudioLevel(); },
        });
        if (!rpc_->Start()) {
            LogMessage(L"Axonkey protobuf RPC endpoint could not start", LogLevel::Error);
            rpc_.reset();
        }
        DEV_BROADCAST_DEVICEINTERFACE_W filter{};
        filter.dbcc_size = sizeof(filter); filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
        filter.dbcc_classguid = kKeyboardInterfaceGuid;
        notification_ = RegisterDeviceNotificationW(statusHandle_, &filter, DEVICE_NOTIFY_SERVICE_HANDLE);
        if (!notification_) LogMessage(L"Device notification registration failed; using periodic scan; Win32=" +
            std::to_wstring(GetLastError()), LogLevel::Warning);
        worker_ = std::thread(&Service::Worker, this);
        UpdateStatus(SERVICE_RUNNING);
        axonkey_service::LogMessage(L"AxonkeyService running");
        WaitForSingleObject(stopEvent_, INFINITE);
        if (worker_.joinable()) worker_.join();
        if (rpc_) rpc_->Stop();
        if (notification_) UnregisterDeviceNotification(notification_);
        CloseHandle(stopEvent_); stopEvent_ = nullptr;
        UpdateStatus(SERVICE_STOPPED);
        axonkey_service::LogMessage(L"AxonkeyService stopped");
        return 0;
    }
    void RequestStop() { if (stopEvent_) SetEvent(stopEvent_); cv_.notify_all(); }
    void RequestRescan() { cv_.notify_all(); }
private:
    static DWORD WINAPI Handler(DWORD control, DWORD eventType, void* data, void* context) {
        (void)data;
        auto* self = static_cast<Service*>(context);
        if (control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN) {
            LogMessage(L"Service stop requested; control=" + std::to_wstring(control));
            self->UpdateStatus(SERVICE_STOP_PENDING, 5000); self->RequestStop(); return NO_ERROR;
        }
        if (control == SERVICE_CONTROL_DEVICEEVENT &&
                (eventType == DBT_DEVICEARRIVAL || eventType == DBT_DEVNODES_CHANGED || eventType == DBT_DEVICEREMOVECOMPLETE))
            self->RequestRescan();
        return NO_ERROR;
    }
    void UpdateStatus(DWORD state, DWORD waitHint = 0) {
        status_.dwServiceType = SERVICE_WIN32_OWN_PROCESS; status_.dwCurrentState = state;
        status_.dwControlsAccepted = (state == SERVICE_RUNNING) ?
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN : 0;
        status_.dwWaitHint = waitHint; status_.dwWin32ExitCode = NO_ERROR;
        if (statusHandle_ && !::SetServiceStatus(statusHandle_, &status_))
            LogWin32Error(L"Updating service status failed", GetLastError());
    }
    void Worker() {
        axonkey_service::LogMessage(L"Audio gain loaded: " + std::to_wstring(config_.audioGainDb) +
            L" dB; service enabled=" + std::to_wstring(enabled_.load() ? 1 : 0));
        while (WaitForSingleObject(stopEvent_, 0) != WAIT_OBJECT_0) {
            try { Reconcile(); }
            catch (const std::exception& error) {
                LogMessage(std::string("Device reconciliation failed; retrying: ") + error.what(), LogLevel::Error);
            }
            catch (...) { LogMessage(L"Device reconciliation failed with an unknown exception; retrying", LogLevel::Error); }
            std::unique_lock lock(mutex_);
            cv_.wait_for(lock, std::chrono::seconds(2));
        }
        std::lock_guard operationLock(operationMutex_);
        StopEndpoints();
        DetachDevices();
    }
    void Reconcile() {
        std::lock_guard operationLock(operationMutex_);
        ReconcileLocked();
    }
    void ReconcileLocked() {
        // This is intentionally the first device-initialization decision. A
        // disabled service must not enumerate, attach filters, or start voice
        // and HID workers.
        if (!enabled_.load(std::memory_order_acquire)) return;
        std::vector<std::wstring> targets;
        try {
            for (const auto& device : quarbor::EnumerateHidKeyboards(true)) if (IsRc003(device.instanceId)) {
                targets.push_back(device.instanceId);
                if (!device.attachmentConfigured) {
                    quarbor::SetDeviceAttachment(device.instanceId, true, true);
                    LogMessage(L"HID filter attached; device=" + device.instanceId);
                }
            }
        } catch (const std::exception& error) {
            LogMessage(std::string("Device discovery/filter attachment failed; retrying: ") + error.what(), LogLevel::Error);
            return;
        } catch (...) {
            LogMessage(L"Device discovery/filter attachment failed; retrying", LogLevel::Error);
            return;
        }
        auto endpoints = EnumerateEndpoints();
        std::lock_guard lock(mutex_);
        targets_ = targets;
        for (auto it = active_.begin(); it != active_.end();) {
            if (!(*it)->Valid() || std::find(targets.begin(), targets.end(), (*it)->InstanceId()) == targets.end()) { it = active_.erase(it); }
            else ++it;
        }
        for (auto it = voices_.begin(); it != voices_.end();) {
            if (it->second->Finished() ||
                    std::find(targets.begin(), targets.end(), it->first) == targets.end()) it = voices_.erase(it);
            else ++it;
        }
        for (const auto& target : targets) {
            if (voices_.find(target) == voices_.end()) {
                auto receiver = std::make_shared<axonkey_service::VoiceReceiver>(target, config_.audioGainDb,
                    [this](float peak, float rms) {
                        axonkey::rpc::AudioLevel level;
                        level.peak = peak;
                        level.rms = rms;
                        level.timestampMs = static_cast<std::uint64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(
                            std::chrono::system_clock::now().time_since_epoch()).count());
                        {
                            std::lock_guard levelLock(levelMutex_);
                            latestAudioLevel_ = level;
                        }
                        if (rpc_) rpc_->PublishAudioLevel(level);
                    },
                    [this](const axonkey::rpc::VoiceStatus& status) {
                        if (rpc_) rpc_->PublishVoiceStatus(status);
                    });
                receiver->Start();
                voices_.emplace(target, std::move(receiver));
            }
        }
        for (const auto& endpoint : endpoints) {
            if (!IsRc003(endpoint.instanceId) || std::find(targets.begin(), targets.end(), endpoint.instanceId) == targets.end()) continue;
            auto existing = std::find_if(active_.begin(), active_.end(), [&](const auto& e) { return e->Path() == endpoint.path; });
            if (existing != active_.end()) continue;
            auto connection = std::make_shared<Endpoint>(endpoint.path, endpoint.instanceId,
                [this](const std::wstring& device, const std::vector<std::uint8_t>& report) {
                    if (rpc_) rpc_->PublishKeyboard(WideToUtf8(device), report);
                });
            if (connection->Valid()) { connection->Start(); active_.push_back(std::move(connection)); }
            else LogMessage(L"HID endpoint initialization failed; retrying; device=" + endpoint.instanceId, LogLevel::Warning);
        }
    }
    void StopEndpoints() {
        std::lock_guard lock(mutex_);
        active_.clear();
        voices_.clear();
        targets_.clear();
    }
    void DetachDevices() {
        // All endpoint and voice handles are closed before changing
        // LowerFilters on the devices.
        try {
            for (const auto& device : quarbor::EnumerateHidKeyboards(false)) {
                if (!device.attachmentConfigured) continue;
                try { quarbor::SetDeviceAttachment(device.instanceId, false, true); }
                catch (...) { LogMessage(L"Device filter detach failed; device=" + device.instanceId, LogLevel::Warning); }
            }
        } catch (...) { LogMessage(L"Device enumeration failed while detaching devices", LogLevel::Warning); }
    }
    SERVICE_STATUS_HANDLE statusHandle_ = nullptr; SERVICE_STATUS status_{};
    HANDLE stopEvent_ = nullptr, notification_ = nullptr;
    std::thread worker_; std::mutex mutex_; std::condition_variable cv_;
    std::mutex operationMutex_;
    std::atomic_bool enabled_{true};
    std::vector<std::shared_ptr<Endpoint>> active_;
    std::unordered_map<std::wstring, std::shared_ptr<axonkey_service::VoiceReceiver>> voices_;
    axonkey_service::ServiceConfig config_;
    std::unique_ptr<axonkey_service::RpcServer> rpc_;

    static std::string WideToUtf8(const std::wstring& value) {
        if (value.empty()) return {};
        const int bytes = WideCharToMultiByte(CP_UTF8, 0, value.data(), static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
        std::string result(static_cast<size_t>(bytes), '\0');
        WideCharToMultiByte(CP_UTF8, 0, value.data(), static_cast<int>(value.size()), result.data(), bytes, nullptr, nullptr);
        return result;
    }
    axonkey::rpc::ServiceInfo ServiceInfo() const {
        return {"AxonkeyService", "0.3.1", "axonkey.service.v1", "\\\\.\\pipe\\AxonkeyService.v1"};
    }
    axonkey::rpc::ServiceStatus ServiceStatus() const {
        return {enabled_.load(std::memory_order_acquire)};
    }
    axonkey::rpc::OperationResult SetServiceEnabled(bool enabled) {
        std::lock_guard operationLock(operationMutex_);
        enabled_.store(enabled, std::memory_order_release);
        if (enabled) {
            ReconcileLocked();
        } else {
            StopEndpoints();
            DetachDevices();
        }
        if (!axonkey_service::SaveServiceEnabled(enabled))
            return {false, "Enabled could not be saved"};
        cv_.notify_all();
        LogMessage(std::wstring(L"Service device processing ") + (enabled ? L"enabled" : L"disabled"));
        return {true, {}};
    }
    axonkey::rpc::OperationResult SetAudioGain(std::int32_t gain) {
        gain = std::clamp(gain, axonkey_service::kMinAudioGainDb, axonkey_service::kMaxAudioGainDb);
        std::lock_guard lock(mutex_);
        config_.audioGainDb = gain;
        for (const auto& voice : voices_) voice.second->SetAudioGain(gain);
        if (!axonkey_service::SaveAudioGain(gain)) return {false, "AudioGainDb could not be saved"};
        return {true, {}};
    }
    axonkey::rpc::DeviceList DeviceList() {
        axonkey::rpc::DeviceList result;
        bool hasTargets = false;
        {
            std::lock_guard lock(mutex_);
            hasTargets = !targets_.empty();
        }
        const auto bluetooth = hasTargets ? ReadBluetoothDeviceMetadata() : BluetoothDeviceMetadata{};
        std::lock_guard lock(mutex_);
        for (const auto& target : targets_) {
            axonkey::rpc::Device device;
            device.instanceId = WideToUtf8(target);
            device.driverMounted = true;
            device.batteryLevel = bluetooth.batteryLevel;
            device.descriptionName = WideToUtf8(bluetooth.descriptionName);
            for (const auto& endpoint : active_) if (endpoint->InstanceId() == target) {
                device.endpointPath = WideToUtf8(endpoint->Path());
                device.connected = endpoint->Valid();
                device.inputBlocked = endpoint->InputBlocked();
                device.dataForwardEnabled = endpoint->DataForwardEnabled();
            }
            result.devices.push_back(std::move(device));
        }
        return result;
    }
    axonkey::rpc::VoiceStatus VoiceStatus() {
        axonkey::rpc::VoiceStatus result;
        std::lock_guard lock(mutex_);
        for (const auto& item : voices_) {
            result = item.second->Status();
            if (result.connected || result.active) break;
        }
        return result;
    }
    axonkey::rpc::AudioLevel AudioLevel() const {
        std::lock_guard lock(levelMutex_);
        return latestAudioLevel_;
    }
    std::vector<std::wstring> targets_;
    mutable std::mutex levelMutex_;
    axonkey::rpc::AudioLevel latestAudioLevel_;
};

void WINAPI ServiceMain(DWORD, LPWSTR*) { Service::Instance().Run(); }
}

int wmain() {
    SERVICE_TABLE_ENTRYW table[] = {{const_cast<LPWSTR>(kServiceName), ServiceMain}, {nullptr, nullptr}};
    if (!StartServiceCtrlDispatcherW(table)) {
        const auto error = GetLastError();
        LogWin32Error(L"Service dispatcher startup failed", error);
        return static_cast<int>(error);
    }
    return 0;
}
