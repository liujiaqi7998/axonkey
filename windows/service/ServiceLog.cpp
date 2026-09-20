#include "ServiceLog.h"
#include <windows.h>
#include <spdlog/logger.h>
#include <spdlog/sinks/msvc_sink.h>
#include <spdlog/sinks/rotating_file_sink.h>
#include <algorithm>
#include <filesystem>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace axonkey_service {
namespace {
constexpr size_t kMaxLogBytes = 100 * 1024;
// Bound individual records too: spdlog permits a single oversized record.
constexpr size_t kMaxMessageBytes = 16 * 1024;

struct LastErrorGuard {
    DWORD value = GetLastError();
    ~LastErrorGuard() { SetLastError(value); }
};

std::filesystem::path LogPath() {
    std::wstring path(32768, L'\0');
    const auto length = GetModuleFileNameW(nullptr, path.data(), static_cast<DWORD>(path.size()));
    if (!length || length >= path.size()) throw std::runtime_error("Cannot resolve service executable path");
    path.resize(length);
    return std::filesystem::path(path).parent_path() / L"AxonkeyService.log";
}

spdlog::logger& Logger() {
    static const auto logger = [] {
        std::vector<spdlog::sink_ptr> sinks{std::make_shared<spdlog::sinks::msvc_sink_mt>()};
        std::string fileError;
        try {
            const auto path = LogPath();
            std::error_code error;
            const auto size = std::filesystem::file_size(path, error);
            // Also enforce the limit when an older service left a larger file.
            sinks.push_back(std::make_shared<spdlog::sinks::rotating_file_sink_mt>(
                path.native(), kMaxLogBytes, 0, !error && size > kMaxLogBytes));
        } catch (const std::exception& error) {
            fileError = error.what();
        }
        auto result = std::make_shared<spdlog::logger>("AxonkeyService", sinks.begin(), sinks.end());
        result->set_pattern("%Y-%m-%dT%H:%M:%S.%e%z [%l] [%n] [pid=%P tid=%t] %v");
        result->flush_on(spdlog::level::info);
        result->set_error_handler([](const std::string& error) {
            OutputDebugStringA(("AxonkeyService logging failed: " + error + "\n").c_str());
        });
        if (!fileError.empty()) result->error("File logging unavailable: {}", fileError);
        return result;
    }();
    return *logger;
}

void Write(std::string_view message, LogLevel level) {
    size_t length = (std::min)(message.size(), kMaxMessageBytes);
    // Do not split a UTF-8 code point when shortening a long message.
    if (length < message.size()) {
        while (length && (static_cast<unsigned char>(message[length]) & 0xc0) == 0x80) --length;
    }
    std::string text(message.substr(0, length));
    for (auto& ch : text) if (ch == '\r' || ch == '\n' || ch == '\0') ch = ' ';
    if (length < message.size()) text += " [truncated]";
    const auto severity = level == LogLevel::Error ? spdlog::level::err :
        level == LogLevel::Warning ? spdlog::level::warn : spdlog::level::info;
    Logger().log(severity, "{}", text);
}
}

void LogMessage(std::string_view message, LogLevel level) noexcept {
    const LastErrorGuard guard;
    try { Write(message, level); }
    catch (...) { OutputDebugStringA("AxonkeyService logging failed\n"); }
}

void LogMessage(std::wstring_view message, LogLevel level) noexcept {
    const LastErrorGuard guard;
    try {
        // Bound conversion work even if the original message is huge.
        auto length = (std::min)(message.size(), kMaxMessageBytes);
        if (length < message.size() && length && message[length - 1] >= 0xd800 && message[length - 1] <= 0xdbff) --length;
        const auto bytes = WideCharToMultiByte(CP_UTF8, 0, message.data(), static_cast<int>(length), nullptr, 0, nullptr, nullptr);
        std::string encoded(bytes, '\0');
        if (bytes) WideCharToMultiByte(CP_UTF8, 0, message.data(), static_cast<int>(length), encoded.data(), bytes, nullptr, nullptr);
        if (length < message.size()) encoded += " [truncated]";
        Write(encoded, level);
    } catch (...) { OutputDebugStringA("AxonkeyService logging failed\n"); }
}
} // namespace axonkey_service
