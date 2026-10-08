#include <windows.h>
#include <stddef.h>
#include <string.h>
#include <QuarborHidFilter.h>
#include <QuarborVirtualMicrophone.h>

/* No Rust frames unwind through SEH. Caller owns the control handle and has
 * checked every range against MAP_RING/QUERY_STATE. Failure forbids COMMIT. */
DWORD axonkey_guarded_copy(void *destination, const void *source, size_t bytes) {
    __try {
        memcpy(destination, source, bytes);
        MemoryBarrier();
        return ERROR_SUCCESS;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return GetExceptionCode();
    }
}

/* Startup/test validation against actual public C headers. */
size_t axonkey_abi_value(unsigned int index) {
    const size_t values[] = {
        sizeof(QUARBOR_IDENTITY), __alignof(QUARBOR_IDENTITY),
        offsetof(QUARBOR_IDENTITY, Version), offsetof(QUARBOR_IDENTITY, MaxReportSize),
        sizeof(QUARBOR_SWITCH_CONTROL), sizeof(QUARBOR_REMAP_CONFIG),
        offsetof(QUARBOR_REMAP_CONFIG, Entries),
        sizeof(QUARBOR_MIC_RING_MAPPING), __alignof(QUARBOR_MIC_RING_MAPPING),
        offsetof(QUARBOR_MIC_RING_MAPPING, UserAddress), offsetof(QUARBOR_MIC_RING_MAPPING, Generation),
        sizeof(QUARBOR_MIC_COMMIT), offsetof(QUARBOR_MIC_COMMIT, WritePosition),
        sizeof(QUARBOR_MIC_STATE), offsetof(QUARBOR_MIC_STATE, Generation),
        offsetof(QUARBOR_MIC_STATE, AvailableBytes), offsetof(QUARBOR_MIC_STATE, SilenceBytes),
        IOCTL_QUARBOR_QUERY_IDENTITY, IOCTL_QUARBOR_HID_DATA,
        IOCTL_QUARBOR_SET_DATA_FORWARD, IOCTL_QUARBOR_SET_INPUT_BLOCK,
        IOCTL_QUARBOR_MIC_MAP_RING, IOCTL_QUARBOR_MIC_START, IOCTL_QUARBOR_MIC_STOP,
        IOCTL_QUARBOR_MIC_RESET, IOCTL_QUARBOR_MIC_QUERY_STATE, IOCTL_QUARBOR_MIC_COMMIT,
        QUARBOR_API_VERSION, QUARBOR_MAX_REPORT_SIZE
    };
    return index < sizeof(values) / sizeof(values[0]) ? values[index] : (size_t)-1;
}
