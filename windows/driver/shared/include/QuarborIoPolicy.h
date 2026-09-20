#pragma once
#include "QuarborHidFilter.h"

/* Shared with user-mode boundary tests. Validate before retaining requests or
 * mapping their buffers. One pending read is sufficient for the streaming API. */
#define QUARBOR_MAX_PENDING_READS 1u
#define QUARBOR_RETRY_BATCH_SIZE 64u

/* Applied to a validated report before any remapping. User-mode observation
 * is independent of input blocking; blocking always takes priority over remap. */
#define QUARBOR_REPORT_COPY_TO_USER 0x1u
#define QUARBOR_REPORT_BLOCK_INPUT 0x2u
#define QUARBOR_REPORT_TRY_REMAP 0x4u

static __inline ULONG QuarborReportActions(int dataForwardEnabled, int inputBlocked)
{
    return (dataForwardEnabled ? QUARBOR_REPORT_COPY_TO_USER : 0u) |
        (inputBlocked ? QUARBOR_REPORT_BLOCK_INPUT : QUARBOR_REPORT_TRY_REMAP);
}

static __inline int QuarborKnownIoctl(ULONG code)
{
    return code == IOCTL_QUARBOR_QUERY_IDENTITY ||
        code == IOCTL_QUARBOR_HID_DATA || code == IOCTL_QUARBOR_SET_REMAP ||
        code == IOCTL_QUARBOR_QUERY_REMAP || code == IOCTL_QUARBOR_CLEAR_REMAP ||
        code == IOCTL_QUARBOR_SET_DATA_FORWARD || code == IOCTL_QUARBOR_SET_INPUT_BLOCK ||
        code == IOCTL_QUARBOR_DELETE_REMAP;
}

static __inline int QuarborValidIoctlLengths(ULONG code, size_t input, size_t output)
{
    switch (code) {
    case IOCTL_QUARBOR_QUERY_IDENTITY:
        return input == 0 && output >= sizeof(QUARBOR_IDENTITY) && output <= QUARBOR_MAX_REPORT_SIZE;
    case IOCTL_QUARBOR_HID_DATA:
        return input == 0 && output > 0 && output <= QUARBOR_MAX_REPORT_SIZE;
    case IOCTL_QUARBOR_SET_REMAP:
        return input == sizeof(QUARBOR_REMAP_CONFIG) && output == 0;
    case IOCTL_QUARBOR_QUERY_REMAP:
        return input == 0 && output >= sizeof(QUARBOR_REMAP_CONFIG) && output <= QUARBOR_MAX_REPORT_SIZE;
    case IOCTL_QUARBOR_CLEAR_REMAP:
        return input == 0 && output == 0;
    case IOCTL_QUARBOR_DELETE_REMAP:
        return input == sizeof(QUARBOR_REMAP_DELETE) && output == 0;
    case IOCTL_QUARBOR_SET_DATA_FORWARD:
    case IOCTL_QUARBOR_SET_INPUT_BLOCK:
        return input == sizeof(QUARBOR_SWITCH_CONTROL) && output == 0;
    default:
        return 0;
    }
}

static __inline int QuarborIoctlRequiresWrite(ULONG code)
{
    return code == IOCTL_QUARBOR_SET_REMAP ||
        code == IOCTL_QUARBOR_CLEAR_REMAP || code == IOCTL_QUARBOR_SET_DATA_FORWARD ||
        code == IOCTL_QUARBOR_SET_INPUT_BLOCK || code == IOCTL_QUARBOR_DELETE_REMAP;
}

static __inline int QuarborValidRemapConfig(const QUARBOR_REMAP_CONFIG* config)
{
    if (config == NULL || config->Size != sizeof(*config) || config->Version != QUARBOR_API_VERSION ||
            config->Reserved != 0 || config->Count > QUARBOR_MAX_REMAP_ENTRIES) return 0;
    for (ULONG index = 0; index < QUARBOR_MAX_REMAP_ENTRIES; ++index) {
        const QUARBOR_REMAP_ENTRY* entry = &config->Entries[index];
        if (index >= config->Count) {
            if (entry->SourceUsage != 0 || entry->TargetUsage != 0) return 0;
            continue;
        }
        /* Usages 1..3 are keyboard error/rollover markers, never key IDs. */
        if (entry->SourceUsage < 4 || (entry->TargetUsage != 0 && entry->TargetUsage < 4) ||
                entry->SourceUsage == entry->TargetUsage) return 0;
        for (ULONG previous = 0; previous < index; ++previous) {
            if (config->Entries[previous].SourceUsage == entry->SourceUsage) return 0;
        }
    }
    return 1;
}

static __inline int QuarborValidRemapDelete(const QUARBOR_REMAP_DELETE* removal)
{
    return removal != NULL && removal->Size == sizeof(*removal) &&
        removal->Version == QUARBOR_API_VERSION && removal->Reserved == 0 && removal->SourceUsage >= 4;
}
