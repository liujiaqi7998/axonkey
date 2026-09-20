#pragma once

/**
 * @file QuarborDeviceSetup.h
 * @brief Windows 用户态 C++17 设备挂载及驱动包管理接口。
 *
 * 设备管理需编译 shared/src/QuarborDeviceSetup.cpp，并链接 Setupapi.lib、
 * Cfgmgr32.lib、Advapi32.lib；驱动包管理还需编译
 * shared/src/QuarborDriverPackage.cpp，并链接 Newdev.lib。
 *
 * 枚举与配置不依赖已加载的过滤驱动或设备 IOCTL 句柄；启用挂载前仍必须安装
 * 可用的驱动服务。修改系统配置需要提升后的管理员权限，本接口不自动提权。
 * 所有函数均同步执行；安装、卸载和请求设备重启应在工作线程调用。调用方应
 * 串行化系统配置修改，并在改变设备挂载、安装或卸载前取消相关待决读取、等待
 * 完成及关闭相关 Quarbor 设备句柄；这些函数不会替调用方管理 IOCTL 句柄。
 *
 * Windows API 或配置验证失败通常抛出 std::system_error；标准库分配失败可
 * 抛出 std::bad_alloc，驱动包路径操作还可抛出 std::filesystem::filesystem_error。
 * 修改操作不具备事务回滚保证，异常不代表系统配置完全未变。调用方应捕获
 * 异常、显示错误并重新查询状态。函数返回的字符串、容器由调用方按值持有。
 */
#include <windows.h>
#include <string>
#include <vector>

namespace quarbor {
/** @brief 本驱动在 Windows 服务管理器和设备过滤器列表中使用的固定服务名。 */
inline constexpr wchar_t FilterServiceName[] = L"QuarborHIDFilterDriver";

/** @brief 一个 HID 枚举器下、Keyboard 安装类设备实例的枚举快照。 */
struct KeyboardDevice {
    /** @brief 显示名称：优先友好名称，其次设备描述；均缺失时使用通用名称。 */
    std::wstring name;
    /**
     * @brief Windows Device Instance ID，用于精确选择设备实例。
     * 属于本机设备实例标识，不是 VID/PID 通配条件或跨计算机的全局硬件标识。
     */
    std::wstring instanceId;
    /** @brief 枚举时设备是否在线；默认 false，设备随后仍可能断开。 */
    bool present = false;
    /**
     * @brief 设备 LowerFilters 中是否已保存本驱动服务名；默认 false。
     * 只表示持久化配置，不表示当前设备栈已加载驱动或已启用转发、拦截、改键。
     */
    bool attachmentConfigured = false;
};

/** @brief 保存设备挂载选择及尝试重启设备的结果；保存与即时生效分别报告。 */
struct AttachmentResult {
    /** @brief 本次是否改变设备 LowerFilters 列表；默认 false。 */
    bool changed = false;
    /** @brief 是否尝试通过 DICS_PROPCHANGE 重启所选在线设备；默认 false。 */
    bool restartAttempted = false;
    /**
     * @brief 是否仍需重新连接设备或重启 Windows 以应用配置；默认 true。
     * 未请求重启、设备离线、重启失败或 Windows 要求重启时保持 true；即使
     * changed 为 false，函数也不会据此推断当前运行栈已应用保存的选择。
     */
    bool restartRequired = true;
    /**
     * @brief 重启或读取重启结果失败时的 Win32 错误码；默认 ERROR_SUCCESS。
     * 非零不回滚已保存选择；为零也不一定尝试过重启，应结合其他字段判断。
     */
    DWORD restartError = ERROR_SUCCESS;
};

/**
 * @brief 枚举支持配置挂载的 HID Keyboard 设备实例，不改变系统状态。
 * @param presentOnly 默认 true，仅返回在线实例；false 时还返回已配置本驱动
 *        LowerFilters 的离线实例，不包含未配置挂载的离线实例。
 * @return 设备快照列表；没有匹配项时返回空列表，不保证枚举顺序。
 * @throws std::system_error 设备枚举、实例 ID 或属性读取失败时抛出。
 * @note 无需已安装或已加载驱动，也无需打开设备 IOCTL 句柄。适用范围由 HID
 *       枚举器和 Keyboard 安装类决定，不按 USB VID/PID 或物理连接方式筛选。
 */
std::vector<KeyboardDevice> EnumerateHidKeyboards(bool presentOnly = true);

/**
 * @brief 保存指定 HID Keyboard 实例是否挂载本驱动，并可请求立即应用。
 * @param instanceId 完整且非空的 Device Instance ID；不得含内嵌 NUL，长度必须
 *        小于 MAX_DEVICE_ID_LEN。只匹配该实例，不支持 VID/PID 通配。
 * @param enabled true 向设备 LowerFilters 添加本驱动；false 删除本驱动项。
 * @param restartNow 默认 true，对在线实例请求 DICS_PROPCHANGE；false 仅保存
 *        配置。重复设置相同选择也可重新请求重启，以应用此前保存的配置。
 * @return 持久化列表是否变化及设备重启结果；重启失败通过 restartError 返回。
 * @throws std::system_error 实例无效、不属于 HID Keyboard、存在类级过滤
 *         注册、启用时驱动服务不可用、读取或保存/回读属性失败时抛出。
 * @note 需要提升后的管理员权限。调用前关闭该设备的 Quarbor IOCTL 句柄。
 *       保留其他驱动的过滤器项；选择写入该实例的 LowerFilters，退出软件、
 *       重连或重启 Windows 后仍有效；实例 ID 改变时需为新实例重新设置。
 *       离线设备在重连时应用保存选择；本函数不主动禁用设备或重启 Windows。
 *       请求设备重启可能短暂中断该键盘输入。挂载本身不启用转发或拦截。
 *       保存后发生重启失败或后续异常时，已写入的选择不会自动回滚。
 */
AttachmentResult SetDeviceAttachment(const std::wstring& instanceId,
    bool enabled, bool restartNow = true);

/** @brief 正常返回时的驱动包管理结果；操作异常时不返回部分结果。 */
struct DriverPackageResult {
    /**
     * @brief Windows 安装 API 或过滤器清理是否要求重启；默认 false。
     * 设备无法即时卸载或移除类级过滤注册时也可能为 true；由调用方提示
     * 用户重启，本接口不会自动执行 Windows 重启。
     */
    bool rebootRequired = false;
    /**
     * @brief 成功执行包安装/卸载的数量；默认 0。
     * 安装成功时为 1，包括重复安装同一包；卸载时为成功删除的已发布包数量。
     * 不表示设备数量，也不包含单独清理的过滤器注册数量。
     */
    unsigned packagesChanged = 0;
};

/**
 * @brief 只读检查待安装包的文件及 INF 身份，返回可传递给安装 API 的 INF 路径。
 * @param directory 包所在目录，可为相对路径或绝对路径；相对路径基于进程当前
 *        工作目录解析。目录中应包含 QuarborHIDFilterDriver.inf、.sys、.cat。
 * @return 验证通过的 INF 绝对路径。
 * @throws std::system_error 缺少文件、INF 读取失败，或不符合本驱动 x64 primitive
 *         INF 的服务、提供者、安装类及目录文件等身份要求时抛出。
 * @note 无需设备句柄或已安装驱动。仅验证文件存在及 INF 元数据，不验证 CAT
 *       签名、文件哈希或发布者可信度，也不防止校验后文件被替换。调用方应提供
 *       可信且不受非授权写入的包目录；正式安装时由 Windows 执行签名策略检查。
 */
std::wstring ValidateDriverPackage(const std::wstring& directory);
/**
 * @brief 只读查找 Windows 中已发布的本驱动包，包括匹配身份的已安装包。
 * @return 按路径排序的已发布 oem*.inf 绝对路径；未找到时为空列表。
 * @throws std::system_error Windows INF 目录、INF 内容或原始文件名查询失败时抛出。
 * @note 通过原始 INF 文件名及服务、提供者、CAT 等元数据识别；不会仅按服务名
 *       推断归属。该识别不构成密码学签名验证。无需本地安装包或设备 IOCTL 句柄。
 */
std::vector<std::wstring> FindInstalledDriverPackages();
/**
 * @brief 安装验证通过的驱动包，检查驱动服务并清理类级过滤器注册。
 * @param directory 包目录，文件要求与 ValidateDriverPackage 相同。
 * @param owner 可选 Windows 安装界面的父窗口句柄，默认 nullptr；调用方保留
 *        句柄所有权，并确保非空句柄在整个同步调用期间有效。
 * @return 安装计数及是否需要重启 Windows；正常返回时 packagesChanged 为 1。
 * @throws std::system_error 包验证、Windows 安装、服务验证或注册清理失败时抛出。
 * @note 需要提升后的管理员权限，应在工作线程且关闭所有 Quarbor IOCTL 句柄
 *       后调用。安装保留已有的按设备挂载选择，不自动选择新设备。首次使用
 *       需再调用 SetDeviceAttachment。可能显示 Windows 安装界面，不自动重启
 *       Windows。安装成功后的验证或清理仍可能失败，异常不会撤销已安装包。
 */
DriverPackageResult InstallDriverPackage(const std::wstring& directory, HWND owner = nullptr);

/**
 * @brief 清理本驱动的设备/类级过滤注册并卸载匹配的全部已发布驱动包。
 * @param owner 可选 Windows 安装界面的父窗口句柄，默认 nullptr；非空句柄需
 *        在整个同步调用期间有效，函数不接管其所有权。
 * @return 卸载包数量及是否需要重启 Windows；没有已发布包时数量可为 0，仍会
 *         清理遗留注册，且仍可能报告需要重启。
 * @throws std::system_error 已安装包发现、过滤注册清理/回读验证或包卸载失败时
 *         抛出；某个包删除失败后不会继续删除后续包。
 * @note 需要提升后的管理员权限，应在工作线程且关闭所有 Quarbor IOCTL 句柄
 *       后调用。无需本地安装包。清理包括离线实例；保留其他过滤驱动条目，并
 *       在删除包前确认本驱动注册已清除，再请求重启受影响的在线设备。设备
 *       无法即时重启时通过 rebootRequired 提示后续重启，不强制禁用设备、
 *       关闭其他进程句柄或自动重启 Windows。操作可能短暂中断键盘输入。
 *       异常时可能已清除挂载选择或删除部分包；应查询状态，按错误提示重试。
 */
DriverPackageResult UninstallDriverPackages(HWND owner = nullptr);
// Preserves completed operations and reported reboot requirements even if a
// later package removal throws. The caller initializes result before the call.
void UninstallDriverPackages(DriverPackageResult& result, HWND owner = nullptr);
} // namespace quarbor
