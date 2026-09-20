# QuarborAxonkeyDriverInstaller 启动参数与集成技术文档

## 1. 适用范围与交付

安装器管理 HID 键盘过滤驱动和虚拟麦克风驱动，两者作为一次整体操作处理。支持 Windows x64，平台基线沿用仓库配置。

构建项目位于 `application/QuarborAxonkeyDriverInstaller/QuarborAxonkeyDriverInstaller.vcxproj`，可执行文件为 **QuarborAxonkeyDriverInstaller.exe**。输出目录采用项目名：

```text
artifacts/bin/x64/Release/QuarborAxonkeyDriverInstaller/QuarborAxonkeyDriverInstaller.exe
artifacts/bin/x64/Debug/QuarborAxonkeyDriverInstaller/QuarborAxonkeyDriverInstaller.exe
```

发布时使用 `QuarborAxonkeyDriverInstaller.exe`。驱动包名、服务名、设备 ID 和重启保护注册表键继续使用 Quarbor，保证与已安装驱动兼容；跨会话并发锁使用管理员控制的 `%ProgramFiles%\Axonkey\QuarborDriverInstaller.lock`。

需要安装的签名驱动包与 EXE 放在同一目录；路径按 EXE 位置解析，不依赖调用者工作目录：

```text
QuarborAxonkeyDriverInstaller.exe
QuarborHIDFilterDriver.inf
QuarborHIDFilterDriver.sys
QuarborHIDFilterDriver.cat
QuarborVirtualMicrophoneDriver.inf
QuarborVirtualMicrophoneDriver.sys
QuarborVirtualMicrophoneDriver.cat
```

卸载和查询不要求旁边存在源驱动包。安装会跳过已就绪的驱动，只校验需要安装的包；该接口不是强制版本升级接口。

安装器可以从桌面、下载目录及其他普通用户可写目录运行，不再检查 EXE 和驱动包源目录/文件的所有者及 ACL。安装、卸载仍需要管理员权限。Release 构建在开始更改驱动前，使用 Windows 内核驱动信任策略验证待安装包 CAT、INF、SYS 的签名及目录成员哈希；未签名、签名不受信任、文件被修改、只有普通代码签名或混用不同包时停止，返回失败码 `1`，`message` 给出本地化说明、文件路径和十六进制校验错误码。Windows 证书链吊销检查可能需要联网。Debug 不增加此预检，但两种配置都遵守 Windows 安装/内核签名策略；生产发布仍需经过 Microsoft Hardware Dev Center 硬件签名流程。

## 2. 参数与运行模式

| 命令 | 行为 |
| --- | --- |
| `QuarborAxonkeyDriverInstaller.exe` | 原有交互界面，检查状态，等待用户点击 |
| `QuarborAxonkeyDriverInstaller.exe --install` | 显示界面后自动开始安装，无需点击安装按钮 |
| `QuarborAxonkeyDriverInstaller.exe --uninstall` | 显示界面后自动开始卸载，无需点击卸载按钮 |
| `QuarborAxonkeyDriverInstaller.exe --install --silent` | 无窗口安装，执行完成后自动退出 |
| `QuarborAxonkeyDriverInstaller.exe --uninstall --silent` | 无窗口卸载，执行完成后自动退出 |
| `QuarborAxonkeyDriverInstaller.exe --status` | 无窗口只读查询，返回 JSON 后退出 |
| `QuarborAxonkeyDriverInstaller.exe --status --silent` | 与 `--status` 相同 |
| `QuarborAxonkeyDriverInstaller.exe --help` | 输出包含用法的 JSON，不请求提权 |

静默安装、静默卸载和状态查询均可追加 `--output "C:\path with spaces\result.json"`。路径支持 Unicode，相对路径按调用者工作目录解析。父目录必须已存在，输出文件必须是新文件，已有文件不会被覆盖；每次调用应使用独立结果文件。输出目标应是独立 JSON 文件，不要指向驱动包或其他业务文件。最终输出文件使用 `CREATE_NEW` 和 `FILE_FLAG_OPEN_REPARSE_POINT`，降低覆盖敏感文件和最终路径符号链接重定向的风险。

参数区分大小写，顺序任意，不支持 `/S`、`/install` 或 `--output=path`。动作只能指定一次；未知参数、重复参数、多个动作、单独 `--silent`、缺少输出路径，以及在界面模式使用 `--output`，均返回 `87`，且不弹窗、不执行驱动操作。`--help` 必须单独使用。

自动界面模式执行一次指定动作，保留窗口供用户查看结果；关闭窗口时返回操作退出码。切换语言不会重复执行。执行期间禁止安装、卸载、取消和关闭；完成后允许关闭，需要重启时继续禁止安装/卸载。

## 3. 权限与静默约定

EXE manifest 使用 `asInvoker`。无参数和自动界面模式在未提权时通过 `ShellExecuteExW(runas)` 请求管理员权限；原进程等待提权子进程并转发退出码，取消 UAC 返回 `1223`。

**静默安装/卸载必须由已提权管理员进程或 SYSTEM 调用。** 未提权时直接返回 `740`，不触发 UAC，不自行重启为管理员。状态查询与帮助不主动提权；查询所需权限不足时，返回部分状态及字段错误，查询退出码为 `1`。

静默路径不创建安装器窗口、消息框或控制台。通过 `SetupSetNonInteractiveMode(TRUE)` 禁止 SetupAPI 交互，麦克风驱动绑定额外设置 `INSTALLFLAG_NONINTERACTIVE`。驱动 API 始终传入重启结果指针，不由系统 API 弹出重启提示。签名或安装条件无法在非交互环境满足时返回失败，不降低驱动签名要求。实现依据：[SetupSetNonInteractiveMode](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupsetnoninteractivemode)、[UpdateDriverForPlugAndPlayDevicesW](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-updatedriverforplugandplaydevicesw)、[DiInstallDriverW](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-diinstalldriverw)。

程序不会自动重启 Windows，不关闭音频应用或停止音频服务。

## 4. 退出码

| 退出码 | 含义与调用方处理 |
| --- | --- |
| `0` | 安装/卸载流程成功，或查询成功；查询成功不代表驱动已安装，需读取状态字段 |
| `1` | 驱动操作、状态查询或启动失败；读取诊断及 `rebootRequired`，失败也可能需要重启 |
| `5` | 安全策略拒绝：安装器目录、EXE、驱动源文件或并发锁路径不是管理员控制路径 |
| `29` | 结果输出失败；打开输出文件失败时不开始驱动操作，操作后的写入失败则可能已经改变驱动状态 |
| `87` | 参数错误，未执行驱动操作 |
| `740` | 静默安装/卸载需要管理员权限，未执行驱动操作 |
| `1223` | 界面模式 UAC 被用户取消 |
| `1618` | 另一个安装器持有 Program Files 文件锁，或锁状态冲突，本次未执行操作/查询 |
| `3010` | 需要重启；结合 `outcome` 区分操作请求重启、操作被已有保护阻止、查询检测到待重启 |

界面提权启动还可能直接返回 `ShellExecuteExW` 的其他 Win32 错误。调用方应将未识别的非零退出码当作失败。

`3010` 不能一律理解为“两项驱动已经完全就绪”：安装流程在任一阶段请求重启时停止后续阶段，重启后先查询，如尚未就绪，再运行安装。卸载成功并返回 `3010` 时已经提交包移除并验证包数为零，剩余设备/文件清理由 Windows 在重启时完成。`outcome=blocked` 表示被既有重启保护阻止，重启后需重新提交原动作。

同一内核会话中的重启保护沿用 `HKLM\SOFTWARE\QuarborDriverInstaller.RestartRequired`（64 位、volatile）。更改前记录、成功且不需重启才清除；失败或中断保留。关闭安装器、移动 EXE、换参数都不能绕过保护。应使用 Windows 的“重启”，不能假设快速启动关机或注销已经清除保护。

## 5. JSON 协议（schemaVersion = 1）

静默命令向继承的标准输出句柄写入一条 UTF-8 JSON，以换行结尾；指定 `--output` 时同时写入 UTF-8 无 BOM 文件。不会为输出新建控制台。GUI 子系统程序在某些 Shell 下直接运行不会等待或显示标准输出，集成时应显式等待进程，使用重定向管道或结果文件。

以下为未安装驱动的示例，非当前机器采样：

```json
{
  "schemaVersion": 1,
  "action": "status",
  "outcome": "success",
  "exitCode": 0,
  "rebootRequired": false,
  "message": "Read-only driver status snapshot.",
  "status": {
    "complete": true,
    "rebootRequired": false,
    "hid": { "packages": 0, "service": "not_installed", "enabled": false, "ready": false },
    "microphone": { "packages": 0, "service": "not_installed", "enabled": false, "ready": false },
    "devices": [],
    "errors": []
  }
}
```

| 字段 | 语义 |
| --- | --- |
| `action` | `install`、`uninstall`、`status`、`help`；参数/输出预检失败分别使用 `arguments` / `output` |
| `outcome` | `success`、`reboot_required`、`blocked`、`failed`；`blocked` 专指现有重启保护阻止操作 |
| `exitCode` | 操作/查询返回码；若随后输出失败，实际进程码为 `29`，已有 JSON 内的码仍是操作结果 |
| `rebootRequired` | 操作或快照已确认的重启要求；失败后仍保留 `true`；无法确定时为 `null` |
| `message` | 英文诊断文本，可能包含 Windows 错误码；不作为稳定的程序判断字段 |
| `status` | 操作后的快照或查询快照；参数、权限、互斥等早期失败时为 `null` |
| `status.complete` | 所有状态读取是否成功；与驱动是否就绪无关 |
| `status.rebootRequired` | 本次只读快照的重启状态；无法查询时为 `null` |
| `hid` / `microphone` | 两个驱动的状态对象；各字段独立读取，失败不会掩盖其他字段 |
| `packages` | Driver Store 中匹配的包数量；读取失败为 `null`，不伪装成 `0` |
| `service` | `not_installed`、`pending_removal`、`running`、`stopped`、`starting`、`stopping`、`paused`、`changing`；未知为 `null` |
| `enabled` | 服务存在、为内核驱动且未禁用；不等同于正在运行；未知为 `null` |
| `ready` | HID：有驱动包且服务启用。麦克风：另外要求至少一个 present、started、driverBound、无问题且无待重启的设备；依赖字段读取失败为 `null` |
| `devices` | 麦克风设备数组，包含 `instanceId`、`present`、`started`、`driverBound`、`rebootRequired`、`problem`；枚举失败为 `null` |
| `errors` | 字段读取错误数组，每项含 `field`、`code`、`message`；`code` 为捕获的 Windows 错误码，非系统异常使用 `1` |

`ready` 是安装器的就绪判定，不是 HID 已挂载到具体键盘或麦克风音频业务正常的端到端证明。调用方判断可用性时还应检查整体 `rebootRequired`。

查询遇到任一字段错误时返回 `1`，但保留其他成功字段和已确认的重启要求。安装/卸载后的附加状态读取失败不覆盖已经取得的操作结果，避免调用方因查询失败重复安装；应另外检查 `status.complete`。状态快照由多次系统读取组成，不保证面对设备热插拔、设备管理器等外部操作的事务一致性。

输出文件在互斥量成功获取后、驱动操作前以独占新建方式打开，打开失败则不执行操作；写入或 stdout 管道失败返回 `29`。解析参数失败不使用 `--output`，错误 JSON 仅写 stdout。进程被强杀、资源耗尽等异常场景不保证能写出 JSON；调用方应检查进程码、结果文件完整性及协议版本，不能把旧结果文件作为本次结果。

## 6. 调用示例

### PowerShell：自动界面

```powershell
$process = Start-Process -FilePath '.\QuarborAxonkeyDriverInstaller.exe' `
    -ArgumentList '--install' -Wait -PassThru
$process.ExitCode   # 用户关闭结果窗口后得到退出码
```

卸载时将 `--install` 改为 `--uninstall`。

### PowerShell：静默安装并读取结果

从已经提权的 PowerShell 执行，调用本身不会弹 UAC：

```powershell
$resultPath = Join-Path $env:TEMP ('axonkey-' + [Guid]::NewGuid().ToString('N') + '.json')
$process = Start-Process -FilePath '.\QuarborAxonkeyDriverInstaller.exe' `
    -ArgumentList ('--install --silent --output "' + $resultPath + '"') -Wait -PassThru
$code = $process.ExitCode
$result = if (Test-Path -LiteralPath $resultPath) {
    Get-Content -LiteralPath $resultPath -Raw -Encoding UTF8 | ConvertFrom-Json
}
if ($code -eq 3010) {
    Write-Host ('需要重启；结果：' + $result.outcome)
} elseif ($code -ne 0) {
    throw "安装器返回 $code；诊断：$($result.message)；重启要求：$($result.rebootRequired)"
}
```

静默卸载替换动作为 `--uninstall --silent`；只读查询替换为 `--status`。查询返回 `0` 后，使用 `$result.status.hid.ready` 和 `$result.status.microphone.ready` 判断安装器定义的就绪状态。

### C#：捕获 stdout（.NET 6+）

```csharp
using System.Diagnostics;
using System.Text;
using System.Text.Json;

var start = new ProcessStartInfo(@"C:\Program Files\Axonkey\QuarborAxonkeyDriverInstaller.exe")
{
    UseShellExecute = false,
    CreateNoWindow = true,
    RedirectStandardOutput = true,
    StandardOutputEncoding = Encoding.UTF8
};
start.ArgumentList.Add("--status");
// 安装时替换为 --install、--silent，且调用进程必须已提权。
using var process = Process.Start(start) ?? throw new Exception("无法启动安装器");
var outputTask = process.StandardOutput.ReadToEndAsync();
await process.WaitForExitAsync();
var json = await outputTask;
using var report = JsonDocument.Parse(json);
Console.WriteLine($"ExitCode={process.ExitCode}, Result={report.RootElement}");
```

使用 Win32 时同样采用 `CreateProcessW`、继承 stdout 管道、读取输出并等待进程，再调用 `GetExitCodeProcess`。不要使用不等待的 Shell 启动结果代替安装结果。

## 7. 实现结构与验证

- `Main.cpp`：Windows 参数拆分、权限、界面提权、Program Files 文件锁、非交互 SetupAPI 范围、文件/stdout 输出和进程退出码。
- `InstallerCommandLine.h/.cpp`：严格参数校验、可注入后端的静默调度和 JSON 序列化。
- `InstallerWindow.cpp`：`initialAction` 在窗口首次创建时自动启动，页面语言导航不重跑动作。
- `WindowsInstaller.cpp`：复用 `InstallAll` / `UninstallAll` 和原有重启保护；另设 `ReadWindowsInstallerStatus`，只读取保护记录和 Windows 状态，不创建/清除保护记录。
- `VirtualMicrophoneSetup.cpp`：静默绑定时附加 `INSTALLFLAG_NONINTERACTIVE`。

构建：在 Visual Studio 开发者 PowerShell 中执行：

```powershell
MSBuild application\QuarborAxonkeyDriverInstaller\QuarborAxonkeyDriverInstaller.vcxproj /p:Configuration=Release /p:Platform=x64
MSBuild application\QuarborAxonkeyDriverInstaller\tests\DriverInstallerTests.vcxproj /p:Configuration=Debug /p:Platform=x64
.\artifacts\bin\x64\Debug\DriverInstallerTests\DriverInstallerTests.exe --ui-auto
powershell -NoProfile -ExecutionPolicy Bypass -File .\application\QuarborAxonkeyDriverInstaller\tests\Test-CommandLine.ps1 `
    -InstallerPath .\artifacts\bin\x64\Release\QuarborAxonkeyDriverInstaller\QuarborAxonkeyDriverInstaller.exe
```

自动化覆盖参数冲突、动作只执行一次、无点击自动开始、语言切换、成功/失败/重启/保护阻止、部分查询失败、未知状态、JSON 转义和 UTF-8、中文输出路径、真实进程退出码、输出预检失败及并发互斥。普通权限运行进程测试时还验证静默安装/卸载返回 `740`；管理员运行时跳过这两项，防止改动真实驱动。现有工作流测试继续覆盖中断保护、卸载顺序和安装阶段重启边界。

自动测试中的安装/卸载后端均为模拟实现，真实进程测试只运行只读或预检失败路径。签名驱动的实际安装、卸载、设备占用、重启恢复和 SYSTEM/Session 0 部署，需要在专用 Windows 测试机上进行生命周期验收：安装并查询、重复安装、卸载并查询、待重启时重试应被阻止、重启后查询/继续安装，以及不可信签名下的静默失败。

本次开发验证（2026-09-19）：Debug / Release x64 安装器构建通过，Debug 工作流/命令行单元测试及 `--ui-auto` 自动界面测试通过，Release EXE 真实进程测试通过（包含普通权限的 `740` 路径）。未在开发机执行真实驱动安装、卸载或重启。
