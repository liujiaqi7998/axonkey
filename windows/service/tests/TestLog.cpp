#include "../ServiceLog.h"
#include <iostream>

namespace axonkey_service {
// Simulated failures must not be confused with live hardware/service errors.
void LogMessage(std::wstring_view message, LogLevel) noexcept {
    try { std::wcerr << L"[test] " << message << L'\n'; } catch (...) {}
}
void LogMessage(std::string_view message, LogLevel) noexcept {
    try { std::cerr << "[test] " << message << '\n'; } catch (...) {}
}
}
