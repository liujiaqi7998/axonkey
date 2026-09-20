#include <QuarborDeviceSetup.h>
#include <QuarborHidSelection.h>
#include "QuarborDeviceSetup.Internal.h"
#include "QuarborDriverPackage.Internal.h"
#include <setupapi.h>
#include <cfgmgr32.h>
#include <system_error>

namespace quarbor {
namespace {
constexpr GUID KeyboardClass = {0x4d36e96b, 0xe325, 0x11ce, {0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18}};
[[noreturn]] void Fail(const char* operation, DWORD error = GetLastError()) {
    throw std::system_error(static_cast<int>(error), std::system_category(), operation);
}
struct DeviceSet {
    HDEVINFO value;
    explicit DeviceSet(DWORD flags) : value(SetupDiGetClassDevsW(&KeyboardClass, nullptr, nullptr, flags)) {
        if (value == INVALID_HANDLE_VALUE) Fail("Enumerate Keyboard devices");
    }
    ~DeviceSet() { SetupDiDestroyDeviceInfoList(value); }
};
std::vector<wchar_t> Property(HDEVINFO set, SP_DEVINFO_DATA& info, DWORD property, DWORD expectedType) {
    DWORD required = 0, type = 0;
    if (!SetupDiGetDeviceRegistryPropertyW(set, &info, property, &type, nullptr, 0, &required)) {
        const DWORD error = GetLastError();
        if (error == ERROR_INVALID_DATA) return {}; // Property not set.
        if (error != ERROR_INSUFFICIENT_BUFFER) Fail("Read device property", error);
    }
    for (int attempt = 0; attempt < 3; ++attempt) {
        if (required > 1024 * 1024 || required % sizeof(wchar_t) != 0) Fail("Invalid device property size", ERROR_INVALID_DATA);
        std::vector<wchar_t> buffer(required / sizeof(wchar_t) + 2, L'\0');
        if (SetupDiGetDeviceRegistryPropertyW(set, &info, property, &type,
                reinterpret_cast<BYTE*>(buffer.data()), required, &required)) {
            if (type != expectedType) Fail("Invalid device property type", ERROR_INVALID_DATA);
            return buffer;
        }
        const DWORD error = GetLastError();
        if (error == ERROR_INVALID_DATA) return {};
        if (error != ERROR_INSUFFICIENT_BUFFER) Fail("Read device property", error);
    }
    Fail("Device property kept changing", ERROR_RETRY);
}
std::wstring TextProperty(HDEVINFO set, SP_DEVINFO_DATA& info, DWORD property) {
    const auto value = Property(set, info, property, REG_SZ);
    return value.empty() ? L"" : value.data();
}
std::vector<std::wstring> FilterList(HDEVINFO set, SP_DEVINFO_DATA& info) {
    const auto value = Property(set, info, SPDRP_LOWERFILTERS, REG_MULTI_SZ);
    std::vector<std::wstring> result;
    for (size_t offset = 0; offset < value.size() && value[offset];) {
        result.emplace_back(value.data() + offset);
        offset += result.back().size() + 1;
    }
    return result;
}
bool Eligible(HDEVINFO set, SP_DEVINFO_DATA& info) {
    return IsEqualGUID(info.ClassGuid, KeyboardClass) &&
        QuarborEqual(TextProperty(set, info, SPDRP_ENUMERATOR_NAME).c_str(), L"HID");
}
bool Present(const std::wstring& instanceId) {
    DEVINST node = 0;
    const CONFIGRET result = CM_Locate_DevNodeW(&node, const_cast<wchar_t*>(instanceId.c_str()), CM_LOCATE_DEVNODE_NORMAL);
    if (result == CR_NO_SUCH_DEVNODE) return false;
    if (result != CR_SUCCESS) Fail("Locate device", CM_MapCrToWin32Err(result, ERROR_GEN_FAILURE));
    return true;
}
void ValidateService() {
    struct ServiceHandle {
        SC_HANDLE value;
        ~ServiceHandle() { if (value) CloseServiceHandle(value); }
    } manager{OpenSCManagerW(nullptr, nullptr, SC_MANAGER_CONNECT)};
    if (!manager.value) Fail("Open service manager");
    ServiceHandle service{OpenServiceW(manager.value, FilterServiceName, SERVICE_QUERY_CONFIG)};
    if (!service.value) Fail("Install Quarbor driver before attaching a device");
    DWORD required = 0;
    if (QueryServiceConfigW(service.value, nullptr, 0, &required)) Fail("Missing driver service configuration", ERROR_INVALID_DATA);
    if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) Fail("Query driver service");
    std::vector<ULONG_PTR> storage((required + sizeof(ULONG_PTR) - 1) / sizeof(ULONG_PTR));
    auto* config = reinterpret_cast<QUERY_SERVICE_CONFIGW*>(storage.data());
    if (!QueryServiceConfigW(service.value, config, required, &required)) Fail("Query driver service");
    if (config->dwServiceType != SERVICE_KERNEL_DRIVER || config->dwStartType == SERVICE_DISABLED)
        Fail("Quarbor service is not an enabled kernel driver", ERROR_SERVICE_DISABLED);
}
void RejectClassFilter() {
    // A class-wide registration would keep attaching unselected devices.
    for (const DWORD property : {SPCRP_LOWERFILTERS, SPCRP_UPPERFILTERS}) {
        DWORD size = 0, type = 0;
        if (!SetupDiGetClassRegistryPropertyW(&KeyboardClass, property, &type, nullptr, 0, &size, nullptr, nullptr)) {
            const DWORD error = GetLastError();
            if (error == ERROR_INVALID_DATA || error == ERROR_FILE_NOT_FOUND) continue;
            if (error != ERROR_INSUFFICIENT_BUFFER) Fail("Read Keyboard class filters", error);
        }
        if (size > 1024 * 1024 || size % sizeof(wchar_t)) Fail("Invalid class filters", ERROR_INVALID_DATA);
        std::vector<wchar_t> filters(size / sizeof(wchar_t) + 2, L'\0');
        if (!SetupDiGetClassRegistryPropertyW(&KeyboardClass, property, &type,
                reinterpret_cast<BYTE*>(filters.data()), size, nullptr, nullptr, nullptr)) Fail("Read Keyboard class filters");
        if (type != REG_MULTI_SZ) Fail("Invalid class filters", ERROR_INVALID_DATA);
        for (size_t offset = 0; offset < filters.size() && filters[offset];) {
            const std::wstring filter(filters.data() + offset);
            if (detail::IsOurFilter(filter)) Fail("Use Install Driver in the test application to remove the class filter registration, then reboot", ERROR_INVALID_STATE);
            offset += filter.size() + 1;
        }
    }
}
AttachmentResult RestartDevice(HDEVINFO set, SP_DEVINFO_DATA& info) {
    AttachmentResult result;
    result.restartAttempted = true;
    SP_PROPCHANGE_PARAMS change{};
    change.ClassInstallHeader.cbSize = sizeof(SP_CLASSINSTALL_HEADER);
    change.ClassInstallHeader.InstallFunction = DIF_PROPERTYCHANGE;
    change.StateChange = DICS_PROPCHANGE;
    change.Scope = DICS_FLAG_CONFIGSPECIFIC;
    if (!SetupDiSetClassInstallParamsW(set, &info, &change.ClassInstallHeader, sizeof(change)) ||
            !SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set, &info)) {
        result.restartError = GetLastError();
        return result;
    }
    SP_DEVINSTALL_PARAMS_W params{};
    params.cbSize = sizeof(params);
    if (!SetupDiGetDeviceInstallParamsW(set, &info, &params)) result.restartError = GetLastError();
    else result.restartRequired = (params.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART)) != 0;
    return result;
}

class WindowsAttachment final : public detail::AttachmentBackend {
public:
    explicit WindowsAttachment(const std::wstring& instanceId) : set_(0), instanceId_(instanceId) {
        if (instanceId.empty() || instanceId.size() >= MAX_DEVICE_ID_LEN || instanceId.find(L'\0') != std::wstring::npos)
            Fail("Invalid device instance ID", ERROR_INVALID_PARAMETER);
        info_.cbSize = sizeof(info_);
        if (!SetupDiOpenDeviceInfoW(set_.value, instanceId.c_str(), nullptr, 0, &info_)) Fail("Open selected device instance");
    }
    void Validate(bool enabled) override {
        if (!Eligible(set_.value, info_)) Fail("Only HID Keyboard collections can be attached", ERROR_NOT_SUPPORTED);
        RejectClassFilter();
        if (enabled) ValidateService();
    }
    std::vector<std::wstring> ReadFilters() override { return FilterList(set_.value, info_); }
    void WriteFilters(const std::vector<std::wstring>& filters) override {
        std::vector<wchar_t> data;
        for (const auto& filter : filters) {
            data.insert(data.end(), filter.begin(), filter.end());
            data.push_back(L'\0');
        }
        if (!data.empty()) data.push_back(L'\0');
        if (!SetupDiSetDeviceRegistryPropertyW(set_.value, &info_, SPDRP_LOWERFILTERS,
                data.empty() ? nullptr : reinterpret_cast<const BYTE*>(data.data()),
                static_cast<DWORD>(data.size() * sizeof(wchar_t)))) Fail("Save selected device attachment (administrator required)");
    }
    bool IsPresent() override { return Present(instanceId_); }
    AttachmentResult Restart() override {
        return RestartDevice(set_.value, info_);
    }
private:
    DeviceSet set_;
    SP_DEVINFO_DATA info_{};
    std::wstring instanceId_;
};
} // namespace

std::vector<KeyboardDevice> EnumerateHidKeyboards(bool presentOnly) {
    DeviceSet set(presentOnly ? DIGCF_PRESENT : 0);
    std::vector<KeyboardDevice> devices;
    for (DWORD index = 0;; ++index) {
        SP_DEVINFO_DATA info{};
        info.cbSize = sizeof(info);
        if (!SetupDiEnumDeviceInfo(set.value, index, &info)) {
            if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
            Fail("Enumerate Keyboard device");
        }
        if (!Eligible(set.value, info)) continue;
        wchar_t id[MAX_DEVICE_ID_LEN]{};
        if (!SetupDiGetDeviceInstanceIdW(set.value, &info, id, MAX_DEVICE_ID_LEN, nullptr)) Fail("Read device instance ID");
        KeyboardDevice device;
        device.instanceId = id;
        device.present = presentOnly || Present(device.instanceId);
        device.name = TextProperty(set.value, info, SPDRP_FRIENDLYNAME);
        if (device.name.empty()) device.name = TextProperty(set.value, info, SPDRP_DEVICEDESC);
        if (device.name.empty()) device.name = L"HID Keyboard collection";
        const auto filters = FilterList(set.value, info);
        device.attachmentConfigured = std::any_of(filters.begin(), filters.end(), detail::IsOurFilter);
        if (device.present || device.attachmentConfigured) devices.push_back(std::move(device));
    }
    return devices;
}

AttachmentResult SetDeviceAttachment(const std::wstring& instanceId, bool enabled, bool restartNow) {
    WindowsAttachment target(instanceId);
    return detail::ConfigureAttachment(target, enabled, restartNow);
}

namespace detail {
void CheckInstalledFilterService() { ValidateService(); }

static bool RemoveOurFilter(std::vector<std::wstring>& filters) {
    const auto end = std::remove_if(filters.begin(), filters.end(), IsOurFilter);
    const bool changed = end != filters.end();
    filters.erase(end, filters.end());
    return changed;
}

static std::vector<wchar_t> MultiString(const std::vector<std::wstring>& filters) {
    std::vector<wchar_t> data;
    for (const auto& filter : filters) {
        data.insert(data.end(), filter.begin(), filter.end());
        data.push_back(L'\0');
    }
    if (!data.empty()) data.push_back(L'\0');
    return data;
}

static bool ClearClassFilter(const wchar_t* classId) {
    const std::wstring path = std::wstring(L"SYSTEM\\CurrentControlSet\\Control\\Class\\") + classId;
    struct Key { HKEY value = nullptr; ~Key() { if (value) RegCloseKey(value); } } key;
    LSTATUS error = RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.c_str(), 0, KEY_QUERY_VALUE | KEY_SET_VALUE, &key.value);
    if (error == ERROR_FILE_NOT_FOUND) return false;
    if (error != ERROR_SUCCESS) Fail("Open class filter registration (administrator required)", error);
    bool changed = false;
    for (const auto* name : {L"LowerFilters", L"UpperFilters"}) {
        DWORD type = 0, size = 0;
        error = RegQueryValueExW(key.value, name, nullptr, &type, nullptr, &size);
        if (error == ERROR_FILE_NOT_FOUND) continue;
        if (error != ERROR_SUCCESS) Fail("Read class filter registration", error);
        if (type != REG_MULTI_SZ || size % sizeof(wchar_t) || size > 1024 * 1024) Fail("Invalid class filter registration", ERROR_INVALID_DATA);
        std::vector<wchar_t> buffer(size / sizeof(wchar_t) + 2, L'\0');
        error = RegQueryValueExW(key.value, name, nullptr, &type, reinterpret_cast<BYTE*>(buffer.data()), &size);
        if (error != ERROR_SUCCESS) Fail("Read class filter registration", error);
        if (type != REG_MULTI_SZ) Fail("Class filter registration changed concurrently", ERROR_RETRY);
        std::vector<std::wstring> filters;
        for (size_t offset = 0; offset < buffer.size() && buffer[offset];) {
            filters.emplace_back(buffer.data() + offset);
            offset += filters.back().size() + 1;
        }
        if (!RemoveOurFilter(filters)) continue;
        const auto data = MultiString(filters);
        error = data.empty() ? RegDeleteValueW(key.value, name) :
            RegSetValueExW(key.value, name, 0, REG_MULTI_SZ, reinterpret_cast<const BYTE*>(data.data()),
                static_cast<DWORD>(data.size() * sizeof(wchar_t)));
        if (error != ERROR_SUCCESS) Fail("Remove class filter registration", error);
        DWORD verifiedSize = static_cast<DWORD>((data.size() + 2) * sizeof(wchar_t));
        std::vector<wchar_t> verified(data.size() + 2, L'\0');
        error = RegQueryValueExW(key.value, name, nullptr, &type, reinterpret_cast<BYTE*>(verified.data()), &verifiedSize);
        if (data.empty() ? error != ERROR_FILE_NOT_FOUND :
                (error != ERROR_SUCCESS || type != REG_MULTI_SZ || verifiedSize != data.size() * sizeof(wchar_t) ||
                    !std::equal(data.begin(), data.end(), verified.begin())))
            Fail("Class filters changed during uninstall; stop and retry", ERROR_RETRY);
        changed = true;
    }
    return changed;
}

bool ClearFilterRegistrations(bool includeDevices) {
    bool reboot = ClearClassFilter(L"{4D36E96B-E325-11CE-BFC1-08002BE10318}");
    reboot = ClearClassFilter(L"{745A17A0-74D3-11D0-B6FE-00A0C90F57DA}") || reboot;
    if (!includeDevices) return reboot;
    // Include offline instances and registrations on HIDClass nodes.
    const HDEVINFO set = SetupDiGetClassDevsW(nullptr, nullptr, nullptr, DIGCF_ALLCLASSES);
    if (set == INVALID_HANDLE_VALUE) Fail("Enumerate filter registrations before uninstall");
    struct Cleanup { HDEVINFO value; ~Cleanup() { SetupDiDestroyDeviceInfoList(value); } } cleanup{set};
    std::vector<SP_DEVINFO_DATA> changedDevices;
    for (DWORD index = 0;; ++index) {
        SP_DEVINFO_DATA info{};
        info.cbSize = sizeof(info);
        if (!SetupDiEnumDeviceInfo(set, index, &info)) {
            if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
            Fail("Enumerate registered devices before uninstall");
        }
        bool changed = false;
        for (const DWORD property : {SPDRP_LOWERFILTERS, SPDRP_UPPERFILTERS}) {
            const auto buffer = Property(set, info, property, REG_MULTI_SZ);
            std::vector<std::wstring> filters;
            for (size_t offset = 0; offset < buffer.size() && buffer[offset];) {
                filters.emplace_back(buffer.data() + offset);
                offset += filters.back().size() + 1;
            }
            if (!RemoveOurFilter(filters)) continue;
            const auto data = MultiString(filters);
            if (!SetupDiSetDeviceRegistryPropertyW(set, &info, property,
                    data.empty() ? nullptr : reinterpret_cast<const BYTE*>(data.data()),
                    static_cast<DWORD>(data.size() * sizeof(wchar_t))))
                Fail("Cannot clear all Quarbor device registrations; package removal was not attempted");
            const auto verify = Property(set, info, property, REG_MULTI_SZ);
            std::vector<std::wstring> remaining;
            for (size_t offset = 0; offset < verify.size() && verify[offset];) {
                remaining.emplace_back(verify.data() + offset);
                offset += remaining.back().size() + 1;
            }
            if (remaining != filters) Fail("Device filters changed during uninstall; package removal was not attempted", ERROR_RETRY);
            changed = true;
        }
        if (changed) changedDevices.push_back(info);
    }
    // All references have been cleared before any device restarts. Restart only
    // affected online instances. A veto leaves the selection removed and falls
    // back to a reboot; never force-disable or close another process's handles.
    for (auto& info : changedDevices) {
        wchar_t id[MAX_DEVICE_ID_LEN]{};
        if (!SetupDiGetDeviceInstanceIdW(set, &info, id, ARRAYSIZE(id), nullptr)) { reboot = true; continue; }
        try {
            if (Present(id)) reboot = RestartDevice(set, info).restartRequired || reboot;
        } catch (const std::system_error&) { reboot = true; }
    }
    return reboot;
}
} // namespace detail
} // namespace quarbor
