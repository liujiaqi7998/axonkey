#pragma once
#include <string_view>

namespace axonkey_service {
enum class LogLevel { Info, Warning, Error };
// UTF-8 log beside the EXE, capped at 100 KiB with no archives, plus debugger
// output. Logging must never throw or change the caller's Win32 last error.
void LogMessage(std::wstring_view message, LogLevel level = LogLevel::Info) noexcept;
void LogMessage(std::string_view message, LogLevel level = LogLevel::Info) noexcept;
}
