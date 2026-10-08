# AxonkeyService Rust 迁移计划

状态：Rust 核心实现及本地自动验证完成。2026-10-08 根据用户后续“测试编译如果没问题就将原来的 c 代码删除”的指示，复测通过后已切换默认构建并删除旧服务。实机及长期验收仍待完成，详见 [实施记录](WINDOWS_SERVICE_RUST_IMPLEMENTATION.md)。

本文后续保留迁移时的原始计划和 C++ 基线事实。原先“先实机验收再删除/切换”的顺序已被上述用户指示调整；已删除源码可从下述 Git 基线恢复，当前模块和构建方式以实施记录及服务 README 为准。

代码基线：`795ecd98e867ca0403ccce404608226f259022c7`，计划分支：`codex/windows-service-rust-plan`。

## 1. 目标与实施决策

将 `windows/service` 的用户态服务逻辑迁移为 Rust，交付可构建、可安装、功能与现有桌面端兼容的 `AxonkeyService.exe`。验收围绕三个目标展开：

1. **功能完整**：设备发现、过滤器挂载、HID 转发、ATVV 语音、虚拟麦克风、RPC、配置及日志逐项覆盖；迁移不能只完成服务壳和协议层。
2. **代码高效、简洁，资源可回收**：业务状态使用安全 Rust；少量 Windows 边界集中封装；明确所有句柄、订阅、线程、任务及映射的所有者，限制队列和连接数量，并用反复启停验证资源回收。
3. **持续可编译**：每阶段均有能链接的 Windows MSVC Release 产物和测试；最后从干净环境构建并通过现有安装链路验收。

采用一个独立 Rust crate，先位于 `windows/service/rust/`，与 C++ 源码并存。第一版保留一个很小的 C/SEH 内存复制函数：现有虚拟麦克风通过 `__try/__except` 捕获映射写入异常，Rust `catch_unwind` 不能等价接替它。这个函数由 Cargo 静态编入 EXE，不引入额外 DLL；设备管理、RPC、GATT、音频及配置逻辑全部使用 Rust。

内核驱动、驱动安装器和桌面 UI 不在本次重写范围内。最终服务继续使用现有驱动的 IOCTL ABI、protobuf 协议和安装位置。不会因更换语言自动获得更低延迟或绝对无泄漏；相关结论以第 7 节的实际验收为准。

## 2. 源码事实与迁移基线

以迁移前 Git 基线源码作为兼容性依据，文档作为辅助。下表中的旧服务文件名指该基线，已删除文件不再链接到当前工作区；仍保留的公共代码及协议可直接访问。

| 基线文件 | 必须迁移的职责 |
| --- | --- |
| `windows/service/main.cpp` | SCM 生命周期、设备协调、HID 端点、RPC 业务处理、设备元数据 |
| [QuarborDeviceSetup.cpp](../windows/driver/shared/src/QuarborDeviceSetup.cpp)、[内部挂载策略](../windows/driver/shared/src/QuarborDeviceSetup.Internal.h) | SetupAPI 枚举、LowerFilters 修改与回读、设备重启；迁移服务实际使用的部分 |
| `windows/service/VoiceReceiver.cpp`、`GattAccess.h` | ATVV 发现、通知、事件排序、电量、GATT 清理和重连 |
| `windows/service/VoiceAudioSession.cpp`、`AdpcmDecoder.h`、`AudioGain.h` | 语音状态机、解码、平滑、重采样、增益、电平 |
| `windows/service/VirtualMicrophoneSink.cpp` | 独占驱动句柄、映射、写入、提交、排空及停止 |
| `windows/service/RpcServer.cpp` | 管道 ACL、帧传输、分发、订阅、慢客户端隔离 |
| `windows/service/ServiceConfig.cpp`、`ServiceLog.cpp` | 64 位注册表配置、日志限制、错误码保持 |
| [axonkey_service.proto](../protobuf/axonkey_service.proto)、[Rust 客户端](../src-tauri/src/service_rpc/mod.rs) | 共同的消息定义、已有客户端兼容性测试 |

基线 `windows/service/CMakeLists.txt` 实际注册两组测试：`axonkey_rpc_tests` 和 `axonkey_rpc_pipe_tests`。当时 README 提到的完整音频、GATT、日志测试和 `VirtualMicrophoneProbe`，迁移前服务构建中未接入，不能作为当时已具备的回归保护。

本轮前序评估已验证：C++ CTest 2/2 通过；桌面端 `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib service_rpc` 为 10 通过、1 忽略。忽略项需要真实运行的服务。它们证明协议已有测试基础，不能证明硬件路径已覆盖。

## 3. 功能实现清单

### 3.1 服务、设备与 HID

| 功能 | Rust 实现要求 | 验收点 |
| --- | --- | --- |
| 服务生命周期 | 使用 `extern "system"` SCM 回调；上报 START_PENDING、RUNNING、STOP_PENDING、STOPPED；STOP/SHUTDOWN 回调只发停止信号；设备通知只请求重扫 | 回调不执行 join 或设备 I/O；退出码和启动失败可诊断；较长清理更新 checkpoint/wait hint |
| 启动顺序 | 读取配置、建立 RPC、注册设备通知、启动协调线程 | `Enabled=0` 时 RPC 可用，不能枚举、挂载或启动 HID/GATT；Rust 版在 RPC 创建失败时上报启动失败并清理，避免没有转发出口仍拦截键盘输入；这是与原版继续启动设备的明确差异，P0/P3 验证 |
| 设备协调 | 启用时每 2 秒或设备通知触发重扫；按端点路径去重；移除失败、断开的端点和结束的语音 worker，后续重扫重建 | 热插拔、蓝牙重连和单设备异常不终止协调线程 |
| RC003 选择 | 保留 HID Keyboard 条件及 `VID_2717` / `VID&012717`、`PID_32B8` / `PID&32B8` 的不区分大小写匹配；端点允许按父设备解析实例 ID | 不挂载到非目标键盘；端点必须属于已识别目标 |
| 过滤器挂载 | 保留其他 LowerFilters，合并/去重自身项；校验驱动服务及类级过滤器状态；写入后回读比较，再尝试重启在线设备 | 并发变化返回可重试错误；需要重启系统的情况可诊断；不强制禁用设备 |
| 过滤器卸载 | 关闭端点和语音资源后，保留现有清理已配置 Quarbor HID Keyboard 实例的范围，包含离线实例；只移除自身过滤器项 | 不将范围悄悄缩小为本进程新挂载的设备，也不扩大为全类清理；重启失败继续清理其他实例 |
| HID 建连 | 独占、overlapped 打开端点；校验 QUERY_IDENTITY 长度、Size、Version、MaxReportSize；先开启转发，再开启输入拦截 | 初始化任意步骤失败均尝试回滚开关并释放资源 |
| HID 报告 | 每端点一个读取线程和一个待完成 HID_DATA IOCTL；保留 Report ID 和完整字节 | 按端点顺序发布 `keyboard`；读线程正常或异常结束均尝试解除拦截并发送空 report reset |
| 停止 HID | 停止信号 → CancelIoEx → 等待 I/O 真正完成 → 解除拦截/转发 → 关闭句柄 | OVERLAPPED、事件和缓冲区在完成前保持有效；桌面端没有残留按下状态 |
| 元数据 | 优先使用语音连接电量，保留设备名称/电量查询回退；有效电量 0–100，未知为 absent；连接内每 30 秒尝试刷新 | GetDevices 失败不影响其他请求；实例 ID 正确转换 UTF-8；不能把 0% 当未知 |

两项已知语义需要在 P0 固定：当前 GATT 匹配主要依赖 VID/PID，不能视为已实现多台同型号设备的唯一绑定；GetDevices 的 `driver_mounted` 当前按目标列表填充。先记录现状及多设备样本，若修正，单独列出行为差异及验收，不借迁移默默改变含义。

### 3.2 RPC 与配置

管道名固定为 `\\.\pipe\AxonkeyService.v1`，协议标识固定为 `axonkey.service.v1`。使用 4 字节小端长度和 protobuf 消息，单帧上限 1 MiB；这不是 gRPC，不引入 HTTP/2 或 tonic。

| 方法 | 必须保留的语义 |
| --- | --- |
| GetServiceInfo | 名称 `AxonkeyService`、版本、协议、管道名和当前增益；服务版本由自身包版本管理，首版显式说明与现有 `0.3.1` 的关系 |
| GetServiceStatus | 返回设备处理总开关，区别于 SCM Running |
| SetServiceStatus | 开启后执行协调，关闭后停止 HID/GATT 并解除挂载，保存 Enabled；现有代码先修改运行状态后持久化，保存失败返回失败但运行状态可能已变化，必须测试此语义 |
| SetServiceEnable | 修改 SCM Automatic / Manual；不修改设备处理开关，也不停止当前服务 |
| SetAudioGain | 请求值截断到 -30～30 dB；更新现有会话并保存；持久化失败返回失败，不能假称运行配置已回滚 |
| GetDevices | 保留所有字段及 battery_level 的 optional 语义 |
| GetVoiceStatus | 保留状态字段及单条聚合响应；多个设备同时存在时必须规定稳定的选择规则并测试 |
| GetAudioLevel | 返回增益后 PCM 的 peak、RMS、时间戳快照 |
| Subscribe | 支持 keyboard、audio_level、voice_status、service_issues；先将确认响应排入同一发送队列，再开放订阅事件 |

请求 ID、Response.success/error/payload、OperationResult、未知方法和损坏 payload 的响应方式全部与现有客户端互通；不要求 protobuf 字节字段排列完全一致，应比较解码后的内容。负增益和 optional 电量必须有 C++/Rust 交叉编解码样本。

每客户端一个接收任务和一个发送任务，共用服务 RPC runtime；保留 256 帧 / 4 MiB 出站上限和每帧 2 秒写超时。一次发布只编码一次，使用 `Arc<[u8]>` 向各客户端入队；满队列断开该客户端。超时或取消造成半帧时关闭整个连接，不能继续在同一连接发送下一帧。键盘 report/reset 不能静默丢弃后继续使用连接。

增加服务端总连接上限，初始设为 32（新约束，需在兼容性测试中覆盖桌面端的订阅连接和短连接）。队列字节预算在出队、断开、任务取消时都必须归还，完成的任务持续回收；不能仅限制每连接队列而让连接和 JoinHandle 集合无限增长。单连接上限之外还应计入一帧正在读/写的内存、解码副本和 Windows 管道缓冲。

当前管道显式 DACL 为 `D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)(A;;GA;;;IU)(A;;GA;;;WD)`。首版显式配置兼容本地普通桌面用户的访问规则，不依赖 LocalSystem 默认 DACL。Tokio 默认拒绝远程客户端；当前 C++ 未设置该标志，计划显式使用 `reject_remote_clients(true)`，作为“本地 RPC”约束的明确差异验收。安全描述符创建失败不能静默使用不确定的默认权限；创建后及时 `LocalFree`。不能直接使用 Tokio 默认 ACL 导致普通用户无法连接。

注册表固定使用 `HKLM\SOFTWARE\Axonkey\Service` 的 64 位视图：Enabled 默认 1；AudioGainDb 默认 2，按有符号 32 位解释 REG_DWORD；RemapConfig 为 1040 字节完整结构，保留验证、默认值回写和仅存储不下发驱动的语义。异常配置、负增益、注册表无写权限均有独立用例。

### 3.3 GATT 与语音状态机

直接使用 `windows` 的 WinRT GATT 投影，保持 ATVV 及 TX/AUDIO/CTL UUID：`AB5E0001`～`AB5E0004`，共同后缀 `5A21-4F05-BC7D-AF01F617B664`。流程为发现服务 → 获取 Session/DeviceId → 打开 BluetoothLEDevice → 获取三个特征 → 注册回调 → 启用 Notify 或 Indicate → 发送 `0A 01 00 00 03 03`。

| 输入/状态 | 兼容行为 |
| --- | --- |
| CTL `0B` | 检查长度、协议版本及 ADPCM codec；按协议读取 frameBytes，0 回退 120 |
| CTL `08` | 新请求先争取虚拟麦克风独占；成功才回复 `0C 00`，旧版本回复 `0C 00 02`；失败不在后续 AUDIO 包里持续抢占 |
| CTL `04` | 处理主动 AUDIO_START 和 sessionId；不重复发开始确认；已打开麦克风时 RESET；同一被拒会话不能借此重抢 |
| CTL `0A` | 按现有字节偏移解析 predictor/index，清理分包与插值状态，在下一帧应用同步 |
| CTL `00` | 现有源码按首字节结束会话，不能仅照 README 收窄为 `00 02`；Flush 尾音、限时排空、STOP、重置状态 |
| 首次 AUDIO | 能力协商成功后允许开启会话；成功打开驱动的首包继续参与解码 |
| 结束后的 AUDIO | 丢弃迟到包，直到新的开始事件 |
| 关闭连接 | 活跃会话按版本发送 `0D sessionId` 或 `0D`；错误不阻止其余本地资源清理 |
| 队列溢出 | AUDIO/CTL 共用一个有序队列，最多 1024 个事件 / 65536 字节；溢出结束本次连接并等待协调重试，不能丢 ADPCM 字节后接着解码 |

回调只复制有界数据并入队；解码、协议处理和麦克风写入由该设备的唯一 worker 执行。保留 Notify 优先、Indicate 回退和 WriteWithoutResponse 优先的能力选择；显式检查 GattCommunicationStatus、空结果、空集合和断连。battery service `180F` / characteristic `2A19` 读取优先 Uncached，再尝试 Cached。

ADPCM 移植保留高半字节先解码、步长表、predictor/index 饱和、跨通知分包、每帧三点平滑、16 kHz → 48 kHz 三倍线性插值，以及最后一个样本 Flush 为三个样本的语义。整数除法和负数右移须用样本对照验证。增益在解码/重采样后只应用一次，四舍五入并饱和到 PCM16；电平基于增益后数据，不能提前到 ADPCM 或放大前计算。

### 3.4 虚拟麦克风、异常与日志

独占打开 `\\.\QuarborVirtualMicrophone`，执行 `MAP_RING → RESET → START`。校验映射长度、版本、地址、缓冲大小和 48 kHz / mono / PCM16 / block-align=2；QUERY_STATE 必须验证 flags、Generation、读写位置、FreeBytes/AvailableBytes 关系和样本对齐。

写入使用当前 WritePosition 的环形位置，每次最多 960 字节，允许回绕；复制完成并执行 Windows MemoryBarrier 后，以当前 Generation 和 WritePosition 提交 COMMIT。无空间时每 1 ms 检查，连续 250 ms 无进展结束输出；正常结束最多等待 250 ms 排空，异常/服务停止及时取消等待。保持蓝牙输入已有节奏，不增加测试音示例中的固定 Sleep(9)。

保留服务问题事件的 code、message、实例 ID、原生错误码、recoverable 和时间戳。覆盖现有 `bluetooth_initialization_failed`、`bluetooth_runtime_failed`、`hid_filter_driver_error`、`virtual_microphone_unavailable`、`virtual_microphone_write_failed`、`virtual_microphone_reset_failed`、`memory_allocation_failed`、`rpc_event_publish_failed`，以及源码中的 `device_reconciliation_failed`。局部故障结束相应设备/会话并允许重试。

日志由 `log` 加小型同步文件 sink 实现，不引入异步日志线程。保留 EXE 同目录 `AxonkeyService.log`、UTF-8、本地时间/时区/毫秒、级别、服务名、PID/TID、调试输出；文件超过 100 KiB 清空后写入，不留轮转副本；消息正文上限 16 KiB，按 UTF-8 边界截断并标记。替换换行和 NUL，每条立即 flush；日志失败仅退回调试输出，不覆盖调用方 GetLastError，不记录音频内容。

## 4. 代码结构与资源所有权

建议保持一个 crate，先按职责拆模块，仅给硬件边界定义可替换的窄 trait，避免为了迁移引入通用插件/调度框架。

```text
windows/service/rust/
  Cargo.toml / Cargo.lock / build.rs
  src/
    main.rs                 # SCM dispatcher 入口
    lib.rs                  # 模块组织、测试入口
    service.rs              # 服务生命周期、协调命令、资源所有者
    rpc.rs                  # 管道、会话、有界发送、protobuf
    config.rs / logging.rs
    audio/
      decoder.rs / gain.rs / session.rs
    win/
      scm.rs / device_setup.rs / hid.rs / gatt.rs
      microphone.rs / abi.rs / handles.rs
  native/seh_copy.c         # 仅保护映射写入和内存屏障
  tests/                    # 协议、资源故障、ABI、硬件显式测试
```

### 4.1 并发方案

- RPC 使用一个 Tokio current-thread runtime，I/O 通过异步任务处理，消除每客户端请求/发送/看门狗三个 OS 线程。连接任务由 JoinSet 等所有者持续收割，关闭时全部取消并等待回收。
- 设备协调保持一个专用线程，独占设备集合；RPC 更新通过容量 64 的命令队列和一次性结果通道进入，队列满时返回繁忙。SCM 停止信号使用独立事件，不能排在普通命令后等待。
- 每 HID 端点一个专用线程，保留已理解的 overlapped IOCTL 模型；第一版不自行构造 DeviceIoControl 的可取消 Rust Future。
- 每语音接收器一个专用 MTA 线程，使用本线程的 Tokio current-thread runtime 等待 WinRT 操作与停止信号。解码/麦克风写入仍由这一个 worker 顺序执行，不创建音频包任务或独立重采样线程。麦克风等待直接检查共享取消标志，避免它占住本地执行器时无法响应停止。
- GATT 回调持有 `Weak` 状态和事件发送入口，不持有整个 Service、连接对象或可回调自身的强引用。状态快照锁只保护短暂复制，不跨 `.await`、join、驱动 I/O 或 GATT 调用。

GetDevices 的蓝牙元数据读取要经过受控的 MTA 工作路径；服务快照与耗时硬件查询分开。若引入缓存，必须说明刷新时机及缺失值语义，不能用陈旧电量冒充本次读取成功。普通控制请求沿用桌面端 3 秒超时作为目标预算；接收器初始化可异步进行，停止/完成后的状态要有实际依据。

### 4.2 每种资源必须有确定的释放方式

| 资源 | 所有者与规则 |
| --- | --- |
| 普通文件/事件/驱动 HANDLE | 使用 `OwnedHandle` 或针对该句柄类别的 RAII wrapper；借用使用 BorrowedHandle/原始借用；禁止重复 from_raw_handle |
| SCM、HDEVINFO、HKEY、设备通知、SDDL 内存 | 分别调用 CloseServiceHandle、SetupDiDestroyDeviceInfoList、RegCloseKey、UnregisterDeviceNotification、LocalFree；不使用通用 CloseHandle 代替 |
| SERVICE_STATUS_HANDLE | 按 SCM 契约使用，不误当普通 HANDLE 关闭；回调 context 由服务主生命周期持有，在 dispatcher 退出后才释放 |
| OVERLAPPED 与输出缓冲 | 由对应 I/O 操作的唯一所有者持有，地址在 pending 期间固定；使用固定分配的 Box/缓冲，必要时 Pin + !Unpin；完成前不得移动、扩容或释放 |
| WinRT 对象与 apartment | GATT owner 在线程上初始化 MTA，显式关闭连接/Session 等适用对象并释放接口，再 RoUninitialize；只平衡成功初始化 |
| GATT 订阅和在途回调 | guard 保存 characteristic/token；释放前停止入队并逐个 RemoveValueChanged；回调可能仍在途，必须用 Weak 和停止标志拒绝访问已关闭资源 |
| 虚拟麦克风映射 | 指针只在独占控制句柄有效期间使用；单线程生产者；停止写入后清空本地指针并关闭控制句柄；由驱动负责映射的释放，不擅自 VirtualFree/UnmapViewOfFile |
| 工作线程和异步任务 | 显式 shutdown → 唤醒/取消 → join/await；不靠丢弃 JoinHandle 脱离运行；停止失败记录并继续清理独立资源 |
| 队列、缓存和订阅者表 | 容量/字节数有限制；连接退出即移除；重连不累积旧事件、strong Arc 或已完成任务 |

服务关闭顺序：停止接受新变更/重扫 → 向所有 worker 发取消信号 → 完成 HID 取消和语音清理并回收 worker（此时 RPC 仍能发布 reset/最终状态）→ 解除过滤器挂载 → 关闭 RPC 接入及客户端、回收任务 → 注销通知并 flush 日志 → 上报 STOPPED。先广播停止再逐个等待，避免多设备的排空时间无谓串联。

析构只作幂等兜底，不在 Drop 中发起异步协议操作或依赖未知时间的普通 join。显式关闭负责可报告错误的工作；涉及 pending I/O 时安全优先，不能为缩短超时释放仍被内核引用的内存。

### 4.3 unsafe、取消和异常边界

1. 纯音频/协议/配置模型模块禁止 unsafe。`win/` 和一个 C ABI 声明集中承载必要 unsafe，每处说明长度、对齐、指针、线程和有效期条件；启用 `unsafe_op_in_unsafe_fn` 检查，拒绝无依据的 `unsafe impl Send/Sync`。
2. IOCTL 结构采用 `#[repr(C)]` 及固定宽度整数，不用 Rust bool/usize 代替 UCHAR/ULONG/ULONGLONG，不盲目使用 packed。对 HID identity=24、switch=4、remap=1040，MIC mapping=48、commit=32、state=80 做尺寸、对齐和字段 offset 测试，并与包含真实驱动头的 C 测试辅助代码交叉比对。
3. CancelIoEx 只是取消请求，必须等完成；无论取消成功、ERROR_NOT_FOUND 或超时，都要按实际完成状态管理缓冲生命周期。若驱动无法完成取消，不能强行释放或声称服务已干净停止；记录为阻断发布的驱动/取消缺陷。[Microsoft 文档](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex)
4. WinRT 等待包装同时处理业务超时和停止信号，显式保存 operation 和同一个固定的 Future，取消后继续等该 Future 的终态并关闭/释放操作，不重新注册第二个 Completed handler；迟到的成功结果也必须关闭。仅丢弃 Tokio timeout 内的 Future 不能证明底层已取消。每设备串行保留一个在途协议操作；发现/订阅/写控制命令的超时初值分别为 10 秒/5 秒/2 秒，需用硬件测量调整；超过取消宽限期不再启动同设备的新操作，避免积累后台请求，SCM 状态如实报告清理进度。
5. 映射内存不暴露为长生命周期 `&mut [u8]`。Rust 校验范围后将原始指针传入小型 C `__try/__except` 函数，异常在 C 内转为错误码；执行真实 Windows MemoryBarrier 后再 COMMIT。任意一段回绕复制失败都禁止提交本次数据，并结束会话。普通 `ptr::copy_nonoverlapping` 加 `catch_unwind` 不提供现有 SEH 保障。
6. 不使用泄漏 Box/Arc、forget 或 detached thread 解决生命周期编译错误。正常业务错误返回 Result，停止路径不 unwrap；FFI 回调边界捕获可展开的 Rust panic 并转成错误/停止信号，不允许 unwind 穿过系统 ABI。
7. 保留 `panic = "unwind"` 以便边界隔离，但它不覆盖 SEH、进程终止或 Rust 默认分配器 OOM abort。可控大分配先校验预算并使用 fallible reserve；不能承诺所有 OOM 都像 C++ bad_alloc 一样可恢复。

### 4.4 简洁与性能要求

复用 HID/PCM 缓冲和 ADPCM 临时存储，以游标消费分包，避免频繁从 Vec 头部删除；先锁定样本输出一致，再做这些优化。增益只在 dB 变化时重新计算倍率。保持每连接响应与事件串行发送，对发布者只做有界入队，不在硬件回调里等待管道。

不引入全局 `Arc<Mutex<Service>>`、逐包 spawn、额外实时音频框架或复杂无锁队列。已有限的短临界区足够时优先 Mutex 和明确的单所有者状态。首版不减少既有事件频率；后续若合并状态通知，应单独测量桌面端行为。

## 5. 可编译方案及已验证内容

### 5.1 已完成的隔离编译核验

2026-10-08 在本机 `rustc 1.98.0 (88d9e12ae 2026-08-18)`、`x86_64-pc-windows-msvc` 下，已在被 Git 忽略的 `.build/service-rust-plan-probe/` 创建编译探针，完成：

- Release 构建和 EXE 链接，带调试信息及 `-C target-feature=+crt-static`；使用 cc 编译并链接 C/SEH 复制函数。
- 编译真实 GATT 服务发现、特征查询、ValueChanged 注册/解除、写命令、Cancel/await/Close 调用；编译 SCM callback、SetupAPI、overlapped IOCTL 和 Tokio 带 SECURITY_ATTRIBUTES 的管道调用。
- 从本仓库 `.proto` 用 vendored protoc + Prost 生成代码；运行 protobuf 往返和普通有效缓冲区的 C 复制检查，均通过。

实际解析组合为 windows **0.61.3**、windows-future **0.2.1**、Tokio **1.53.2**、Prost/prost-build **0.13.5**、winreg **0.55.0**、protoc-bin-vendored **3.3.0**、cc **1.6.0**、log **0.4.34**。这些是本次探针版本，不是要求升级桌面端的依赖。

发现并修正了一个真实编译问题：windows 0.61.3 自身的 `std` feature 不会自动开启 windows-future 0.2.1 的 `std`；如果计划使用 `.await` / `IntoFuture`，必须显式添加下面的 windows-future 依赖，否则会出现 E0277/E0599。后续若升级 windows 系列，需重新验证配套版本和 feature。

这证明关键绑定和构建组合可用，不代表完整服务已实现或硬件逻辑通过。探针未执行 SCM 注册、设备挂载、GATT、驱动访问或异常地址写入，也未验证安装环境的全部 DLL 依赖。探针只作为本地评估产物；P1 应将有价值的编译覆盖移入正式源码/测试，不依赖 `.build` 缓存复现。

### 5.2 Cargo 配置起点

下面是首阶段 `windows/service/rust/Cargo.toml` 的拟定配置，依赖组合已由探针核验；整个正式服务仍须逐阶段实现并编译。Windows API 的 Rust 方法名可能与 C++ 重载名不同，例如 `GetCharacteristicsForUuidWithCacheModeAsync`、`WriteValueWithOptionAsync`、`RemoveValueChanged`，应以锁定版本绑定为准。

```toml
[package]
name = "axonkey-service"
version = "0.3.1"
edition = "2021"
rust-version = "1.98"
publish = false

[[bin]]
name = "AxonkeyService"
path = "src/main.rs"

[dependencies]
prost = "=0.13.5"
log = "=0.4.34"

[target.'cfg(windows)'.dependencies]
windows = { version = "=0.61.3", features = [
  "Foundation", "Devices_Bluetooth_GenericAttributeProfile",
  "Devices_Enumeration", "Storage_Streams",
  "Win32_Foundation", "Win32_Devices_DeviceAndDriverInstallation",
  "Win32_Security_Authorization", "Win32_Storage_FileSystem",
  "Win32_System_IO", "Win32_System_Services", "Win32_System_Threading",
  "Win32_System_WinRT", "Win32_UI_WindowsAndMessaging",
  "Win32_System_Registry", "Win32_System_Diagnostics_Debug",
  "Win32_System_Time", "Win32_System_SystemInformation",
] }
windows-future = { version = "=0.2.1", features = ["std"] }
tokio = { version = "=1.53.2", features = ["rt", "net", "time", "io-util", "sync", "macros"] }
winreg = "=0.55.0"

[build-dependencies]
prost-build = "=0.13.5"
protoc-bin-vendored = "=3.3.0"
cc = "=1.6.0"

[profile.release]
debug = 2
panic = "unwind"
```

P1 创建并提交该二进制 crate 的 Cargo.lock；后续构建统一 `--locked`。初次构建允许正常下载依赖，不把 `--offline` 当干净环境前提。toolchain 固定到已验证的 1.98.0，并显式使用 `cargo +1.98.0`；不要假设从仓库根传入 `--manifest-path` 会自动应用子目录的 rust-toolchain 或 `.cargo/config.toml`。

build.rs 基于 `CARGO_MANIFEST_DIR` 定位 `../../../protobuf/axonkey_service.proto` 和 `native/seh_copy.c`，使用 `prost_build::Config::protoc_executable` 指定 vendored protoc，生成文件写 OUT_DIR，声明 rerun-if-changed。SEH 编译仅在 Windows MSVC 目标执行，并让 cc 的 CRT 设置与 Rust `crt-static` 一致。配置应对非 Windows/非 MSVC 运行目标给出清楚的错误，不产生可误打包的空壳服务。

首次无需抽出共享 Rust workspace：桌面端和新服务各自由同一份 proto 生成类型；从客户端提取传输算法时去除 Tauri 依赖。待确有公共代码再建小型共享 crate，服务不得依赖 Tauri 或 WebView。

### 5.3 构建、打包和安装

构建仍需 MSVC Build Tools、Windows SDK 和目标 linker；保留 C/SEH 小函数意味着仍需 cl.exe。切换后服务不再依赖 CMake/Ninja、C++/WinRT 头、nanopb、spdlog，但驱动/安装器的独立构建需求不受影响。

修改 [build-windows-service.mjs](../scripts/build-windows-service.mjs) 为调用 Cargo，保持 `npm run build:windows-service` 入口和非 Windows 跳过行为。构建子进程显式设置静态 CRT，限定为服务子进程，不污染随后 Tauri 的编译环境；从 Cargo 产物复制 EXE 和配套 PDB 到 `windows/service/dist/`，编译失败时不能复制或继续打包旧 EXE。

[tauri.windows.conf.json](../src-tauri/tauri.windows.conf.json)、[服务管理脚本](../scripts/manage-windows-service.ps1) 继续消费同一路径/名称，服务仍由 LocalSystem 运行，安装到 `%ProgramData%\Axonkey\service\AxonkeyService.exe`。验证复制后的构建时间与原有升级检测一致；PDB 作为对应发布诊断产物保存。安装/升级按现有停止、替换、注册和启动流程执行，C++ 与 Rust 服务不能同时竞争生产管道、HID 或麦克风。

新增 CI job 使用固定 Windows/MSVC 环境，实际执行下列层级的检查；普通 CI 不要求真实设备：

```powershell
# 以下命令在正式 crate 建立后执行；文档当前不声称已完成整服务构建。
cargo +1.98.0 fmt --manifest-path windows/service/rust/Cargo.toml -- --check
cargo +1.98.0 check --locked --manifest-path windows/service/rust/Cargo.toml --all-targets --target x86_64-pc-windows-msvc
cargo +1.98.0 clippy --locked --manifest-path windows/service/rust/Cargo.toml --all-targets --target x86_64-pc-windows-msvc -- -D warnings
cargo +1.98.0 test --locked --manifest-path windows/service/rust/Cargo.toml --target x86_64-pc-windows-msvc
cargo +1.98.0 test --locked --release --manifest-path windows/service/rust/Cargo.toml --target x86_64-pc-windows-msvc

# 由服务构建脚本在独立子进程内设置并使用；这里示意同等构建参数。
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
cargo +1.98.0 build --locked --release --manifest-path windows/service/rust/Cargo.toml --target x86_64-pc-windows-msvc
```

以上静态 CRT 环境变量限于构建会话；实际 CI 对测试也应用相同 target 设置，检查 Debug/Release 行为差异。随后检查 EXE/PDB 输出，并在 Developer PowerShell 用 `dumpbin /dependents` 检查 CRT/DLL 依赖；最终在没有开发环境 VC runtime 的目标机验证启动。不能以 cargo check 代替 EXE 链接和安装验收。

## 6. 实施阶段与退出条件

| 阶段 | 工作 | 阶段完成条件 |
| --- | --- | --- |
| P0：行为基线 | 收集各 RPC 样本、CTL/AUDIO 序列和 PCM 期望；补可运行的 C++ 行为测试/夹具；记录 ACL、持久化失败、聚合状态、多设备等已知差异 | 样本有来源和版本；覆盖第 3 节边界；可对同一输入比较两种实现 |
| P1：构建与 ABI | 新建 crate/lock/build.rs；加入真实头文件 ABI 比对、SEH 函数和 Windows API 编译覆盖；固定工具链、依赖和静态 CRT | 干净环境 Release EXE/PDB 可生成；关键 API 无占位实现；ABI 校验通过 |
| P2：纯逻辑与 RPC | 移植 decoder/gain/session、配置和日志；实现 9 个 RPC 方法、4 类事件、有界连接/出站队列；此阶段硬件 handler 可以使用明确的测试替身 | PCM 和会话行为对照通过；C++ codec 与 Prost 双向兼容；真实命名管道测试通过；替身不进入最终生产产物 |
| P3：SCM、SetupAPI、HID | 实现协调、挂载/卸载、设备通知、动态开关、真实 IOCTL 和 cancel/drain；处理分阶段初始化失败 | 真实设备键盘映射正常；反复启停/热插拔后原始输入恢复；普通用户桌面可连接 LocalSystem 服务 |
| P4：GATT 与虚拟麦克风 | 实现完整发现、取消、订阅、状态机、映射及写入，接入元数据与问题事件；补实际输出回采工具 | RC003 首包、分包、尾音、拒绝抢占、断连重连、停止取消和录音回采通过；超时无遗留后台操作 |
| P5：持续运行与切换 | 泄漏/延迟/CPU 验收，干净构建及安装升级回滚，切换 npm 构建入口，更新真实文档和依赖 notices | 所有正式 handler 有实现、CI 与硬件矩阵通过；产物可直接替换；保留经核验的 C++ 回滚产物 |

每阶段保持 build/test 通过。P1 要在大规模移植前验证关键调用；P3/P4 发现硬件访问或停止行为不满足要求时先解决，不能用成功编译替代运行验收。

迁移阶段将 C++/Rust 产物放在不同构建目录，仅在 P5 让默认构建写入生产 dist。比较硬件行为时按顺序停止一个实现再启动另一个；无硬件 RPC 测试使用带进程 ID 的测试管道。最终删除 C++ 业务实现和旧构建入口的时机取决于回滚验证，不在 P1 提前删除。

## 7. 三项验收标准

### 7.1 功能验收

| 测试层 | 必需场景 |
| --- | --- |
| 无硬件单元测试 | ADPCM 全半字节输入、跨包/同步/Reset/Flush、负样本插值、全部增益边界与削波；CTL 所有路径，重复开始、迟到 AUDIO、被拒会话；remap 校验、非法配置和日志失败 |
| 故障注入 | 每个资源获取步骤失败后的释放；MAP_RING/QUERY/COMMIT 错误、Generation 变化、写回绕、250 ms 满缓冲/排空及取消；其他过滤器项保留、写后回读失败 |
| 协议/真实管道 | 九方法、四事件、空/碎片/合并/超长/损坏帧、负 int32、optional 电量、响应 ID、订阅响应顺序；慢读/不读客户端、半帧断开、最大连接数、反复连接后任务回收 |
| Windows 服务与设备 | LocalSystem 身份、普通桌面用户访问、开机自动启动、禁用设备处理、增益实时变更、硬件缺失和恢复、蓝牙关闭/恢复、休眠唤醒、设备移除、服务停止 |
| 音频端到端 | 实际 RC003 → Rust 服务 → Quarbor Virtual Microphone → WASAPI 回采；单独注入已知测试音验证输出；多设备独占及同型号映射问题有明确结果 |
| 发布 | 项目声明支持的 Windows 版本至少覆盖 Win10/Win11 x64 对应测试机；安装、升级、卸载、无开发工具环境启动、C++ 回滚均可执行 |

需要硬件/管理员权限的测试显式标记、单独运行和记录结果；普通 cargo test 不修改真实键盘的 LowerFilters、不安装服务。使用隔离注册表测试键、临时日志目录和随机测试管道。

### 7.2 无泄漏与效率验收

“无内存泄漏”落地为：安全 Rust 所有权检查 + FFI 释放审查 + 错误路径计数 + 长时间实际测量。Rust 本身不能排除 Arc 环、未解绑订阅或遗留异步任务。

- 测试替身记录 Open/Close、subscribe/unsubscribe、map/close、spawn/join 及队列预算，每次退出/初始化失败后数量成对，活跃对象归零；每个取消路径都必须覆盖。
- 同一进程内连续 1000 次模拟设备连接/语音会话及 1000 次 RPC 连接/断开；真实设备至少 100 次会话/连接循环；服务 SCM 启停单独做 100 次。不能只通过重启进程清零来证明无泄漏。
- 预热后持续运行至少 8 小时，周期记录 Private Bytes、句柄数、线程数、在途 I/O/WinRT 操作、任务数和队列占用。无活动时已登记资源回到基线；堆容量可保留但须稳定，若随轮数持续增长，使用 WPR/堆栈或 Application Verifier 定位，不因 RSS 未归零就直接判泄漏，也不因进程未崩溃就判通过。
- 同机同设备同录音客户端比较 C++/Rust：空闲/活动 CPU、Private Bytes、HID 入站至 RPC 交付的 p50/p95/p99、语音开始至首个 PCM COMMIT 的分位数、掉包/超时率。初始目标为稳定场景 CPU、内存和 p95 延迟不超过基线约 10% 的回退，必须同时报告绝对值；接近零的空闲 CPU 按测量噪声判断，不能只算比例。
- Release 模式执行压力测试；硬件输入吞吐无需人为节流，活动客户端增加不带来对应数量的 OS 发送/看门狗线程。Miri 如使用，仅验证纯 Rust 模块，不能证明 Win32/驱动共享内存安全。

### 7.3 可编译与可交付验收

最终必须同时满足：固定 toolchain + 已提交 Cargo.lock；干净 checkout 在 MSVC 目标完成 fmt/check/clippy/test/build；Release EXE/PDB 对应；无未实现 handler、`todo!()` 或生产 mock；ABI 尺寸/offset 通过；静态 CRT 和依赖检查通过；现有 Tauri 打包能包含新产物；安装后的服务真实运行并通过硬件验收。只有这些条件满足后才切换默认实现。

## 8. 参考资料

- [Microsoft Rust GATT 投影](https://microsoft.github.io/windows-docs-rs/doc/windows/Devices/Bluetooth/GenericAttributeProfile/struct.GattDeviceService.html)：可验证服务发现、Session、特征查询、Close 等投影能力；在线文档当前版本与本计划锁定的 0.61.3 可能不同，以实际编译为准。
- [Tokio Named Pipe ServerOptions](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/struct.ServerOptions.html)：管道创建、安全属性和远程客户端选项。
- [Rust CRT 链接说明](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)：`crt-static` 的配置与最终链接检查。
- [HID 公共 ABI](../windows/driver/shared/include/QuarborHidFilter.h)、[虚拟麦克风公共 ABI](../windows/driver/shared/include/QuarborVirtualMicrophone.h)：布局、句柄有效期、提交与取消的项目内依据。
