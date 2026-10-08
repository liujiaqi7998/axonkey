# AxonkeyService Rust

这是 Windows 服务的唯一用户态业务实现。旧 C++ 服务与 CMake 已移除；服务名、管道名、protobuf、驱动 ABI、注册表和安装路径保持兼容。服务版本仍为 `0.3.1`。架构见 [服务说明](../SERVICE_ARCHITECTURE.md)，验收进度见 [实施记录](../../../docs/WINDOWS_SERVICE_RUST_IMPLEMENTATION.md)。

在仓库根执行，需要 Rust 1.98.0、Visual Studio x64 MSVC 工具链和 Windows SDK：

```powershell
npm run build:windows-service
npm run test:windows-service
```

默认输出到 `windows/service/dist/AxonkeyService.exe` 及同次构建 PDB，也复制到 `.build/service-rust/dist`。Tauri 的现有开发/打包入口自动使用 Rust。仅想生成独立产物时仍可使用 `npm run build:windows-service:rust`，它不写生产 dist。`test:windows-service:rust` 同样保留为独立测试入口。

构建脚本核对固定编译器版本，显式指定 MSVC 目标、静态 CRT 和 `--locked`；首次构建下载依赖。安装在 stable 别名下且实际版本恰为 1.98.0 的编译器也可使用。无需设置 AXONKEY_SERVICE_IMPLEMENTATION；旧值 cpp 会明确报错。

```powershell
& .\windows\service\dist\AxonkeyService.exe --check
```

`--check` 仅检查公共头文件与 Rust ABI。无参数运行必须通过 SCM；真实运行按 Enabled 配置管理设备。自动测试使用隔离 HKCU 键、日志和管道，不注册服务、修改真实设备或替换已安装服务。

生产只编入 `native/boundary.c` 的 SEH 复制保护、内存屏障及 ABI 核对函数。原测试专用 C++ adapter 也已移除，音频回归改为 [107 组固定 C++ 输出摘要](tests/fixtures/README.md)。sha2 仅为测试依赖，不进入服务产物。
