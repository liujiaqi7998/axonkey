#pragma once

#include <windows.h>
#include <QuarborHidFilter.h>
#include <string>
#include "AudioGain.h"

namespace axonkey_service {

struct ServiceConfig {
    QUARBOR_REMAP_CONFIG remap{};
    std::int32_t audioGainDb = kDefaultAudioGainDb;
    bool enabled = true;
};

bool IsRc003(const std::wstring& instanceId);
ServiceConfig LoadServiceConfig();
bool SaveAudioGain(std::int32_t gainDb);
bool SaveServiceEnabled(bool enabled);
// Reads/materializes values in an already-open key; caller retains ownership.
ServiceConfig LoadServiceConfigFromKey(HKEY key);

} // namespace axonkey_service
