#pragma once
#include <QuarborDeviceSetup.h>

namespace quarbor::detail {
// Cleanup is shared with native package management. Exact service names only;
// includes known offline device instances when requested. Returns reboot need.
bool ClearFilterRegistrations(bool includeDevices);
void CheckInstalledFilterService();

struct PackageBackend {
    virtual ~PackageBackend() = default;
    virtual std::wstring ValidateSource() = 0;
    virtual bool Install(const std::wstring& inf) = 0;
    virtual void CheckService() = 0;
    virtual std::vector<std::wstring> FindInstalled() = 0;
    virtual bool ClearRegistrations(bool includeDevices) = 0;
    virtual bool Uninstall(const std::wstring& inf) = 0;
};

inline DriverPackageResult InstallPackage(PackageBackend& backend) {
    const auto inf = backend.ValidateSource();
    DriverPackageResult result;
    result.rebootRequired = backend.Install(inf);
    backend.CheckService();
    result.packagesChanged = 1;
    // Preserve per-device choices; remove unsupported class-wide registration.
    result.rebootRequired = backend.ClearRegistrations(false) || result.rebootRequired;
    return result;
}

inline void UninstallPackages(PackageBackend& backend, DriverPackageResult& result) {
    // Discover before changing the system. Cleanup failure must leave packages
    // installed, otherwise a remaining filter reference can break a keyboard.
    const auto packages = backend.FindInstalled();
    result.rebootRequired = backend.ClearRegistrations(true) || result.rebootRequired;
    for (const auto& inf : packages) {
        // Removing a package can make Windows bind another matching package
        // and recreate its AddReg filters. Clear those
        // references again before removing that next package.
        if (result.packagesChanged != 0)
            result.rebootRequired = backend.ClearRegistrations(true) || result.rebootRequired;
        result.rebootRequired = backend.Uninstall(inf) || result.rebootRequired;
        ++result.packagesChanged;
    }
    if (!packages.empty()) result.rebootRequired = backend.ClearRegistrations(true) || result.rebootRequired;
}

inline DriverPackageResult UninstallPackages(PackageBackend& backend) {
    DriverPackageResult result;
    UninstallPackages(backend, result);
    return result;
}
} // namespace quarbor::detail
