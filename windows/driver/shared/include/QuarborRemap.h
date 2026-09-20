#pragma once
#include "QuarborHidFilter.h"

/* Fixed workspace shared by the driver and deterministic user-mode tests.
 * Descriptor/configuration validation happens once, before a table is stored. */
#define QUARBOR_MAX_ACTIVE_USAGES 256u

typedef struct _QUARBOR_REMAP_PLAN {
    ULONG RemoveCount;
    ULONG AddCount;
    USHORT Remove[QUARBOR_MAX_REMAP_ENTRIES];
    USHORT Add[QUARBOR_MAX_REMAP_ENTRIES];
} QUARBOR_REMAP_PLAN;

/* Idempotent deletion from an already validated table. Preserve table order
 * and its all-zero unused tail, so subsequent QUERY can be passed to SET. */
static __inline int QuarborDeleteRemapEntry(QUARBOR_REMAP_CONFIG* config, USHORT source)
{
    if (config == NULL || config->Count > QUARBOR_MAX_REMAP_ENTRIES || source < 4) return 0;
    for (ULONG index = 0; index < config->Count; ++index) {
        if (config->Entries[index].SourceUsage != source) continue;
        for (ULONG next = index + 1; next < config->Count; ++next) {
            config->Entries[next - 1] = config->Entries[next];
        }
        --config->Count;
        config->Entries[config->Count].SourceUsage = 0;
        config->Entries[config->Count].TargetUsage = 0;
        break;
    }
    return 1;
}

static __inline int QuarborUsagePresent(const USHORT* usages, ULONG count, USHORT usage)
{
    for (ULONG index = 0; index < count; ++index) {
        if (usages[index] == usage) return 1;
    }
    return 0;
}

/* Produce remove/add deltas from the ORIGINAL physical keyboard snapshot.
 * Apply all removals before additions. This preserves swaps, does not cascade
 * mappings and merges physical/mapped targets until their last source releases.
 * Caller must first validate config with QuarborValidRemapConfig. */
static __inline int QuarborPlanRemap(const QUARBOR_REMAP_CONFIG* config,
    const USHORT* active, ULONG activeCount, QUARBOR_REMAP_PLAN* plan)
{
    if (plan == NULL) return 0;
    plan->RemoveCount = plan->AddCount = 0;
    if (config == NULL || config->Count > QUARBOR_MAX_REMAP_ENTRIES ||
            activeCount > QUARBOR_MAX_ACTIVE_USAGES || (activeCount != 0 && active == NULL)) return 0;
    for (ULONG index = 0; index < activeCount; ++index) {
        if (active[index] > 0 && active[index] < 4) return 0;
    }
    ULONGLONG activeMappings[(QUARBOR_MAX_REMAP_ENTRIES + 63u) / 64u] = {0};
    for (ULONG index = 0; index < config->Count; ++index) {
        const USHORT source = config->Entries[index].SourceUsage;
        if (QuarborUsagePresent(active, activeCount, source)) {
            activeMappings[index / 64u] |= 1ull << (index % 64u);
            plan->Remove[plan->RemoveCount++] = source;
        }
    }
    /* Reuse source matches in a bounded bitmap (32 bytes for 256 rules).
     * Each shift stays within its 64-bit word; no second source scan is needed. */
    for (ULONG index = 0; index < config->Count; ++index) {
        const QUARBOR_REMAP_ENTRY* entry = &config->Entries[index];
        if (entry->TargetUsage == 0 ||
                !(activeMappings[index / 64u] & (1ull << (index % 64u)))) continue;
        if (QuarborUsagePresent(active, activeCount, entry->TargetUsage) &&
                !QuarborUsagePresent(plan->Remove, plan->RemoveCount, entry->TargetUsage)) continue;
        if (!QuarborUsagePresent(plan->Add, plan->AddCount, entry->TargetUsage)) {
            plan->Add[plan->AddCount++] = entry->TargetUsage;
        }
    }
    return 1;
}
