#include <QuarborDeviceSetup.h>
#include <QuarborHidSelection.h>
#include "QuarborDriverPackage.Internal.h"
#include <setupapi.h>
#include <newdev.h>
#include <algorithm>
#include <filesystem>
#include <system_error>

namespace quarbor {
namespace {
namespace fs = std::filesystem;
constexpr wchar_t InfName[] = L"QuarborHIDFilterDriver.inf";
constexpr wchar_t SysName[] = L"QuarborHIDFilterDriver.sys";
constexpr wchar_t CatName[] = L"QuarborHIDFilterDriver.cat";
[[noreturn]] void Fail(const char* operation, DWORD error = GetLastError()) {
    throw std::system_error(static_cast<int>(error), std::system_category(), operation);
}
struct InfHandle {
    HINF value;
    explicit InfHandle(const std::wstring& path) : value(SetupOpenInfFileW(path.c_str(), nullptr, INF_STYLE_WIN4, nullptr)) {
        if (value == INVALID_HANDLE_VALUE) Fail("Open Quarbor INF");
    }
    ~InfHandle() { SetupCloseInfFile(value); }
};
std::wstring Field(INFCONTEXT& line, DWORD index) {
    DWORD needed = 0;
    if (!SetupGetStringFieldW(&line, index, nullptr, 0, &needed) && GetLastError() != ERROR_INSUFFICIENT_BUFFER)
        Fail("Read INF field");
    std::vector<wchar_t> buffer(needed + 1, L'\0');
    if (!SetupGetStringFieldW(&line, index, buffer.data(), static_cast<DWORD>(buffer.size()), nullptr)) Fail("Read INF field");
    return buffer.data();
}
std::wstring Value(HINF inf, const wchar_t* section, const wchar_t* key) {
    INFCONTEXT line{};
    if (!SetupFindFirstLineW(inf, section, key, &line)) return {};
    return Field(line, 1);
}
bool HasOurService(HINF inf, bool primitive) {
    for (UINT index = 0;; ++index) {
        wchar_t section[1024]{};
        if (!SetupEnumInfSectionsW(inf, index, section, ARRAYSIZE(section), nullptr)) {
            if (GetLastError() == ERROR_NO_MORE_ITEMS) return false;
            Fail("Enumerate INF sections");
        }
        if (primitive && !QuarborEqual(section, L"DefaultInstall.NTamd64.Services")) continue;
        INFCONTEXT line{};
        if (!SetupFindFirstLineW(inf, section, L"AddService", &line)) continue;
        do {
            if (!QuarborEqual(Field(line, 1).c_str(), FilterServiceName)) continue;
            if (primitive) {
                int flags = -1;
                if (!SetupGetIntField(&line, 2, &flags) || flags != 0) return false;
            }
            const auto serviceSection = Field(line, 3);
            const auto binary = Value(inf, serviceSection.c_str(), L"ServiceBinary");
            if (!QuarborEqual(fs::path(binary).filename().c_str(), SysName)) return false;
            return Value(inf, serviceSection.c_str(), L"ServiceType") == L"1";
        } while (SetupFindNextMatchLineW(&line, L"AddService", &line));
    }
}
bool OurInf(HINF inf, bool primitive) {
    if (!QuarborEqual(Value(inf, L"Version", L"Provider").c_str(), L"Quarbor") ||
        !QuarborEqual(Value(inf, L"Version", L"CatalogFile").c_str(), CatName)) return false;
    if (primitive && (SetupGetLineCountW(inf, L"DefaultInstall.NTamd64") < 0 ||
            SetupGetLineCountW(inf, L"Manufacturer") >= 0 ||
            !QuarborEqual(Value(inf, L"Version", L"ClassGuid").c_str(), L"{4D36E96B-E325-11CE-BFC1-08002BE10318}"))) return false;
    return HasOurService(inf, primitive);
}
std::wstring OriginalInfName(HINF inf) {
    DWORD required = 0;
    if (!SetupGetInfInformationW(inf, INFINFO_INF_SPEC_IS_HINF, nullptr, 0, &required)) Fail("Read published INF information");
    std::vector<ULONG_PTR> storage((required + sizeof(ULONG_PTR) - 1) / sizeof(ULONG_PTR));
    auto* information = reinterpret_cast<SP_INF_INFORMATION*>(storage.data());
    if (!SetupGetInfInformationW(inf, INFINFO_INF_SPEC_IS_HINF, information, required, nullptr)) Fail("Read published INF information");
    SP_ORIGINAL_FILE_INFO_W original{};
    original.cbSize = sizeof(original);
    if (!SetupQueryInfOriginalFileInformationW(information, 0, nullptr, &original)) Fail("Read original driver package name");
    return fs::path(original.OriginalInfName).filename().wstring();
}
bool PublishedName(const std::wstring& name) {
    if (name.size() < 8 || !QuarborEqual(name.substr(0, 3).c_str(), L"oem") ||
            !QuarborEqual(name.substr(name.size() - 4).c_str(), L".inf")) return false;
    return std::all_of(name.begin() + 3, name.end() - 4, [](wchar_t c) { return c >= L'0' && c <= L'9'; });
}
class WindowsPackages final : public detail::PackageBackend {
public:
    WindowsPackages(std::wstring directory, HWND owner) : directory_(std::move(directory)), owner_(owner) {}
    std::wstring ValidateSource() override { return ValidateDriverPackage(directory_); }
    bool Install(const std::wstring& inf) override {
        BOOL reboot = FALSE;
        if (!DiInstallDriverW(owner_, inf.c_str(), 0, &reboot)) Fail("Install driver package");
        return reboot != FALSE;
    }
    void CheckService() override { detail::CheckInstalledFilterService(); }
    std::vector<std::wstring> FindInstalled() override { return FindInstalledDriverPackages(); }
    bool ClearRegistrations(bool includeDevices) override { return detail::ClearFilterRegistrations(includeDevices); }
    bool Uninstall(const std::wstring& path) override {
        // Revalidate immediately before removing each package, including its
        // original filename. Never infer ownership from a service name alone.
        {
            InfHandle inf(path);
            if (!QuarborEqual(OriginalInfName(inf.value).c_str(), InfName) || !OurInf(inf.value, false))
                Fail("Published driver package identity changed; refresh and retry", ERROR_INVALID_DATA);
        }
        BOOL reboot = FALSE;
        if (!DiUninstallDriverW(owner_, path.c_str(), 0, &reboot)) {
            const DWORD error = GetLastError();
            const auto name = fs::path(path).filename().string();
            Fail(("Filter registrations removed, but " + name + " could not be uninstalled. Reboot Windows and retry").c_str(), error);
        }
        return reboot != FALSE;
    }
private:
    std::wstring directory_;
    HWND owner_;
};
} // namespace

std::wstring ValidateDriverPackage(const std::wstring& directory) {
    const fs::path folder = fs::absolute(directory);
    for (const auto* name : {InfName, SysName, CatName}) {
        if (!fs::is_regular_file(folder / name)) {
            const auto file = fs::path(name).string();
            Fail(("Place " + file + " beside QuarborHIDFilterDriverTest.exe").c_str(), ERROR_FILE_NOT_FOUND);
        }
    }
    const auto path = (folder / InfName).wstring();
    InfHandle inf(path);
    if (!OurInf(inf.value, true)) Fail("Use the Quarbor HID Keyboard primitive INF/SYS/CAT package", ERROR_INVALID_DATA);
    return path;
}

std::vector<std::wstring> FindInstalledDriverPackages() {
    wchar_t windows[MAX_PATH]{};
    const UINT length = GetWindowsDirectoryW(windows, ARRAYSIZE(windows));
    if (!length || length >= ARRAYSIZE(windows)) Fail("Find Windows INF directory");
    std::vector<std::wstring> result;
    for (const auto& entry : fs::directory_iterator(fs::path(windows) / L"INF")) {
        if (!PublishedName(entry.path().filename().wstring()) || !entry.is_regular_file()) continue;
        InfHandle inf(entry.path().wstring());
        // Cheap identity check before querying Driver Store metadata.
        if (!OurInf(inf.value, false)) continue;
        if (QuarborEqual(OriginalInfName(inf.value).c_str(), InfName)) result.push_back(entry.path().wstring());
    }
    std::sort(result.begin(), result.end());
    return result;
}

DriverPackageResult InstallDriverPackage(const std::wstring& directory, HWND owner) {
    WindowsPackages backend(directory, owner);
    return detail::InstallPackage(backend);
}
DriverPackageResult UninstallDriverPackages(HWND owner) {
    WindowsPackages backend({}, owner);
    return detail::UninstallPackages(backend);
}
void UninstallDriverPackages(DriverPackageResult& result, HWND owner) {
    WindowsPackages backend({}, owner);
    detail::UninstallPackages(backend, result);
}
} // namespace quarbor
