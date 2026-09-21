#include "ServiceConfig.h"
#include "ServiceLog.h"
#include <QuarborIoPolicy.h>
#include <algorithm>
#include <cwctype>

namespace axonkey_service {
namespace {
constexpr wchar_t kRegistryPath[] = L"SOFTWARE\\Axonkey\\Service";
constexpr wchar_t kRemapValue[] = L"RemapConfig";
constexpr wchar_t kAudioGainValue[] = L"AudioGainDb";
constexpr wchar_t kEnabledValue[] = L"Enabled";

bool ContainsInsensitive(std::wstring value, const wchar_t* needle) {
    std::transform(value.begin(), value.end(), value.begin(), towupper);
    std::wstring wanted(needle);
    std::transform(wanted.begin(), wanted.end(), wanted.begin(), towupper);
    return value.find(wanted) != std::wstring::npos;
}

class RegKey final {
public:
    HKEY value = nullptr;
    ~RegKey() { if (value) RegCloseKey(value); }
};
}

bool IsRc003(const std::wstring& id) {
    return (ContainsInsensitive(id, L"VID_2717") || ContainsInsensitive(id, L"VID&012717")) &&
        (ContainsInsensitive(id, L"PID_32B8") || ContainsInsensitive(id, L"PID&32B8"));
}

ServiceConfig LoadServiceConfig() {
    RegKey key;
    const auto error = RegCreateKeyExW(HKEY_LOCAL_MACHINE, kRegistryPath, 0, nullptr, 0,
        KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY, nullptr, &key.value, nullptr);
    if (error != ERROR_SUCCESS) {
        LogMessage(L"Service configuration registry open failed; using defaults; Win32=" + std::to_wstring(error), LogLevel::Warning);
    }
    return LoadServiceConfigFromKey(key.value);
}

ServiceConfig LoadServiceConfigFromKey(HKEY key) {
    ServiceConfig result;
    result.remap.Size = sizeof(result.remap);
    result.remap.Version = QUARBOR_API_VERSION;
    if (!key) return result;
    DWORD size = sizeof(result.remap), type = 0;
    const bool remapNeedsDefault = RegQueryValueExW(key, kRemapValue, nullptr, &type,
            reinterpret_cast<BYTE*>(&result.remap), &size) != ERROR_SUCCESS ||
            type != REG_BINARY || size != sizeof(result.remap) ||
            !QuarborValidRemapConfig(&result.remap);
    if (remapNeedsDefault) {
        result.remap = {};
        result.remap.Size = sizeof(result.remap);
        result.remap.Version = QUARBOR_API_VERSION;
    }
    // Materialize the default table so administrators can edit it with reg.exe.
    if (remapNeedsDefault) RegSetValueExW(key, kRemapValue, 0, REG_BINARY,
        reinterpret_cast<const BYTE*>(&result.remap), sizeof(result.remap));
    // REG_DWORD retains its existing 32-bit storage; interpret the bits as a
    // signed integer so negative dB values also work (e.g. -6 = 0xFFFFFFFA).
    static_assert(sizeof(result.audioGainDb) == sizeof(DWORD));
    size = sizeof(result.audioGainDb);
    const auto gainError = RegQueryValueExW(key, kAudioGainValue, nullptr, &type,
        reinterpret_cast<BYTE*>(&result.audioGainDb), &size);
    if (gainError != ERROR_SUCCESS || type != REG_DWORD || size != sizeof(DWORD) ||
        result.audioGainDb < kMinAudioGainDb || result.audioGainDb > kMaxAudioGainDb) {
        result.audioGainDb = kDefaultAudioGainDb;
        if (gainError != ERROR_FILE_NOT_FOUND)
            LogMessage(L"AudioGainDb missing/unreadable or invalid; restoring 2 dB default", LogLevel::Warning);
        const auto writeError = RegSetValueExW(key, kAudioGainValue, 0, REG_DWORD,
            reinterpret_cast<const BYTE*>(&result.audioGainDb), sizeof(result.audioGainDb));
        if (writeError != ERROR_SUCCESS)
            LogMessage(L"AudioGainDb default could not be saved; Win32=" + std::to_wstring(writeError), LogLevel::Warning);
    }
    DWORD enabled = 1;
    size = sizeof(enabled);
    const auto enabledError = RegQueryValueExW(key, kEnabledValue, nullptr, &type,
        reinterpret_cast<BYTE*>(&enabled), &size);
    if (enabledError == ERROR_SUCCESS && type == REG_DWORD && size == sizeof(enabled) &&
        (enabled == 0 || enabled == 1)) {
        result.enabled = enabled != 0;
    } else {
        result.enabled = true;
        if (enabledError != ERROR_FILE_NOT_FOUND)
            LogMessage(L"Enabled missing/unreadable or invalid; restoring enabled default", LogLevel::Warning);
        enabled = 1;
        const auto writeError = RegSetValueExW(key, kEnabledValue, 0, REG_DWORD,
            reinterpret_cast<const BYTE*>(&enabled), sizeof(enabled));
        if (writeError != ERROR_SUCCESS)
            LogMessage(L"Enabled default could not be saved; Win32=" + std::to_wstring(writeError), LogLevel::Warning);
    }
    return result;
}

bool SaveAudioGain(std::int32_t gainDb) {
    gainDb = std::clamp(gainDb, kMinAudioGainDb, kMaxAudioGainDb);
    HKEY key = nullptr;
    const auto error = RegCreateKeyExW(HKEY_LOCAL_MACHINE, kRegistryPath, 0, nullptr, 0,
        KEY_SET_VALUE | KEY_WOW64_64KEY, nullptr, &key, nullptr);
    if (error != ERROR_SUCCESS) return false;
    const auto result = RegSetValueExW(key, kAudioGainValue, 0, REG_DWORD,
        reinterpret_cast<const BYTE*>(&gainDb), sizeof(gainDb));
    RegCloseKey(key);
    if (result != ERROR_SUCCESS) {
        LogMessage(L"AudioGainDb RPC update failed; Win32=" + std::to_wstring(result), LogLevel::Warning);
        return false;
    }
    return true;
}

bool SaveServiceEnabled(bool enabled) {
    HKEY key = nullptr;
    const auto error = RegCreateKeyExW(HKEY_LOCAL_MACHINE, kRegistryPath, 0, nullptr, 0,
        KEY_SET_VALUE | KEY_WOW64_64KEY, nullptr, &key, nullptr);
    if (error != ERROR_SUCCESS) return false;
    const DWORD value = enabled ? 1u : 0u;
    const auto result = RegSetValueExW(key, kEnabledValue, 0, REG_DWORD,
        reinterpret_cast<const BYTE*>(&value), sizeof(value));
    RegCloseKey(key);
    if (result != ERROR_SUCCESS) {
        LogMessage(L"Enabled RPC update failed; Win32=" + std::to_wstring(result), LogLevel::Warning);
        return false;
    }
    return true;
}
} // namespace axonkey_service
