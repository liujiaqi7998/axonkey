#include "axonkey_rpc.h"
#include <cassert>

int main() {
    using namespace axonkey::rpc;
    Request request{42, "GetServiceInfo", {1, 2, 3}};
    Request decoded;
    assert(Parse(Serialize(request), decoded));
    assert(decoded.requestId == request.requestId && decoded.method == request.method && decoded.payload == request.payload);

    AudioLevel level{0.75f, 0.25f, 1234};
    AudioLevel levelDecoded;
    assert(Parse(Serialize(level), levelDecoded));
    assert(levelDecoded.peak == level.peak && levelDecoded.rms == level.rms && levelDecoded.timestampMs == level.timestampMs);

    KeyboardEvent keyboard{"HID\\VID_2717&PID_32B8", {0, 4, 0}, 99};
    KeyboardEvent keyboardDecoded;
    assert(Parse(Serialize(keyboard), keyboardDecoded));
    assert(keyboardDecoded.deviceInstanceId == keyboard.deviceInstanceId && keyboardDecoded.report == keyboard.report);
    return 0;
}
