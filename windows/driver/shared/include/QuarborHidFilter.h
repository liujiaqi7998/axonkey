#pragma once

/**
 * @file QuarborHidFilter.h
 * @brief Quarbor HID Keyboard 过滤驱动的公共 C ABI；适用于 C/C++ 用户态客户端和内核。
 *
 * 用户态仅依赖 Windows SDK，不需要 WDK/KMDF。通过 DeviceIoControl 调用下列接口；
 * 先使用 QuarborDeviceSetup.h 的用户态管理 API 安装并选择设备，挂载不是 IOCTL 操作。
 * 每个 HID Keyboard collection 有独立端点，仅 SYSTEM/管理员可打开，且独占访问。
 * 建议使用 GENERIC_READ | GENERIC_WRITE 和 FILE_FLAG_OVERLAPPED 打开。
 *
 * 数据处理顺序：复制未经修改的原始 HID 报告给用户态 -> 全部拦截 Windows 输入
 * -> 未全部拦截时改键。转发不改变报告内容；改键包括目标为 NULL 的单键屏蔽。
 * 两个开关默认关闭，关闭句柄会复位；改键表在关闭句柄后保留，设备释放/电源挂起
 * 或卸载时清除。更改改键/拦截前应释放全部按键，避免按下与松开跨越配置边界。
 *
 * 所有长度单位为字节，usage 是 HID Keyboard/Keypad 页按键码，不是虚拟键或扫描码。
 * 发送结构先整体清零，再设置 Size/Version 和有效字段。不得改变字段顺序、对齐或
 * IOCTL 编号；不兼容改动必须提升 QUARBOR_API_VERSION。完整协议见 docs API 文档。
 */
#if !defined(_KERNEL_MODE)
#include <windows.h>
#include <winioctl.h>
#endif

#ifdef __cplusplus
extern "C" {
#endif

/** 当前二进制协议版本；查询身份和规则时须校验，设置/删除规则时须填写。 */
#define QUARBOR_API_VERSION        8u
/** 完整报告的协议上限；实际设备长度查询 MaxReportSize，包含 Report ID 位置。 */
#define QUARBOR_MAX_REPORT_SIZE    65535u
/** 每设备缓存的完整原始报告数；溢出淘汰最旧报告，不保证无损采集。 */
#define QUARBOR_RING_CAPACITY      64u
/** Windows Keyboard 安装类 GUID；还须满足 HID 枚举器条件才能挂载。 */
#define QUARBOR_KEYBOARD_CLASS_GUID_STRING L"{4D36E96B-E325-11CE-BFC1-08002BE10318}"

/**
 * 每个已挂载 Keyboard collection 发布的控制端点接口类 GUID。
 * 用 SetupAPI 枚举其设备路径；端点的父设备实例 ID 用于与挂载选择匹配。
 * 恰好一个客户端源文件在本头之前包含 <initguid.h> 以生成 GUID 定义，
 * 其他源文件只包含本头即可。此 GUID 标识接口类型，不标识某台物理设备。
 */
#ifdef INITGUID
DEFINE_GUID(GUID_DEVINTERFACE_QUARBOR_HID,
    0x8e7f4d20, 0x0f76, 0x4c4b,
    0x91, 0x6c, 0x1b, 0x4b, 0x0f, 0x7d, 0x63, 0x18);
#else
extern const GUID GUID_DEVINTERFACE_QUARBOR_HID;
#endif

/**
 * 查询端点身份、ABI 版本、报告容量和当前开关，不改变设备状态。需要读权限。
 * 输入：无。输出：QUARBOR_IDENTITY；容量范围 [sizeof(...), MAX_REPORT_SIZE]。
 * 成功返回 sizeof(QUARBOR_IDENTITY) 字节；MaxReportSize=0 表示数据功能不可用。
 */
#define IOCTL_QUARBOR_QUERY_IDENTITY \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS)
/**
 * 读取一份完整的原始报告；需要读权限。无输入，输出容量为 1..MAX_REPORT_SIZE，
 * 推荐分配身份查询的 MaxReportSize。报告包含 Report ID 位置（未编号报告为 0）。
 * 无报告时异步等待，每设备仅允许一个待完成读取；转发关闭不自动取消已有读取。
 * 缓冲不足返回错误且不消费报告，实际传输字节数为 0。用 CancelIoEx 取消后，
 * 须等待完成才能释放 OVERLAPPED、事件和输出缓冲；数据不是 USB/蓝牙传输层帧。
 */
#define IOCTL_QUARBOR_HID_DATA \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_OUT_DIRECT, FILE_READ_ACCESS)
/**
 * 原子替换此设备的整张改键表。输入 QUARBOR_REMAP_CONFIG，无输出；读写权限。
 * Count=0 清空。参数非法或任一规则不受 descriptor 支持则失败，旧表保持不变。
 * 表仅驻留设备内存；不是增量添加接口。报告解析失败时原报告透传。
 */
#define IOCTL_QUARBOR_SET_REMAP \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
/** 查询完整规则表。无输入；输出容量 [sizeof(QUARBOR_REMAP_CONFIG), MAX_REPORT_SIZE]；读权限。 */
#define IOCTL_QUARBOR_QUERY_REMAP \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_BUFFERED, FILE_READ_ACCESS)
/** 清空当前设备全部改键规则；不改变转发/全部拦截开关。无输入/输出；读写权限。 */
#define IOCTL_QUARBOR_CLEAR_REMAP \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x805, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
/**
 * 仅控制原始 HID 数据向用户态转发；输入 QUARBOR_SWITCH_CONTROL，无输出；读写权限。
 * 关闭时清空未交付缓存；不改变 Windows 输入、全部拦截开关或改键表。
 */
#define IOCTL_QUARBOR_SET_DATA_FORWARD \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x806, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
/**
 * 仅控制屏蔽送往 Windows 的原始输入；输入 QUARBOR_SWITCH_CONTROL，无输出；读写权限。
 * 开启时跳过改键，但数据转发仍可独立提供原始报告；不修改已保存的规则表。
 */
#define IOCTL_QUARBOR_SET_INPUT_BLOCK \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x807, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
/** 按源 usage 删除单条规则，不存在也成功。输入 QUARBOR_REMAP_DELETE，无输出；读写权限。 */
#define IOCTL_QUARBOR_DELETE_REMAP \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x808, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)

/** 改键所用 HID Keyboard/Keypad usage page；所有 SourceUsage/TargetUsage 均隐含此页。 */
#define QUARBOR_HID_USAGE_PAGE_KEYBOARD 0x07u
/** 每设备最多保存的改键规则数；不同源可以共用目标，源必须唯一。 */
#define QUARBOR_MAX_REMAP_ENTRIES 256u

/** 数据转发或全部拦截开关的输入结构，固定 4 字节；只影响所调用接口对应的开关。 */
typedef struct _QUARBOR_SWITCH_CONTROL {
    ULONG Enabled;         /**< 0 关闭；任意非零值开启，建议使用 1。 */
} QUARBOR_SWITCH_CONTROL, *PQUARBOR_SWITCH_CONTROL;

/** 单条 Keyboard/Keypad 按键映射，固定 4 字节；每条源/非 NULL 目标须属于同一 Report ID。 */
#pragma pack(push, 4)
typedef struct _QUARBOR_REMAP_ENTRY {
    USHORT SourceUsage;    /**< 原始按键 usage，至少 0x04；不允许 0 或错误码 0x01..0x03。 */
    USHORT TargetUsage;    /**< 目标 usage：0 表示 NULL 单键屏蔽，否则至少 0x04 且不同于源。 */
} QUARBOR_REMAP_ENTRY, *PQUARBOR_REMAP_ENTRY;

/**
 * 设置/查询的完整规则表，固定 1040 字节，4 字节对齐。
 * 所有规则从同一份原始物理按键快照计算，不产生 A->B->C 连锁转换；多个源共用
 * 目标时合并输出，直到最后一个来源释放才松开。各字段及未使用条目必须按约定初始化。
 */
typedef struct _QUARBOR_REMAP_CONFIG {
    ULONG Size;             /**< sizeof(QUARBOR_REMAP_CONFIG)，不是已使用条目的总长度。 */
    ULONG Version;          /**< 必须为 QUARBOR_API_VERSION。 */
    ULONG Count;            /**< 有效条目数，范围 0..QUARBOR_MAX_REMAP_ENTRIES；0 禁用改键。 */
    ULONG Reserved;         /**< 保留字段，必须为 0。 */
    QUARBOR_REMAP_ENTRY Entries[QUARBOR_MAX_REMAP_ENTRIES]; /**< 前 Count 项有效，其余项整体为 0。 */
} QUARBOR_REMAP_CONFIG, *PQUARBOR_REMAP_CONFIG;

/** DELETE_REMAP 的输入结构，固定 12 字节；只删除指定源，不改变其他规则。 */
typedef struct _QUARBOR_REMAP_DELETE {
    ULONG Size;             /**< 必须为 sizeof(QUARBOR_REMAP_DELETE)。 */
    ULONG Version;          /**< 必须为 QUARBOR_API_VERSION。 */
    USHORT SourceUsage;     /**< 要删除的源 HID usage，至少 0x04。 */
    USHORT Reserved;        /**< 保留字段，必须为 0。 */
} QUARBOR_REMAP_DELETE, *PQUARBOR_REMAP_DELETE;
#pragma pack(pop)

/** QUERY_IDENTITY 的输出，固定 24 字节；数据描述当前端点，不是持久化设备选择。 */
#pragma pack(push, 4)
typedef struct _QUARBOR_IDENTITY {
    ULONG Size;             /**< sizeof(QUARBOR_IDENTITY)；使用结果前同时校验实际返回长度。 */
    USHORT VendorId;        /**< HID 厂商 ID，未知/未指定时为 0；不能用作设备唯一标识。 */
    USHORT ProductId;       /**< HID 产品 ID，未知/未指定时为 0；不用于限定挂载范围。 */
    ULONG Version;          /**< 当前驱动 ABI 版本，客户端必须与 QUARBOR_API_VERSION 一致。 */
    ULONG CollectionNumber; /**< Hardware ID 中的 collection 编号，未知为 0；不是唯一 ID。 */
    ULONG MaxReportSize;    /**< 报告最大字节数，包含 Report ID；0 表示元数据/内存准备失败。 */
    UCHAR DataForwardEnabled;/**< 1 正在复制原始报告给 HID_DATA，0 不生成新的用户态报告。 */
    UCHAR InputBlocked;     /**< 1 全部拦截 Windows 输入；0 放行并按规则尝试改键。 */
    UCHAR RemapEnabled;     /**< 1 表中有规则；不保证每份报告均可解码，也不表示规则正在执行。 */
    UCHAR Reserved[1];      /**< 保留字节，驱动返回 0，客户端不应赋予其其他含义。 */
} QUARBOR_IDENTITY, *PQUARBOR_IDENTITY;
#pragma pack(pop)

/* 编译期 ABI 布局检查；C 分支 typedef 仅用于触发编译错误，不是额外数据结构。 */
#ifdef __cplusplus
static_assert(sizeof(QUARBOR_SWITCH_CONTROL) == 4, "Quarbor ABI changed");
static_assert(sizeof(QUARBOR_REMAP_ENTRY) == 4, "Quarbor ABI changed");
static_assert(sizeof(QUARBOR_REMAP_CONFIG) == 1040, "Quarbor ABI changed");
static_assert(sizeof(QUARBOR_REMAP_DELETE) == 12, "Quarbor ABI changed");
static_assert(sizeof(QUARBOR_IDENTITY) == 24, "Quarbor ABI changed");
#else
typedef char quarbor_switch_control_size_must_be_4[(sizeof(QUARBOR_SWITCH_CONTROL) == 4) ? 1 : -1];
typedef char quarbor_remap_entry_size_must_be_4[(sizeof(QUARBOR_REMAP_ENTRY) == 4) ? 1 : -1];
typedef char quarbor_remap_config_size_must_be_1040[(sizeof(QUARBOR_REMAP_CONFIG) == 1040) ? 1 : -1];
typedef char quarbor_remap_delete_size_must_be_12[(sizeof(QUARBOR_REMAP_DELETE) == 12) ? 1 : -1];
typedef char quarbor_identity_size_must_be_24[(sizeof(QUARBOR_IDENTITY) == 24) ? 1 : -1];
#endif

#ifdef __cplusplus
}
#endif
