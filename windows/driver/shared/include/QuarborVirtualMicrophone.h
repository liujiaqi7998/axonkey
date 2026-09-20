#pragma once

/* Public control ABI. PCM data is signed little-endian 16-bit mono at 48 kHz.
 * Open with GENERIC_READ | GENERIC_WRITE; one service owns the ingress ring.
 * Only PCM is writable through the mapping. Queue positions remain in kernel
 * memory and are published with COMMIT after the producer finishes copying.
 */
#if !defined(_KERNEL_MODE)
#include <windows.h>
#include <winioctl.h>
#endif

#define QUARBOR_MIC_API_VERSION 1u
#define QUARBOR_MIC_SAMPLE_RATE 48000u
#define QUARBOR_MIC_CHANNELS 1u
#define QUARBOR_MIC_BITS_PER_SAMPLE 16u
#define QUARBOR_MIC_BLOCK_ALIGN 2u
#define QUARBOR_MIC_BYTES_PER_SECOND 96000u
#define QUARBOR_MIC_RING_BYTES 98304u
#define QUARBOR_MIC_DEVICE_PATH L"\\\\.\\QuarborVirtualMicrophone"

#ifdef INITGUID
DEFINE_GUID(GUID_DEVINTERFACE_QUARBOR_MICROPHONE_CONTROL,
    0xe52ea750, 0xb5ce, 0x4d98, 0xb1, 0x23, 0xb5, 0x34, 0x5a, 0x31, 0x09, 0xc4);
#else
extern const GUID GUID_DEVINTERFACE_QUARBOR_MICROPHONE_CONTROL;
#endif

#define IOCTL_QUARBOR_MIC_MAP_RING \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
#define IOCTL_QUARBOR_MIC_START \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
#define IOCTL_QUARBOR_MIC_STOP \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x902, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
#define IOCTL_QUARBOR_MIC_RESET \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x903, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
#define IOCTL_QUARBOR_MIC_QUERY_STATE \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x904, METHOD_BUFFERED, FILE_READ_ACCESS)
#define IOCTL_QUARBOR_MIC_COMMIT \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x905, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)

#define QUARBOR_MIC_STATE_STARTED 0x00000001u
#define QUARBOR_MIC_STATE_MAPPED 0x00000002u
#define QUARBOR_MIC_STATE_ONLINE 0x00000004u

#pragma pack(push, 8)
/* MAP_RING has no input. The address is valid only in the creating process,
 * until that control handle is closed. STOP/RESET retain the mapping.
 */
typedef struct _QUARBOR_MIC_RING_MAPPING {
    ULONG Size;
    ULONG Version;
    ULONGLONG UserAddress;
    ULONG BufferBytes;
    ULONG SampleRate;
    ULONG Channels;
    ULONG BitsPerSample;
    ULONG BlockAlign;
    ULONG Reserved;
    ULONGLONG Generation;
} QUARBOR_MIC_RING_MAPPING, *PQUARBOR_MIC_RING_MAPPING;

/* Copy ByteCount bytes at WritePosition % BufferBytes, wrapping as necessary,
 * issue MemoryBarrier(), then submit COMMIT. Both position and count are in
 * bytes and must be sample aligned. A stale generation/position is rejected.
 * Use QUERY_STATE to determine free space before writing. Never overwrite
 * already committed bytes. Serialize all producer writes and control calls.
 */
typedef struct _QUARBOR_MIC_COMMIT {
    ULONG Size;
    ULONG Version;
    ULONGLONG Generation;
    ULONGLONG WritePosition;
    ULONG ByteCount;
    ULONG Reserved;
} QUARBOR_MIC_COMMIT, *PQUARBOR_MIC_COMMIT;

/* QUERY_STATE has no input. START/STOP/RESET have no input or output.
 * START enables forwarding; STOP disables forwarding and drops queued PCM.
 * RESET drops queued PCM/counters while retaining START state. STOP/RESET
 * increment Generation and reset both positions, invalidating old commits.
 */
typedef struct _QUARBOR_MIC_STATE {
    ULONG Size;
    ULONG Version;
    ULONG Flags;
    ULONG BufferBytes;
    ULONG SampleRate;
    ULONG Channels;
    ULONG BitsPerSample;
    ULONG BlockAlign;
    ULONGLONG Generation;
    ULONGLONG WritePosition;
    ULONGLONG ReadPosition;
    ULONG AvailableBytes;
    ULONG FreeBytes;
    ULONGLONG ForwardedBytes;
    ULONGLONG SilenceBytes;
} QUARBOR_MIC_STATE, *PQUARBOR_MIC_STATE;
#pragma pack(pop)

#ifdef __cplusplus
static_assert(sizeof(QUARBOR_MIC_RING_MAPPING) == 48, "Microphone mapping ABI changed");
static_assert(sizeof(QUARBOR_MIC_COMMIT) == 32, "Microphone commit ABI changed");
static_assert(sizeof(QUARBOR_MIC_STATE) == 80, "Microphone state ABI changed");
#else
C_ASSERT(sizeof(QUARBOR_MIC_RING_MAPPING) == 48);
C_ASSERT(sizeof(QUARBOR_MIC_COMMIT) == 32);
C_ASSERT(sizeof(QUARBOR_MIC_STATE) == 80);
#endif
