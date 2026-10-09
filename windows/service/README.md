# AxonkeyService

Windows 用户态服务已使用 Rust 实现，源码位于 [rust/](rust/README.md)。旧 C++ 服务、CMake 构建和仅供旧服务使用的 C/nanopb 编解码器已移除。驱动、公共 IOCTL 头文件和 [protobuf 协议](../../protobuf/axonkey_service.proto) 保持不变。

服务名为 `AxonkeyService`，可执行文件为 `AxonkeyService.exe`，协议版本为 `axonkey.service.v1`。服务自身版本仍为 `0.3.1`，与桌面应用版本独立。

## 构建和验证

需要 Rust 1.98.0、Visual Studio x64 MSVC 工具链与 Windows SDK。在仓库根目录运行：

```powershell
npm run build:windows-service
npm run test:windows-service
```

默认构建输出 `windows/service/dist/AxonkeyService.exe` 和匹配 PDB，供原有 Tauri 开发和打包入口使用；同时保存一份到 `.build/service-rust/dist`。复制保留源构建时间，兼容桌面端升级检测。

测试入口执行 rustfmt、严格 Clippy、Debug/Release 测试、Release 构建、x64/静态运行库检查和无硬件 ABI 自检。音频回归使用删除前捕获的 107 组 C++ 输出摘要，无需保留或编译旧 C++ 代码。首次构建需要下载锁定依赖。

```powershell
& .\windows\service\dist\AxonkeyService.exe --check
```

`--check` 仅校验公共 C 头文件与 Rust 的 ABI；实际服务必须由 SCM 启动。普通自动测试使用隔离的 HKCU 配置、临时日志和测试管道，不安装服务或修改真实设备。实机验收进度见 [实施记录](../../docs/WINDOWS_SERVICE_RUST_IMPLEMENTATION.md)。

生产构建只保留 `rust/native/boundary.c` 这个很小的 C 边界：用于共享映射的 SEH 异常保护、内存屏障和 ABI 核对。Rust panic 不能替代 Windows 访问异常保护，因此仍需要 MSVC C 编译器；不再需要服务的 C++/WinRT、CMake、Ninja、spdlog 或 nanopb。

## 设备与键盘

服务在启用时枚举 HID Keyboard，按 `VID_2717` / `VID&012717` 和 `PID_32B8` / `PID&32B8` 识别 RC003。它保留其他 LowerFilters，只添加/移除 Quarbor 过滤器，并回读验证。设备通知和每 2 秒扫描用于协调连接。

服务独占打开过滤器端点，校验驱动身份，先开启转发再屏蔽原始输入。完整 HID report 通过 RPC 的 `keyboard` 事件发给桌面端，由桌面端解析和执行映射。端点退出前取消并排空 pending I/O、解除拦截/转发，发布空 report，通知桌面端释放按键。

关闭设备处理或停止服务时，先取消并回收 HID/GATT 工作线程，再解除已配置的 Quarbor HID Keyboard 挂载，包括离线实例。驱动安装仍通过原驱动安装器处理。

## 配置

64 位注册表：`HKLM\SOFTWARE\Axonkey\Service`。

| 值 | 类型与含义 |
| --- | --- |
| Enabled | REG_DWORD，0/1，默认 1；关闭时不启动设备处理 |
| AudioGainDb | REG_DWORD，按有符号 32 位解析，-30～30 dB，默认 2 |
| RemapConfig | REG_BINARY，公共结构完整字节，仅保留存储，不下发驱动 |

缺失或无效值恢复默认值。RPC 运行状态先更新再持久化；写入失败返回失败，不能据此假定运行状态已回滚。`SetServiceEnable` 修改 SCM Automatic/Manual，与设备处理开关 `SetServiceStatus` 分开。

## RC003 语音

每个设备的 MTA 工作线程使用 Windows Rust GATT 投影发现 ATVV 服务，订阅 AUDIO/CTL。UUID 末段固定为 `5A21-4F05-BC7D-AF01F617B664`，服务/TX/AUDIO/CTL 前段分别为 `AB5E0001`～`AB5E0004`。

连接后发送 `0A 01 00 00 03 03` 请求能力；`08` 请求开启麦克风，成功后回复 `0C 00`（旧协议附 codec）；`04` 开始音频，`00` 结束。被拒绝的会话不会中途抢占麦克风，结束后的迟到音频被丢弃；关闭连接时按版本发送 `0D`。

ADPCM 按高半字节优先解码，平滑并从 16 kHz 插值到 48 kHz 单声道 PCM16。增益倍率为 `10^(dB/20)`，饱和后写入；正常结束补齐尾音。音频输出独占打开 `\\.\QuarborVirtualMicrophone`，执行 MAP_RING → RESET → START，经 QUERY_STATE 校验后每次最多复制/提交 960 字节。复制有 SEH 保护和内存屏障；满缓冲区与正常结束排空最长等待 250 ms，可取消。

GATT 回调通过弱引用入队，队列限制 1024 个事件/65536 字节，溢出结束当前连接。WinRT 超时后请求取消并等待同一个操作真正完成，避免遗留后台工作。若驱动永不确认取消，服务可能保持 STOP_PENDING；真实取消行为仍需硬件验证。多台同型号设备沿用旧版 VID/PID 匹配限制。

## 本地 RPC

管道：`\\.\pipe\AxonkeyService.v1`。帧为 4 字节小端长度加 protobuf 消息，最大 1 MiB。服务和桌面端均由 prost 根据同一 schema 生成代码，不使用 gRPC。

支持 GetServiceInfo、GetServiceStatus、SetServiceStatus、SetServiceEnable、SetAudioGain、GetDevices、GetVoiceStatus、GetAudioLevel、Subscribe。订阅事件为 keyboard、audio_level、voice_status、service_issue。订阅确认先入队，再开放事件；设备电量未知为 absent，0% 是有效值。

一个 Tokio runtime 管理管道连接，最多 32 个活动客户端。每客户端出站上限 256 帧/4 MiB，每帧写入超时 2 秒；慢客户端单独断开。允许本机桌面访问，拒绝远程客户端。RPC 绑定失败时不会启动设备拦截。

## 安装和运行

构建不会修改已安装服务。安装驱动后，可使用桌面端服务管理，或管理员 PowerShell：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\manage-windows-service.ps1 -Action Install -ServiceExecutable .\windows\service\dist\AxonkeyService.exe
powershell -ExecutionPolicy Bypass -File .\scripts\manage-windows-service.ps1 -Action Start
```

应用安装位置仍为 `%ProgramData%\Axonkey\service\AxonkeyService.exe`，账户为 LocalSystem。开发辅助脚本 `windows/service/script/Test-AxonkeyService.ps1` 仍管理 dist 中的服务，支持 Install/Uninstall/Start/Stop/Restart/Status；它不安装驱动。

应用内安装每次都会把内置 EXE 复制到上述目录并更新 SCM 的 `BinPath`，即使测试机已有同名服务注册项也会修复旧路径；安装完成前还会等待 `\\.\pipe\AxonkeyService.v1` 可连接。服务管理日志位于 `%ProgramData%\Axonkey\Logs\ServiceManagement.log`，其中会记录实际复制路径、注册路径和管道就绪状态。

## 日志和诊断

`AxonkeyService.log` 写在 EXE 同目录，UTF-8，单文件 100 KiB，不保留轮转副本。每条最多 16 KiB 正文，保留完整 UTF-8 字符，换行/NUL 被替换；多线程记录受锁保护，立即 flush，并保持调用方 Win32 last-error。

每条包含 ISO 8601 本地时间、时区、级别、PID/TID。文件不可写时保留调试器输出。服务问题通过 `service_issue` 发布，某个设备/语音失败不终止整个服务。

```powershell
Get-Service AxonkeyService
Get-Content "$env:ProgramData\Axonkey\service\AxonkeyService.log" -Encoding UTF8 -Tail 50 -Wait
```

`PCM ingress started` 表示输出入口已开启，不代表录音回采已验收。停止日志含 committed/forwarded/queued/silence；录音应用需选择 Quarbor Virtual Microphone。代码导航见 [架构说明](SERVICE_ARCHITECTURE.md)。
