#include <windows.h>
#include "../GattAccess.h"
#include <iostream>
#include <stdexcept>

namespace {
template<typename F>
void ExpectUnavailable(F operation) {
    try { operation(); }
    catch (const winrt::hresult_error& error) {
        if (error.code() == HRESULT_FROM_WIN32(ERROR_NOT_READY)) return;
        throw;
    }
    throw std::runtime_error("Null GATT service was not rejected");
}

struct FailingClose {
    bool present = true;
    int calls = 0;
    explicit operator bool() const { return present; }
    void Close() { ++calls; throw std::runtime_error("Device disconnected during Close"); }
    void operator=(std::nullptr_t) { present = false; }
};
}

int main() {
    try {
        // Reproduce the API outcome that caused the service's access violation.
        // These tests require no paired hardware or administrator privileges.
        axonkey_service::gatt::GattDeviceService unavailable{nullptr};
        ExpectUnavailable([&] { axonkey_service::RequireGattSession(unavailable); });
        ExpectUnavailable([&] { axonkey_service::FindCharacteristic(unavailable, winrt::guid{}); });
        axonkey_service::CloseGattObject(unavailable);

        FailingClose first, second;
        axonkey_service::CloseGattObject(first);
        axonkey_service::CloseGattObject(second);
        axonkey_service::CloseGattObject(first);
        if (first.present || second.present || first.calls != 1 || second.calls != 1)
            throw std::runtime_error("Cleanup did not release all objects exactly once");
        std::cout << "PASS: null service/session discovery and exception-safe cleanup\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
    } catch (...) {
        std::cerr << "Unexpected GATT test exception\n";
    }
    return 1;
}
