# RC003 Windows 按键码 Demo

用于观察 Windows 为 RC003 生成的输入事件。Demo 只记录并原样放行输入，
不安装新驱动、不注入进程，也不执行按键映射。

## Windows / Interception 模式

独立于 Axonkey 主程序。采集时会尝试使用已有 Interception 驱动读取 RC003 原始扫描码，
同时观察 Windows 输入事件。不安装新驱动，不需要管理员权限。
没有可用 Interception 驱动时仍可观察 Windows 事件，但 Demo 无法绕过已有驱动的故障。

## 启动

在仓库根目录运行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\keycode-demo.ps1
```

脚本使用 Windows 自带的 .NET Framework 4.x C# 编译器，生成并打开
`.build\keycode-demo\<构建编号>\Axonkey-KeycodeDemo.exe`，并校验、复制项目已有的 `interception.dll`。
每次源码修改使用独立目录，以免正在运行的旧窗口阻止编译新版；脚本会打印实际 EXE 路径。
之后可直接双击该 EXE；移动到其他目录时应同时携带 DLL。
只支持 Windows x64，不需要安装 Node、Rust 或 .NET SDK。

## 采集

1. 从系统托盘退出 Axonkey，避免看到映射后的事件。保持遥控器已连接并唤醒。
2. Demo 初始为暂停状态。先点击“全部按键 / 对照”，观察驱动状态，然后依次按遥控器确认键、方向键和普通键盘 A。
3. 依次点击“1. 返回”“2. 音量加”“3. 音量减”，各按对应实体键 2–3 次，包含按下和松开；也可以额外长按一次观察重复事件。
4. 点击“暂停采集”，再点击“复制日志”，或者用“日志目录”找到文件。

保持 Demo 在前台，以便窗口接收到 `WM_APPCOMMAND`。Raw Input 和键盘钩子在
采集期间也会观察其他键盘，因此测试时尽量只按遥控器。Demo 会放行按键，音量等原功能
仍会执行。Interception 仅为匹配 RC003 的槽位设置过滤，接收的每个原始事件立即原样转发，
再记录日志；暂停或关闭时清除过滤并释放 context。不要同时运行其他 Interception 映射工具。
切换、暂停和复制不会清除已经采集的数据；重新打开 Demo 会新建一个日志。

完整日志自动写入：

```text
%LOCALAPPDATA%\Axonkey\diagnostics\keycodes\时间-进程号.log
```

屏幕只保留最近一段日志，文件保留本次完整记录。关闭窗口后结束监听。

## 看哪些字段

| 日志类型 | 关键字段 | 设备来源 |
| --- | --- | --- |
| `INTERCEPTION` | `scan` 原始扫描码、`E0/E1`、`state`、`DOWN/UP`、`forwarded` 原样转发结果 | 只记录硬件 ID 匹配 RC003 的驱动槽位 |
| `RAW_KEYBOARD` | `vk` 虚拟键码、`scan` 扫描码、`E0/E1` 扩展标志、`DOWN/UP` | 根据设备路径识别 RC003 / OTHER / UNKNOWN |
| `RAW_HID` | `hex` 完整报告、`buttons=[UsagePage:Usage]` 当前按下的 HID 按钮、`tlc` 顶层集合 | 同时检查设备路径和可用的 VID/PID |
| `LL_KEYBOARD` | `vk`、`scan`、`E0`、`DOWN/UP`、`injected` 注入标志 | 此接口不提供物理设备，始终是 UNKNOWN |
| `WINDOW_KEY` | 当前窗口收到的 `vk`、`scan`、`E0`、`DOWN/UP`、重复次数 | 不提供物理设备，始终是 UNKNOWN；用作全局钩子的对照 |
| `APP_COMMAND` | `command` 十进制/十六进制命令码及名称、`origin` KEY/MOUSE/OEM | 只有来源类别，具体设备始终是 UNKNOWN |
| `DEVICE` / `DEVICE_CHANGE` / `INTERCEPTION_DEVICE` | 当前输入设备、路径、连接变化、驱动槽位 | 用于对照实际设备 |
| `INTERCEPTION_STATUS` | 连接状态、错误、过滤解除 | 明确区分驱动已接入和 Windows 仅枚举到设备 |
| `MARK` | 手动选择的采集阶段 | 用于对照正在测试哪个实体键 |

按键码按 Windows 实际返回值记录，不把零扫描码换成推算值，也不预先假定这三个实体键
应该对应什么码。键盘 `scan + E0/E1`、`vk`、HID usage 和媒体命令码属于不同编码体系，
兼容时必须保留事件类型。同一次按键可能在多条通道留下记录，不代表按了多次。

只有匹配 RC003 硬件标识时才标为 `device=RC003`。阶段名中的“返回”等是人工标签，
**不能把该阶段内的 UNKNOWN 事件据此认定为 RC003**。键盘设备的单独 `vid/pid` 字段可能为零，
此时识别依据是完整 `path`。`tick` 可辅助对照事件时间，但也不是设备归属证据。

HID `buttons=[]` 表示当前报告没有处于 ON 的按钮；包含数值字段的报告仍需要结合 `hex`
分析。只有 Raw Input 对 Windows 可见的 HID 顶层集合才能采集；如果三种按键都没有记录，
可在同一阶段按“确认键”作对照。所有键都没有输入时，先处理设备连接或已有驱动故障。

如果只有 `LL_KEYBOARD` / `APP_COMMAND`，仍可得到系统识别的码，但不足以安全区分普通键盘
与 RC003。这也是后续判断能否做设备级兼容的重要结果。

如果有 `INTERCEPTION` 但没有 `RAW_KEYBOARD`，说明驱动已收到扫描码，Windows 下游没有
对应的 Raw Input 键盘事件。若普通键盘有记录、确认键和方向键有记录，而这三个键始终没有，
应继续检查这些按键是否经过另一条 HID/蓝牙链路；不能仅凭没有日志就断定需要重启。

“等待 RC003 扫描码”表示已经提交过滤请求，直到收到事件才显示“已收到 RC003 扫描码”。
`filter_readback` 原样记录 API 读回值；该 API 不提供错误码，不能只凭读回零就停止采集。

## 验证

```powershell
# 只编译
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\keycode-demo.ps1 -BuildOnly

# 解析与原生接口自检，不向系统注入按键
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\keycode-demo.ps1 -SelfTest
```

自检覆盖 x64 结构布局、设备身份、键盘扫描码和扩展标志、多个 HID 报告拆分、截断数据拒绝、
媒体命令位掩码、暂停采集、实际原生注册/钩子生命周期和日志落盘，并生成窗口预览。
Interception 使用合成驱动测试普通键盘隔离、零/未知扫描码无损转发、转发顺序及失败后的过滤清理。
自检数据保存在对应构建目录中的 `self-test*`，明确标记为合成事件；不代表真实 RC003 按键码。

实现参考 Microsoft 的 [Raw Input 注册说明](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerrawinputdevices)、
[RAWKEYBOARD](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawkeyboard)、
[HidP_GetUsagesEx](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/hidpi/nf-hidpi-hidp_getusagesex) 和
[WM_APPCOMMAND](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-appcommand)。
