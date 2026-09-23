# AxonkeyService

这是管理 `QuarborHIDFilterDriver` 驱动的 Windows 服务，服务名称和可执行文件分别为
`AxonkeyService` 和 `AxonkeyService.exe`。服务启动后即执行设备管理。

服务启动后枚举 HID Keyboard，匹配 `VID_2717`/`VID&012717` 与
`PID_32B8`/`PID&32B8` 的设备，依次挂载过滤器、开启数据转发，再通过
`IOCTL_QUARBOR_SET_INPUT_BLOCK` 屏蔽送往 Windows 的原始输入。设备变化会触发重新枚举。
原始 HID 报告读取循环已接入，`Endpoint::OnHidReport` 将完整报告发布为 RPC `KeyboardEvent`；
桌面端负责按 usage 集合解析和执行映射，并在需要保留原按键时通过 Windows `SendInput`
重放对应虚拟键。服务本身不会把报告重新注入物理 RC003 设备；启用屏蔽后，Windows
不会再收到该设备未经桌面端处理的原始按键输入。Endpoint 读线程停止时会发布一个
空 report，作为桌面端释放按键状态的 reset 通知。

`ServiceConfig` 在服务启动时读取 64 位注册表配置：

- 路径：`HKLM\SOFTWARE\Axonkey\Service`。
- `Enabled`（`REG_DWORD`）：服务设备处理总开关，默认 `1`（开启）。服务初始化时读取；关闭时不枚举/挂载设备，并清理已注册设备和语音线程。
- `AudioGainDb`（`REG_DWORD`）：音频增益，单位 dB，默认 `2`；按有符号 32 位整数读取，允许 `-30`～`30`。负数以补码保存，兼容原有正数配置。
- `RemapConfig`（`REG_BINARY`）：`QUARBOR_REMAP_CONFIG` 的完整二进制结构，默认空表。

缺失或无效的增益值会恢复并保存为 `2`。改键表仍只保留存储定义，不下发驱动。

总开关可以直接修改注册表，也可以通过本地 protobuf RPC 动态控制。`SetServiceStatus`
设置为 `true` 会立即执行一次设备协调并持久化；设置为 `false` 会取消设备注册、停止语音线程并持久化。
查询使用 `GetServiceStatus`：

```powershell
Set-ItemProperty -Path 'HKLM:\SOFTWARE\Axonkey\Service' -Name Enabled -Type DWord -Value 0
```

增益在蓝牙音频完成解码及 48 kHz 重采样之后、写入虚拟麦克风驱动之前应用，
包括结束时的尾音。幅度倍率为 `10^(dB/20)`，默认 2 dB 约为 1.259 倍；0 dB 不改变
音频。超过 PCM16 范围的样本会饱和截断，避免整数溢出；较高增益可能产生削波。

负值降低音量，正值提高音量。在管理员 PowerShell 中调整，例如设置为 -6 dB，重启服务生效：

```powershell
New-ItemProperty -Path 'HKLM:\SOFTWARE\Axonkey\Service' -Name AudioGainDb -PropertyType DWord -Value (-6) -Force
Restart-Service AxonkeyService
```

## RC003 语音接收

每个已初始化的 RC003 设备都会创建一个独立的语音接收线程。线程使用
Windows C++/WinRT GATT API 查找 ATVV 服务，并订阅 AUDIO、CTL 特征：

- ATVV：`AB5E0001-5A21-4F05-BC7D-AF01F617B664`
- TX：`AB5E0002-5A21-4F05-BC7D-AF01F617B664`
- AUDIO：`AB5E0003-5A21-4F05-BC7D-AF01F617B664`
- CTL：`AB5E0004-5A21-4F05-BC7D-AF01F617B664`

连接完成后先向 TX 写入 `0A 01 00 00 03 03`。CTL 的 `08` 事件会尝试独占
`QuarborVirtualMicrophoneDriver`，成功后按协议版本回复 `0C 00`（旧版协议附带
codec 字节）；遥控器主动发送 `04` 时只开启本地音频接收，不重复发送 `0C 00`。
CTL 的 `00 02` 会停止音频，停止后的迟到包会被丢弃，直到新的 `08`/`04` 开始事件。AUDIO 中的 RC003
16 kHz 单声道 ADPCM 按协议解码、平滑，并线性插值转换为 48 kHz、16 位、单声道
PCM，再写入虚拟麦克风。虚拟麦克风已被其他语音线程占用时，后续线程的音频直接
丢弃，先取得句柄的线程保持独占。被拒绝的会话不会因其他线程释放句柄而中途抢占，
只有下一次开始事件才重新尝试。连接销毁前会按协议版本向 TX 发送 `0D` 关闭
当前会话。

线程销毁时会停止并关闭虚拟麦克风句柄、解除 GATT 回调和释放蓝牙对象。服务停止
会先销毁所有语音线程，再解除 HID 过滤器挂载。

语音线程依赖 `windows/driver/shared/include/QuarborVirtualMicrophone.h` 定义的
`MAP_RING/START/STOP/RESET/QUERY_STATE/COMMIT` 控制接口；安装的虚拟麦克风驱动
需要提供该用户态控制接口。

音频输出参考 `AxonkeyVirtualMicrophoneDriverTest/src/AudioTest.cpp` 的 `InjectTone`：
独占打开 `\\.\QuarborVirtualMicrophone`，依次执行 `MAP_RING → RESET → START`，
通过 `QUERY_STATE` 获取空闲空间，将 PCM 拷入映射的环形缓冲区，执行内存屏障后
携带当前 Generation 和 WritePosition 提交 `COMMIT`。每次最多提交 960 字节。
目标录音设备是 **麦克风 (Quarbor Virtual Microphone)**；录音应用需要选择该设备。

- `VoiceReceiver.cpp`：GATT 回调只入队，由单个工作线程按入队顺序处理 AUDIO/CTL。
- `VoiceAudioSession.cpp`、`AdpcmDecoder.h`：会话独占、协议处理、ADPCM 解码与重采样。
- `VirtualMicrophoneSink.cpp`：驱动映射、格式校验、回绕拷贝、提交和释放。

首次 AUDIO 包会在成功打开驱动后继续解码；缓冲区满时以 1 ms 间隔等待，连续
250 ms 无空闲空间则记录故障并结束本次输出。正常语音结束时补齐重采样尾音，
最多等待 250 ms 排空已提交的音频，再执行会清空缓冲区的 `STOP`。服务停止时可取消等待。
参考程序的 `Sleep(9)` 用于给合成测试音限速；实际蓝牙音频已有输入节奏，因此服务不额外延时。

构建需要支持 C++20 的编译器和包含 C++/WinRT 头文件的 Windows SDK。

## 本地 protobuf RPC

服务启动后先创建 `\\.\pipe\AxonkeyService.v1`，再启动设备扫描线程。管道帧由 4
字节小端长度和 `protobuf/axonkey_service.proto` 定义的 proto3 消息组成；编解码由
[nanopb](https://github.com/nanopb/nanopb) 0.4.9（CMake `FetchContent` 钉版本）
完成，C++ 封装位于 `protobuf/axonkey_rpc.*`，生成代码在 `protobuf/generated/`。
桌面端可通过 `GetServiceInfo`（其中包含当前 `audio_gain_db`）、
`GetServiceStatus`、`SetServiceStatus`、`SetAudioGain`、`GetDevices`、`GetVoiceStatus`、`GetAudioLevel` 查询或控制服务，
并通过 `Subscribe` 订阅 `keyboard`、`audio_level`、`voice_status` 事件。键盘报告来自
已挂载并拦截输入的 Quarbor 端点，音频电平来自增益处理后的 PCM 样本。
`GetDevices` 的每个 `Device` 还会尽力返回服务可读取的电量 `battery_level` 和描述名称
`description_name`；读取失败时电量字段不设置、描述名称为空，不影响设备列表响应。

每个 RPC 客户端都有独立的出站发送线程；请求响应和事件先进入有界队列，再由该线程
按顺序写入管道。单次写入超过 2 秒会被取消，队列超过 256 帧或 4 MiB 也会主动断开
客户端，因此不读取管道的用户态客户端不会阻塞服务线程或持续占用内存。
在 Visual Studio Developer PowerShell 中执行：

```powershell
cmake -S windows/service -B .build/service
cmake --build .build/service --config Release
```

安装 `windows/driver/QuarborHIDFilterDriver.inf` 驱动包后，以管理员身份注册服务：

```powershell
sc.exe create AxonkeyService binPath= "C:\path\AxonkeyService.exe" start= auto obj= LocalSystem
sc.exe start AxonkeyService
```

开发验证可使用 `script/Test-AxonkeyService.ps1`，它固定管理
`dist\AxonkeyService.exe`（CMake 默认输出目录）。不带参数进入交互菜单；
也可传 `-Action`。安装、启动、停止、重启和卸载会自动请求管理员权限；
状态查询不需要管理员权限。

```powershell
Set-ExecutionPolicy -Scope Process Bypass
cd windows\service\script
.\Test-AxonkeyService.ps1                  # 交互菜单
.\Test-AxonkeyService.ps1 -Action Status
.\Test-AxonkeyService.ps1 -Action Install
.\Test-AxonkeyService.ps1 -Action Start
.\Test-AxonkeyService.ps1 -Action Stop
.\Test-AxonkeyService.ps1 -Action Restart
.\Test-AxonkeyService.ps1 -Action Uninstall
```

`Start` 在服务还未安装时会先安装；`Install` 将 SCM `binPath` 指向当前
`dist\AxonkeyService.exe`，并设置为开机自动启动（LocalSystem）。
脚本不会安装 HID 或虚拟麦克风驱动，这些驱动需要先单独安装。

服务停止时会关闭端点句柄，并解除所有已保存的 `QuarborHIDFilterDriver` 设备挂载。

## 桌面应用中的服务管理

Windows 首次使用设置的“驱动安装”页包含后台服务状态，以及安装、卸载、启动、停止操作。
状态通过 Windows 服务管理器只读查询，页面每 3 秒和窗口重新获得焦点时刷新。
四项修改操作都通过 `ShellExecuteExW` 的 `runas` 请求管理员权限，等待操作完成后再读取实际状态。

应用使用 `scripts/manage-windows-service.ps1` 管理固定的 `AxonkeyService`。
安装会直接将运行环境中的 `windows\service\AxonkeyService.exe` 注册为 Windows 服务，
不会复制服务程序；服务以 LocalSystem 注册并设置开机自动启动，安装后可单独点击“启动”。
卸载先停止服务并删除服务注册，不会删除运行环境中的服务程序；日志和注册表配置保留。
操作错误记录在 `%ProgramData%\Axonkey\Logs\ServiceManagement.log`。

`npm run build:windows-service` 使用 Visual Studio C++ 工具链和 CMake/Ninja 构建服务，
产物位于 `windows/service/dist/AxonkeyService.exe`，使用静态 MSVC 运行库。
Tauri 的开发与打包入口会先自动执行此构建，发布资源包含服务程序和管理脚本。
直接运行 Cargo 测试前，需先执行一次该构建命令以准备资源。

## 运行日志与回归验证

使用 [spdlog](https://github.com/gabime/spdlog/tree/v1.15.3) 记录服务生命周期、设备连接、
配置回退和语音异常。CMake 首次配置需要 Git 和网络，拉取固定版本 1.15.3 的提交
`6fa36017cfd5731d617e1a934f0e5ea9c4445b13`，编译进服务，
无需额外部署日志 DLL。离线构建可通过 `FETCHCONTENT_SOURCE_DIR_SPDLOG` 指向该版本的源码目录。

UTF-8 日志 `AxonkeyService.log` 写在 **AxonkeyService.exe 所在目录**，不受服务工作目录影响，
同时保留调试器输出。服务账户需有该目录的写权限；日志初始化或写入失败只输出调试诊断，
不会抛出到业务代码或覆盖调用方的 Win32 错误码。

单文件上限为 **100 KiB（102400 字节）**：下一条记录会超限时，丢弃文件内全部旧日志再写入新记录，
不生成备份文件。重启时追加已有日志，已有文件超限则清空；单条超长消息截断并标注 `[truncated]`。
每条日志立即 flush，多线程写入受锁保护，不记录音频内容。

格式为 ISO 8601 本地时间（毫秒和时区）、级别、服务名、进程/线程 ID、消息，例如：

```text
2026-09-19T09:30:00.123+08:00 [info] [AxonkeyService] [pid=1234 tid=5678] AxonkeyService running
```

级别包括 `info`（启停和连接状态）、`warning`（配置回退、可恢复异常）、`error`（操作失败），
可用时附带 Win32/HRESULT 错误码。消息中的换行替换为空格，保持一条事件一行。

管理员 PowerShell 中启动服务并跟踪日志：

```powershell
Start-Service AxonkeyService
Get-Service AxonkeyService
Get-Content ".\windows\service\AxonkeyService.log" -Encoding UTF8 -Tail 50 -Wait
```

蓝牙 API 返回空服务时，语音线程会记录错误并由设备重扫重试，不再直接访问空对象。
日志中的“unavailable or access denied”表示 API 未返回 ATVV 服务，具体连接或访问原因
仍需结合设备状态检查；服务为 Running 不代表语音已经就绪。

构建默认包含无需硬件的回归测试，覆盖空 GATT 服务、清理异常、ADPCM 首包/分包/尾音、
会话独占、写入错误、环形缓冲区回绕、满缓冲区等待及取消；日志测试另外覆盖中文 EXE 路径、
UTF-8 和格式/级别、多线程完整记录、重启追加、超限丢弃、超长消息、不可写路径和错误码保持：

```powershell
ctest --test-dir .build/service-msvc -C Release --output-on-failure
```

MSVC 构建同时生成 `AxonkeyService.pdb`，排查崩溃时请保留与 EXE 同次构建的 PDB。

## 单独验证虚拟麦克风输出

构建也会生成 `VirtualMicrophoneProbe.exe`。它使用服务的实际写入模块注入两秒
1 kHz 测试音，再通过 WASAPI 从名称包含 `Quarbor Virtual Microphone` 的活动录音端点
回采并比对；不会使用默认或物理麦克风，也不依赖 RC003 蓝牙连接。该工具不自动加入 CTest。

关闭占用驱动注入接口的服务或测试工具后，在**管理员 PowerShell** 执行：

```powershell
& .\.build\service-msvc\VirtualMicrophoneProbe.exe
$LASTEXITCODE # 0 = 回采匹配通过；1 = 失败，查看控制台诊断
```

驱动控制入口仅允许 LocalSystem/管理员写入。普通权限运行时可能返回 `Win32=5`
（拒绝访问），不能据此认定驱动没有音频接口。`Win32=2` 通常表示未安装控制入口，
设备忙/共享冲突表示已有其他注入者。Probe 将诊断输出到控制台；实际服务仍写入上述日志文件。

服务日志中的 `PCM ingress started` 表示驱动注入入口已开启；停止时的 `committed` 是
成功提交字节数，`forwarded` 是驱动已转发字节数，`queued` 是尚未读取字节数。
有提交但未转发时，检查录音应用是否正在使用目标麦克风；如果一直只有 `ATVV discovery`
错误，则故障发生在蓝牙输入端，尚未进入 PCM 输出流程。
