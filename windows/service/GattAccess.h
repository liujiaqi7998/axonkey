#pragma once

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Devices.Bluetooth.GenericAttributeProfile.h>

namespace axonkey_service {
namespace gatt = winrt::Windows::Devices::Bluetooth::GenericAttributeProfile;

// A successful WinRT async operation can return a null runtime object. Calling
// a projected method on it dereferences a null ABI pointer, not a C++ exception.
inline gatt::GattSession RequireGattSession(const gatt::GattDeviceService& service) {
    if (!service)
        throw winrt::hresult_error(HRESULT_FROM_WIN32(ERROR_NOT_READY),
            L"Opening RC003 ATVV returned no service (unavailable or access denied)");
    auto session = service.Session();
    if (!session)
        throw winrt::hresult_error(HRESULT_FROM_WIN32(ERROR_NOT_READY), L"RC003 GATT session is unavailable");
    return session;
}

inline gatt::GattCharacteristic FindCharacteristic(const gatt::GattDeviceService& service, const winrt::guid& uuid) {
    if (!service)
        throw winrt::hresult_error(HRESULT_FROM_WIN32(ERROR_NOT_READY), L"RC003 GATT service is unavailable");
    const auto result = service.GetCharacteristicsForUuidAsync(uuid).get();
    // Failed discovery need not return a Characteristics collection.
    if (!result || result.Status() != gatt::GattCommunicationStatus::Success)
        throw winrt::hresult_error(E_FAIL, L"RC003 characteristic discovery failed");
    const auto characteristics = result.Characteristics();
    if (!characteristics || characteristics.Size() == 0)
        throw winrt::hresult_error(E_NOINTERFACE, L"RC003 GATT characteristic was not found");
    auto characteristic = characteristics.GetAt(0);
    if (!characteristic)
        throw winrt::hresult_error(E_NOINTERFACE, L"RC003 GATT characteristic is unavailable");
    return characteristic;
}

// Cleanup must continue after an individual WinRT Close failure, including
// when called from an exception handler at the worker-thread boundary.
template<typename T>
void CloseGattObject(T& object) noexcept {
    if (!object) return;
    try { object.Close(); } catch (...) {}
    object = nullptr;
}
} // namespace axonkey_service
