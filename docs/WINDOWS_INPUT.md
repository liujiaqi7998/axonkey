# Windows 输入

Windows 版通过 Quarbor HID 过滤驱动和 AxonkeyService 接入 RC003。服务按
`VID_2717`/`PID_32B8` 识别目标设备，读取完整的原始 HID report，并在驱动层屏蔽
该设备送往 Windows 的原始输入。桌面端不再加载第三方键盘过滤库，也不直接读取
键盘类设备。

## 输入链路

```text
RC003 HID report
  -> QuarborHIDFilterDriver
  -> AxonkeyService Endpoint
  -> \\.\pipe\AxonkeyService.v1 (KeyboardEvent)
  -> Windows report parser (HID usage set)
  -> click/double-click/long-press state machine
  -> SendInput behavior output
```

AxonkeyService 在端点上开启数据转发和输入屏蔽。每个 report 通过命名管道的
`KeyboardEvent` 事件发送给桌面端：事件外层 `Event.type` 为 `keyboard`，其
`Event.payload` 是 `KeyboardEvent` protobuf，包含 `device_instance_id`、原始
`report` 字节和时间戳；endpoint 停止时 `report` 为空，表示该设备的 reset。管道帧格式是小端 `uint32` 长度后跟 protobuf 数据；连接、
订阅和事件顺序见 [服务架构说明](../windows/service/SERVICE_ARCHITECTURE.md)。

桌面端订阅 `SubscribeRequest.keyboard=true` 后解析 report，不执行扫描码查找，
也不把收到的 report 回送设备。RC003 的键盘 report 使用 Report ID 1 和小端
16-bit HID usage 槽位；解析器同时兼容驱动协议定义的无编号 Report ID 0，按当前
report 的按下集合计算按下/释放边沿，未知 usage 会安全忽略。服务已经完成 RC003
设备发现、挂载和原始 HID 报告转发，前置的按键接收、硬件槽位探测、过滤器设置和
同设备发送逻辑不再属于桌面端。HID
错误/滚键标记 `0x01`–`0x03` 会使当前 report 被丢弃并释放桌面端输出。

以下额外 usage 由当前驱动直接转发并可进入相同的行为状态机：

| HID usage page 0x07 | RC003 按键 |
| --- | --- |
| `0xF1` | 返回 |
| `0x80` | 音量加 |
| `0x81` | 音量减 |

单击、双击、长按和行为序列仍按现有设置执行。模拟短按保持 50 毫秒；长按在持续
按住 600 毫秒后触发，随后按固定间隔重复，释放时清理所有仍按下的输出。

## 服务和驱动

首次使用时安装 Quarbor HID 过滤驱动、虚拟声卡驱动和 AxonkeyService。桌面端可在
设置页管理服务；开发环境也可使用：

```powershell
cmake -S windows/service -B .build/service
cmake --build .build/service --config Release
powershell -ExecutionPolicy Bypass -File .\scripts\manage-windows-service.ps1 -Action Install -ServiceExecutable .\windows\service\dist\AxonkeyService.exe
powershell -ExecutionPolicy Bypass -File .\scripts\manage-windows-service.ps1 -Action Start
```

服务未运行、管道不可用或设备尚未挂载时，输入服务会报告连接错误并自动重试。确认
`AxonkeyService` 正在运行、`GetServiceInfo` 返回协议 `axonkey.service.v1`，并且
`GetDevices` 中目标设备的 `driver_mounted`、`input_blocked` 和
`data_forward_enabled` 均为 `true`。

修改映射只替换桌面端的设置快照，不需要重启应用或重新安装驱动。未启用自定义行为
时，桌面端仍消费并解析键盘事件，再通过 `SendInput` 重放对应的系统虚拟键，避免
服务屏蔽原始输入后按键失效；配置了自定义行为时才交给 `execute_behaviors` 执行。
服务仍可保持运行以提供设备状态和语音通道。

## 诊断

如需查看 Windows HID 到 i8042 扫描码的系统转换，可运行只读脚本：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-rc003-hid-usages.ps1
```

该脚本只调用 Windows `HidP_TranslateUsagesToI8042ScanCodes` 并打印转换结果，
不会安装驱动、修改过滤器或注入按键。它不能替代 AxonkeyService 的真实 report
事件；排查映射时应优先查看服务日志和桌面端的 `keyboard` 事件日志。
