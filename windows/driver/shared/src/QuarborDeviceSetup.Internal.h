#pragma once
#include <QuarborDeviceSetup.h>
#include <algorithm>
#include <system_error>

namespace quarbor::detail {
inline bool IsOurFilter(const std::wstring& value) {
    return CompareStringOrdinal(value.c_str(), -1, FilterServiceName, -1, TRUE) == CSTR_EQUAL;
}

// Isolated from SetupAPI so failure and persistence behavior can be verified
// without changing any real keyboard's configuration.
struct AttachmentBackend {
    virtual ~AttachmentBackend() = default;
    virtual void Validate(bool enabled) = 0;
    virtual std::vector<std::wstring> ReadFilters() = 0;
    virtual void WriteFilters(const std::vector<std::wstring>& filters) = 0;
    virtual bool IsPresent() = 0;
    virtual AttachmentResult Restart() = 0;
};

inline AttachmentResult ConfigureAttachment(AttachmentBackend& backend, bool enabled, bool restartNow) {
    backend.Validate(enabled);
    const auto previous = backend.ReadFilters();
    std::vector<std::wstring> next;
    bool found = false;
    for (const auto& filter : previous) {
        if (!IsOurFilter(filter)) next.push_back(filter);
        else if (enabled && !found) { next.push_back(filter); found = true; }
    }
    if (enabled && !found) next.emplace_back(FilterServiceName);
    AttachmentResult result;
    result.changed = next != previous;
    if (result.changed) {
        backend.WriteFilters(next);
        if (backend.ReadFilters() != next)
            throw std::system_error(ERROR_RETRY, std::system_category(), "Device filters changed concurrently; refresh and retry");
    }
    // A repeated call can retry applying a previously saved selection.
    if (restartNow && backend.IsPresent()) {
        result = backend.Restart();
        result.changed = next != previous;
    }
    return result;
}
} // namespace quarbor::detail
