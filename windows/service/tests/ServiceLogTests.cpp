#include "../ServiceLog.h"
#include <windows.h>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <regex>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

using namespace axonkey_service;
namespace fs = std::filesystem;
namespace {
constexpr std::uintmax_t kLimit = 100 * 1024;
void Require(bool value, const char* message) {
    if (!value) throw std::runtime_error(message);
}
fs::path Executable() {
    std::wstring path(32768, L'\0');
    const auto length = GetModuleFileNameW(nullptr, path.data(), static_cast<DWORD>(path.size()));
    Require(length && length < path.size(), "executable path");
    path.resize(length);
    return path;
}
std::string Read(const fs::path& file) {
    std::ifstream input(file, std::ios::binary);
    return {std::istreambuf_iterator<char>(input), std::istreambuf_iterator<char>()};
}
void WriteChecked(std::wstring_view message, LogLevel level = LogLevel::Info) {
    SetLastError(ERROR_ACCESS_DENIED);
    LogMessage(message, level);
    Require(GetLastError() == ERROR_ACCESS_DENIED, "logging changed GetLastError");
}
void Child(const std::string& mode) {
    const auto log = Executable().parent_path() / L"AxonkeyService.log";
    if (mode == "basic") {
        WriteChecked(L"服务已启动 中文 😀");
        WriteChecked(L"recoverable warning", LogLevel::Warning);
        WriteChecked(L"operation failed", LogLevel::Error);
        WriteChecked(L"first\r\nsecond");
        SetLastError(ERROR_INVALID_HANDLE);
        LogMessage("narrow UTF-8: 中文", LogLevel::Info);
        Require(GetLastError() == ERROR_INVALID_HANDLE, "narrow logging changed GetLastError");
    } else if (mode == "overflow") {
        WriteChecked(std::wstring(200000, L'中'));
        Require(fs::file_size(log) <= kLimit, "oversized record exceeded cap");
        Require(Read(log).find("[truncated]") != std::string::npos, "missing truncation marker");
        for (int i = 0; i < 1800; ++i) {
            WriteChecked(L"rotation record " + std::to_wstring(i) + std::wstring(100, L'x'));
            Require(fs::file_size(log) <= kLimit, "rotation exceeded cap");
        }
        WriteChecked(L"latest record");
    } else if (mode == "concurrent") {
        std::vector<std::thread> threads;
        for (int i = 0; i < 6; ++i) threads.emplace_back([i] {
            for (int j = 0; j < 40; ++j)
                LogMessage("thread=" + std::to_string(i) + " record=" + std::to_string(j));
        });
        for (auto& thread : threads) thread.join();
    } else {
        WriteChecked(L"restart marker");
    }
}
void RunChild(const fs::path& exe, const fs::path& cwd, const wchar_t* mode) {
    std::wstring command = L"\"" + exe.wstring() + L"\" " + mode;
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    PROCESS_INFORMATION process{};
    Require(CreateProcessW(exe.c_str(), command.data(), nullptr, nullptr, FALSE,
        CREATE_NO_WINDOW, nullptr, cwd.c_str(), &startup, &process) != FALSE, "create child");
    const auto wait = WaitForSingleObject(process.hProcess, 30000);
    DWORD exit = 1;
    if (wait != WAIT_OBJECT_0) {
        TerminateProcess(process.hProcess, 1);
        WaitForSingleObject(process.hProcess, 5000);
    } else GetExitCodeProcess(process.hProcess, &exit);
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
    Require(wait == WAIT_OBJECT_0 && exit == 0, "logging child failed");
}
void VerifyLines(const std::string& text, size_t expected) {
    const std::regex line(R"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}[+-]\d{2}:\d{2} \[(info|warning|error)\] \[AxonkeyService\] \[pid=\d+ tid=\d+\] [^\r\n]*\r?)");
    size_t start = 0, count = 0;
    while (start < text.size()) {
        const auto end = text.find('\n', start);
        Require(end != std::string::npos, "partial log record");
        Require(std::regex_match(text.substr(start, end - start), line), "invalid log format or interleaved record");
        start = end + 1;
        ++count;
    }
    Require(count == expected, "unexpected number of log records");
}
}
int main(int argc, char** argv) {
    fs::path temporary;
    try {
        if (argc > 1) { Child(argv[1]); return 0; }
        temporary = fs::temp_directory_path() / (L"Axonkey 日志测试 " + std::to_wstring(GetCurrentProcessId()) +
            L"-" + std::to_wstring(GetTickCount64()));
        const auto bin = temporary / L"服务目录";
        const auto cwd = temporary / L"工作目录";
        fs::create_directories(bin);
        fs::create_directories(cwd);
        const auto exe = bin / L"ServiceLogTests.exe";
        const auto log = bin / L"AxonkeyService.log";
        fs::copy_file(Executable(), exe);
        RunChild(exe, cwd, L"basic");
        auto text = Read(log);
        Require(!fs::exists(cwd / L"AxonkeyService.log"), "log used working directory");
        Require(text.find("服务已启动 中文 😀") != std::string::npos, "wide UTF-8 conversion");
        Require(text.find("narrow UTF-8: 中文") != std::string::npos, "narrow UTF-8 conversion");
        Require(text.find("[warning]") != std::string::npos && text.find("[error]") != std::string::npos, "severity levels");
        VerifyLines(text, 5);
        RunChild(exe, cwd, L"restart");
        Require(Read(log).starts_with(text), "restart discarded existing log");
        RunChild(exe, cwd, L"overflow");
        text = Read(log);
        Require(text.find("latest record") != std::string::npos, "newest record lost");
        Require(text.find("服务已启动") == std::string::npos, "old records not discarded");
        Require(fs::file_size(log) <= kLimit, "file exceeds cap");
        Require(std::distance(fs::directory_iterator(bin), fs::directory_iterator{}) == 2, "unexpected archive files");
        { std::ofstream seed(log, std::ios::binary | std::ios::trunc); seed << std::string(200000, 'x'); }
        RunChild(exe, cwd, L"restart");
        Require(fs::file_size(log) <= kLimit && Read(log).find("restart marker") != std::string::npos, "oversized startup log");
        fs::remove(log);
        RunChild(exe, cwd, L"concurrent");
        VerifyLines(Read(log), 240);
        fs::remove(log);
        fs::create_directory(log); // Deterministically make opening the log fail.
        RunChild(exe, cwd, L"unwritable");
        fs::remove_all(temporary);
        std::cout << "Service logging regression tests passed\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        if (!temporary.empty()) { std::error_code ignored; fs::remove_all(temporary, ignored); }
        return 1;
    }
}
