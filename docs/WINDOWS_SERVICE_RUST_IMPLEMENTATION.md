# Windows 服务 Rust 实施记录

日期：2026-10-08。分支：`codex/windows-service-rust-plan`。基于 [迁移计划](WINDOWS_SERVICE_RUST_PLAN.md) 实施，源码位于 [windows/service/rust](../windows/service/rust/README.md)。

Rust 服务已成为默认构建实现。按用户后续“测试编译如果没问题就将原来的 c 代码删除”的指示，复跑通过后删除了旧 C++ 服务、CMake、旧测试 adapter 和仅供该服务使用的 C/nanopb 编解码器；共享 `.proto`、驱动和公共头文件保留。

**实机验收仍待完成**：本轮未替换已安装服务、未修改真实设备 LowerFilters，未执行录音回采或 8 小时资源测量。默认入口切换属于用户授权的源码迁移，不代表全部 P0–P5 实机验收已通过。

## 1. 已实现的功能

| 模块 | 实现及兼容范围 |
| --- | --- |
| `src/win/scm.rs`、`src/service.rs` | SCM 分发、状态/退出码、STOP/SHUTDOWN、设备通知、2 秒重扫、设备开关、自动启动设置、按设备隔离工作线程；RPC 成功绑定后才启动设备处理 |
| `src/win/device_setup.rs` | HID Keyboard/RC003 选择、父设备实例解析、过滤器服务校验、类级过滤器检查、LowerFilters 保留其他项和去重、写入回读、在线设备重启及离线解除挂载 |
| `src/win/hid.rs` | 独占端点、身份/版本/报告长度校验、先转发再拦截、完整 HID 报告、CancelIoEx 后等待完成、开关回滚、断开空报告 reset |
| `src/rpc.rs` | 全部 protobuf 方法和四类订阅；同一客户端的响应/事件有序；32 个活动客户端、每客户端 256 帧/4 MiB、1 MiB 帧上限、2 秒写超时、拒绝远程客户端 |
| `src/config.rs`、`src/logging.rs` | 64 位 HKLM 配置、损坏值修复、有符号增益、仅存储的 RemapConfig；UTF-8 日志、100 KiB 文件上限、16 KiB 消息截断、多线程完整行、错误码保持 |
| `src/win/gatt.rs` | MTA 线程、ATVV 服务/特征发现、AUDIO/CTL 通知、能力请求、命令发送、FIFO 队列、电量与名称、失败重连、取消与迟到结果回收 |
| `src/audio/` | 原协议会话状态机、被拒绝会话禁止中途抢占、ADPCM 高半字节优先解码、平滑、16→48 kHz 插值、尾音、增益/饱和与电平 |
| `src/win/microphone.rs`、`native/boundary.c` | 独占 MAP_RING→RESET→START、格式及状态校验、每次最多 960 字节、回绕复制、SEH/内存屏障、Generation/WritePosition COMMIT、250 ms 背压/排空、取消和关闭 |

`GetServiceInfo` 保持名称 `AxonkeyService`、版本 `0.3.1`、协议 `axonkey.service.v1` 和管道 `\\.\pipe\AxonkeyService.v1`。应用版本仍独立管理。

语音聚合状态按设备实例 ID 的稳定排序，优先 active，再 connected，最后第一个状态。GetDevices 优先连接内元数据，回退查询最多每 30 秒一次，避免每次查询重复发现 GATT。

## 2. 资源与并发设计

RPC 使用一个 current-thread Tokio runtime，连接采用受限任务集；完成任务及时回收。出站帧只编码一次并共享只读字节，队列预算包括正在发送的帧。满队列、断连或写超时关闭整个连接，释放帧预算和客户端登记，不继续发送半帧后续数据。

每个 HID 端点有一个读取线程。IOCTL 期间缓冲区、OVERLAPPED 和事件保持有效，pending 路径不分配临时等待数组；停止后先取消并等待内核完成，再释放内存、解除拦截/转发和关闭句柄。

每个 GATT 连接有一个 MTA 工作线程和一个本地 runtime。回调只通过弱引用入队，不拥有连接、服务或工作线程；队列同时限制 1024 个事件和 65536 字节，溢出结束连接并报告问题。订阅 token、GattSession、GattDeviceService、BluetoothLEDevice、DataReader/DataWriter 均由专属 guard 回收；结果集合中未使用的服务也关闭。

WinRT 超时/取消请求 Cancel 后，继续等待**同一个 Future**完成，不重复设置 Completed handler，不为重试遗留后台操作；迟到创建的资源会关闭。如果系统驱动永不完成取消，工作线程可能仍等待，服务清理持续上报 STOP_PENDING。这是尚需真机验证的系统边界，不通过遗弃操作来伪造快速停止。

正常服务关闭显式广播取消并 join，完成设备关闭后才解除过滤器、停止 RPC 和注销通知。guard 的 Drop 是错误/展开路径的兜底；线程 owner 的兜底仍会 join，以避免遗留工作线程。不能把该兜底视为固定时间内必定完成的析构。

安全 Rust 所有权、RAII、弱引用和有界队列降低泄漏风险；它们不能代替真实 WinRT/驱动对象和进程资源的长期测量。普通 Rust 堆分配耗尽仍可能终止进程，没有宣称全面恢复 OOM。

## 3. 已执行的验证

本机环境：Windows x64，rustc 1.98.0，MSVC 14.51。`Cargo.lock` 和子目录 `rust-toolchain.toml` 已加入工作区；构建脚本从仓库根显式核对工具链并传入目标和静态 CRT 参数，不依赖子目录配置被 Cargo 自动发现。

```powershell
npm run test:windows-service
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --lib service_rpc
```

| 验证 | 结果及证明范围 |
| --- | --- |
| rustfmt / Clippy `-D warnings` | 通过；删除前后均检查全部测试目标 |
| Rust Debug 测试 | 23 个单元/管道测试 + 3 个固定 C++ 基线回归测试全部通过 |
| Rust Release 测试 | 同样 26 个测试全部通过 |
| 音频对照 | 不同帧长/分包、同步、reset、flush 与实际 C++ 输出逐样本一致；全部 65536 个 PCM16 值在 -30～30 dB 及无效增益回退下逐样本一致 |
| 状态机对照 | 新旧协议、首包、拒绝开启、reset/push 失败、重复请求、停止后迟到音频、关闭命令、异常控制序列及电平与实际 C++ 一致 |
| 真命名管道 | 1000 次重连后登记回到零；32 个活动连接上限；全部订阅组合、ACK 顺序、损坏消息、半帧断连、慢读者超时与健康客户端隔离 |
| 资源/异常 | 队列预算归零；1000 次队列生命周期无回调所有权环；WinRT advisory Cancel/超时后迟到结果清理；麦克风 MAP/RESET/START/QUERY/copy/COMMIT/STOP 故障、非法驱动状态拒绝、回绕/背压/取消 |
| C/SEH 与 ABI | 实际公共头文件尺寸、对齐、关键 offset、IOCTL 和版本检查通过；合法复制及故意空指针目标复制的访问异常捕获通过 |
| 配置与日志 | 隔离 HKCU 中负增益、错误类型/长度和默认修复；UTF-8、日志上限、追加、并发完整行、不可写目标和 Win32 last-error 保持 |
| 业务 handler | 查询、畸形载荷、增益截断、开关/自动启动/设备查询路由、失败响应和关闭中拒绝请求；不调用实际 SCM 配置写入 |
| 默认构建 | 不依赖旧 C++ 源码生成 Release EXE/PDB；EXE `--check` 通过；x64 PE 导入表未出现额外 VC/MinGW 运行库 DLL |
| 已有桌面 RPC 测试 | 10 通过、1 个真实服务探测项忽略；原有 6 项非本次代码的 dead-code 警告仍在 |

删除前使用原 C++ 实现和临时 adapter 做逐样本/逐状态比较，并捕获 107 组 SHA-256 固定输出摘要；删除后保留原输入序列和独立基线，测试无需再编译 C++。基线来源和编码说明见 [fixtures/README](../windows/service/rust/tests/fixtures/README.md)。摘要不会由被测 Rust 实现自动刷新。

产物：`windows/service/dist/AxonkeyService.exe` 及匹配 PDB，同时保留 `.build/service-rust/dist` 副本。EXE 约 1.20 MiB；以实际文件为准。

CI 已新增 `.github/workflows/windows-service-rust.yml`，在 Windows 上安装固定工具链，运行上述服务验证并上传 EXE/PDB。**本轮验证在本机执行，尚无远端 CI 运行结果**；干净机器构建也是下一阶段的验收项。

## 4. 构建与试运行入口

默认构建和测试入口均使用 Rust：

```powershell
npm run build:windows-service
npm run test:windows-service
```

无需设置 AXONKEY_SERVICE_IMPLEMENTATION；旧值 cpp 会明确报错。构建复制到 `windows/service/dist` 并保留源构建时间，既有 Tauri 资源和服务管理脚本继续消费此路径；不自动安装、启动或替换 `%ProgramData%` 下的服务。

全仓库引用核对确认旧 C/nanopb 封装仅被已删除的服务 CMake 使用；桌面端与 Rust 服务的 build.rs 都只读取保留的 `.proto`。删除了 19 个旧服务源码/头文件/CMake/测试文件和 6 个旧编解码器文件；它们没有用户未提交的源码修改，均可从 Git 基线 `795ecd98e867ca0403ccce404608226f259022c7` 恢复。恢复旧实现应在独立 checkout 构建该基线，当前构建入口不再提供 cpp 选择。

## 5. 尚未完成的阶段验收

1. **P0/P2 互通补验**：保存真实 HID/GATT 样本；补当前桌面端与新服务的端到端协议测试，特别是负增益、optional 电量为 0/absent、未知字段和大请求。桌面端保留历史 nanopb wire fixture；普通测试不等同于已安装服务验收。
2. **P3 实机**：LocalSystem 服务与普通用户桌面互通；SCM 启停、禁用启动、挂载/解除、热插拔、pending HID 取消和按键 reset；持久化失败后的真实状态语义；实际驱动错误与重启要求。
3. **P4 实机**：RC003 GATT 首包/尾音、断连重连、独占抢占拒绝、录音回采、映射 generation 改变；还需补接实际 WASAPI 输出回采工具。多台同型号设备的唯一映射仍沿用旧版 VID/PID 匹配限制。
4. **P5 长期与发布**：100 次真实设备/SCM 循环、8 小时资源曲线、与独立构建的历史版本比较 CPU/内存/延迟、Win10/Win11 干净机器运行、实际安装升级与回滚。默认实现已按本次授权切换，这些剩余验收仍需记录真实结果。

兼容性差异按计划保留：RPC 绑定失败阻止设备启动；远程管道连接被拒绝；客户端数和 GATT 队列有上限；元数据回退有缓存；语音聚合选择顺序稳定。`driver_mounted` 仍遵循原有目标列表语义，不能据此推断每次挂载已成功。
