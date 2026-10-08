# AxonkeyService 架构

当前实现为 [Rust crate](rust/README.md)。服务用户态代码使用 Rust，驱动保持现有 C/C++ 实现；唯一编入服务的 C 文件是共享映射保护和 ABI 验证边界。历史 C++ 服务保存在 Git 基线 `795ecd98e867ca0403ccce404608226f259022c7`。

## 模块与所有权

| 入口 | 职责 |
| --- | --- |
| [main.rs](rust/src/main.rs) / [scm.rs](rust/src/win/scm.rs) | 无硬件 --check、日志初始化、SCM 回调及状态更新 |
| [service.rs](rust/src/service.rs) | RPC 业务 handler、设备协调、运行状态和关闭顺序 |
| [device_setup.rs](rust/src/win/device_setup.rs) | RC003 枚举、LowerFilters 校验/修改/回读与设备重启 |
| [hid.rs](rust/src/win/hid.rs) | 独占 HID 端点、报告读取、取消和 reset 发布 |
| [rpc.rs](rust/src/rpc.rs) | 管道连接、protobuf 帧、请求/事件队列与订阅 |
| [gatt.rs](rust/src/win/gatt.rs) | MTA、WinRT 发现、特征订阅、事件队列、电量/名称与清理 |
| [session.rs](rust/src/audio/session.rs) | 语音协议、独占失败、开启/结束/迟到包 |
| [decoder.rs](rust/src/audio/decoder.rs) / [gain.rs](rust/src/audio/gain.rs) | ADPCM、平滑、重采样、尾音、增益和电平 |
| [microphone.rs](rust/src/win/microphone.rs) | 驱动映射、状态校验、环形缓冲复制/提交/排空 |
| [boundary.c](rust/native/boundary.c) / [abi.rs](rust/src/win/abi.rs) | SEH、内存屏障、整数 C 布局与公共头文件验证 |
| [config.rs](rust/src/config.rs) / [logging.rs](rust/src/logging.rs) | 注册表与同步有界日志 |

配置和设备协调由单一协调线程处理；运行开关/增益使用原子值，状态快照使用短持有时间的 Mutex。阻塞的驱动工作与 GATT 操作不在 RPC runtime 执行。

## 启停

1. SCM 注册控制回调，发布 START_PENDING，读取配置并验证 ABI。
2. 同步确认生产管道首实例创建成功，然后注册通知、启动协调线程并发布 RUNNING。
3. Enabled=0 时保留 RPC，设备协调直接返回；启用时按事件或 2 秒周期扫描。
4. STOP/SHUTDOWN 回调只发送取消/唤醒，不等待线程或执行设备 I/O。
5. 协调线程先广播 HID/GATT 停止，再回收工作线程，之后解除自身过滤器项。
6. 清理期间持续更新 STOP_PENDING/checkpoint；随后停止 RPC、注销通知、flush 日志并发布 STOPPED。

错误路径使用 RAII 兜底。所有内核句柄、SetupAPI 设备集、SCM 句柄和通知注册都使用各自正确的释放函数。未完成 I/O 必须等到内核完成后才释放其缓冲区，不能为了快速退出遗弃操作。

## 键盘链路

RC003 → HID 过滤驱动 → HID 读取线程 → RPC Hub → desktop service_rpc → Windows report parser → 输入行为状态机。

开启端点时先验证 QUERY_IDENTITY，再打开转发和拦截；读取线程每次只持有一个待完成 HID_DATA IOCTL。停止时 CancelIoEx 后 GetOverlappedResult 排空，清除拦截/转发，发布空报告，关闭句柄。报告保持 Report ID 和完整原始字节。

设备列表以 HID Keyboard 和 RC003 VID/PID 为范围。挂载保存其他 LowerFilters；读回不一致返回可重试错误，清理也包括离线已配置实例。GetDevices 的 driver_mounted 暂沿用旧版目标列表语义，不等同于该次挂载操作成功。

## RPC 并发和边界

一个 current-thread Tokio runtime 管理 accept 和连接任务集。每个连接内并发读取请求和串行发送出站队列，完成任务及时回收；业务变更经容量 64 的通道交给协调线程，查询快照直接返回。

| 限制 | 行为 |
| --- | --- |
| 32 个活动客户端 | 暂停接纳更多活动任务 |
| 1 MiB 帧 | 超限拒绝读取/发送 |
| 每客户端 256 帧 / 4 MiB | 超限断开该客户端，预算包括正在写的帧 |
| 每帧写 2 秒 | 超时关闭连接，不继续发送半帧后的数据 |
| 远程管道客户端 | 拒绝 |

Hub 每个事件编码一次并用 Arc 共享只读数据。Subscribe ACK 先入队，再更新 topic mask。键盘报告/reset 不能静默丢弃后继续使用同一连接。客户端 Drop 注销登记并释放剩余帧预算。

## 语音链路

GATT AUDIO/CTL 回调 → 有界 FIFO → 会话状态机 → ADPCM 解码 → 平滑/48 kHz 插值 → 增益/电平 → 虚拟麦克风 ring。

每个连接拥有一个 MTA 工作线程和本地 runtime。GATT 回调持有队列弱引用，避免回调与连接形成所有权环。队列限制 1024 个事件和 65536 字节；任何上限溢出都会结束连接并报告问题，不继续解码不完整 ADPCM 流。

WinRT 超时/取消后等待同一个 Future 完成，不重复注册 Completed；迟到资源主动关闭。订阅 guard 移除事件 token，IClosable guard 关闭 Session、Service、Device 和流读写器。系统驱动若不完成取消，工作线程可能继续等待，SCM 维持 STOP_PENDING。

状态机沿用 0B 能力、08 请求、04 开始、0A 同步、00 结束、0D 关闭。麦克风独占失败后丢弃整段会话，只在下一次开始尝试；结束补尾音，迟到包丢弃。

麦克风输出校验 48 kHz / mono / PCM16、映射尺寸、generation、位置、可用/空闲字节。每次最多 960 字节，回绕分两段复制，在 C/SEH 边界中完成内存屏障后 COMMIT。满缓冲和正常结束排空上限 250 ms；异常或服务停止跳过排空。

电量每 30 秒尽力刷新，GetDevices 优先连接快照，必要时使用有缓存的回退查询。多台同型号设备仍沿用旧 VID/PID 关联限制。聚合语音状态按实例 ID 排序，优先 active，再 connected，最后第一个状态。

## 验证与定位

`npm run test:windows-service` 执行格式、静态检查、Debug/Release 测试与产物自检。音频测试使用 [固定 C++ 基线](rust/tests/fixtures/README.md)，其余包括真实命名管道 1000 次重连、慢客户端、WinRT 延迟取消、驱动故障注入、ABI、日志和注册表。

| 现象 | 优先检查 |
| --- | --- |
| 服务无法启动 | scm.rs 的退出码、rpc.rs 首实例绑定、EXE 同目录日志 |
| 按键无响应或未恢复 | device_setup.rs 挂载回读、hid.rs 身份及 pending I/O、空报告 reset |
| 蓝牙发现或语音断连 | gatt.rs 服务/特征查询、权限、事件队列和取消完成 |
| 音频失真或无输出 | session.rs 同步状态、decoder.rs 帧长、microphone.rs QUERY_STATE/COMMIT |
| 桌面 RPC 无响应 | 客户端数量、出站预算/超时、协调线程是否正在处理设备变更 |

编译与自动测试不替代真实服务安装、热插拔、录音回采及长时间资源测量；当前记录见 [实施进度](../../docs/WINDOWS_SERVICE_RUST_IMPLEMENTATION.md)。
