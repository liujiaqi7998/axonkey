#include "axonkey_rpc.h"

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

namespace {

int failures = 0;

void Expect(bool ok, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "FAIL: %s\n", what);
        ++failures;
    }
}

template <typename T>
void ExpectEq(const T& a, const T& b, const char* what) {
    if (!(a == b)) {
        std::fprintf(stderr, "FAIL: %s\n", what);
        ++failures;
    }
}

void ExpectNear(float a, float b, const char* what) {
    if (std::fabs(a - b) > 0.0001f) {
        std::fprintf(stderr, "FAIL: %s (%f vs %f)\n", what, a, b);
        ++failures;
    }
}

void TestRequestRoundTrip() {
    axonkey::rpc::Request in;
    in.requestId = 42;
    in.method = "SetAudioGain";
    in.payload = {0x08, 0x04}; // gain_db = 4 as classic varint field
    const auto bytes = axonkey::rpc::Serialize(in);
    Expect(!bytes.empty(), "request serialize non-empty");
    axonkey::rpc::Request out;
    Expect(axonkey::rpc::Parse(bytes, out), "request parse");
    ExpectEq(out.requestId, in.requestId, "request id");
    ExpectEq(out.method, in.method, "request method");
    ExpectEq(out.payload, in.payload, "request payload");
}

void TestServiceInfoRoundTrip() {
    axonkey::rpc::ServiceInfo in{
        "AxonkeyService", "0.3.1", "axonkey.service.v1", "\\\\.\\pipe\\AxonkeyService.v1"};
    const auto bytes = axonkey::rpc::Serialize(in);
    axonkey::rpc::ServiceInfo out;
    Expect(axonkey::rpc::Parse(bytes, out), "service info parse");
    ExpectEq(out.name, in.name, "name");
    ExpectEq(out.version, in.version, "version");
    ExpectEq(out.protocolVersion, in.protocolVersion, "protocol");
    ExpectEq(out.pipeName, in.pipeName, "pipe");
}

void TestServiceStatusRoundTrip() {
    axonkey::rpc::ServiceStatus status{false};
    auto bytes = axonkey::rpc::Serialize(status);
    axonkey::rpc::ServiceStatus outStatus;
    Expect(axonkey::rpc::Parse(bytes, outStatus), "service status parse");
    ExpectEq(outStatus.enabled, false, "service status disabled");

    axonkey::rpc::SetServiceStatus request{true};
    bytes = axonkey::rpc::Serialize(request);
    axonkey::rpc::SetServiceStatus outRequest;
    Expect(axonkey::rpc::Parse(bytes, outRequest), "set service status parse");
    ExpectEq(outRequest.enabled, true, "set service status enabled");
}

void TestDeviceListRoundTrip() {
    axonkey::rpc::DeviceList in;
    in.devices.push_back({"HID\\VID_2717", "\\\\.\\Quarbor0", true, true, true, true});
    in.devices.push_back({"HID\\VID_0001", "\\\\.\\Quarbor1", false, false, false, false});
    in.devices[0].batteryLevel = 87;
    in.devices[0].descriptionName = "Xiaomi RC003";
    const auto bytes = axonkey::rpc::Serialize(in);
    axonkey::rpc::DeviceList out;
    Expect(axonkey::rpc::Parse(bytes, out), "device list parse");
    ExpectEq(out.devices.size(), in.devices.size(), "device count");
    if (out.devices.size() == 2) {
        ExpectEq(out.devices[0].instanceId, in.devices[0].instanceId, "d0 id");
        ExpectEq(out.devices[0].endpointPath, in.devices[0].endpointPath, "d0 path");
        ExpectEq(out.devices[0].driverMounted, true, "d0 mounted");
        ExpectEq(out.devices[0].batteryLevel, in.devices[0].batteryLevel, "d0 battery");
        ExpectEq(out.devices[0].descriptionName, in.devices[0].descriptionName, "d0 description");
        Expect(!out.devices[1].batteryLevel.has_value(), "d1 battery empty");
        Expect(out.devices[1].descriptionName.empty(), "d1 description empty");
        ExpectEq(out.devices[1].connected, false, "d1 connected");
    }
}

void TestAudioLevelRoundTrip() {
    axonkey::rpc::AudioLevel in{0.75f, 0.25f, 123456789ull};
    const auto bytes = axonkey::rpc::Serialize(in);
    axonkey::rpc::AudioLevel out;
    Expect(axonkey::rpc::Parse(bytes, out), "audio level parse");
    ExpectNear(out.peak, in.peak, "peak");
    ExpectNear(out.rms, in.rms, "rms");
    ExpectEq(out.timestampMs, in.timestampMs, "timestamp");
}

void TestKeyboardEventRoundTrip() {
    axonkey::rpc::KeyboardEvent in{"dev-1", {0x01, 0x00, 0x04, 0x00}, 99};
    const auto bytes = axonkey::rpc::Serialize(in);
    axonkey::rpc::KeyboardEvent out;
    Expect(axonkey::rpc::Parse(bytes, out), "keyboard parse");
    ExpectEq(out.deviceInstanceId, in.deviceInstanceId, "kb device");
    ExpectEq(out.report, in.report, "kb report");
    ExpectEq(out.timestampMs, in.timestampMs, "kb ts");
}

void TestSubscribeAndGain() {
    axonkey::rpc::Subscribe sub{true, false, true};
    auto bytes = axonkey::rpc::Serialize(sub);
    axonkey::rpc::Subscribe outSub;
    Expect(axonkey::rpc::Parse(bytes, outSub), "subscribe parse");
    ExpectEq(outSub.keyboard, true, "sub kb");
    ExpectEq(outSub.audioLevel, false, "sub al");
    ExpectEq(outSub.voiceStatus, true, "sub vs");

    axonkey::rpc::SetAudioGain gain{-6};
    bytes = axonkey::rpc::Serialize(gain);
    axonkey::rpc::SetAudioGain outGain;
    Expect(axonkey::rpc::Parse(bytes, outGain), "gain parse");
    ExpectEq(outGain.gainDb, std::int32_t{-6}, "gain db");
}

void TestResponseEventVoice() {
    axonkey::rpc::Response response{7, true, "", {0x08, 0x01}};
    auto bytes = axonkey::rpc::Serialize(response);
    axonkey::rpc::Response outResp;
    Expect(axonkey::rpc::Parse(bytes, outResp), "response parse");
    ExpectEq(outResp.requestId, response.requestId, "resp id");
    ExpectEq(outResp.success, true, "resp ok");
    ExpectEq(outResp.payload, response.payload, "resp payload");

    axonkey::rpc::EventEnvelope event{"keyboard", {0x01, 0x02}};
    bytes = axonkey::rpc::Serialize(event);
    axonkey::rpc::EventEnvelope outEvent;
    Expect(axonkey::rpc::Parse(bytes, outEvent), "event parse");
    ExpectEq(outEvent.type, event.type, "event type");
    ExpectEq(outEvent.payload, event.payload, "event payload");

    axonkey::rpc::VoiceStatus voice{"active", "HID\\RC003", true, true, true, 3, 9};
    bytes = axonkey::rpc::Serialize(voice);
    axonkey::rpc::VoiceStatus outVoice;
    Expect(axonkey::rpc::Parse(bytes, outVoice), "voice parse");
    ExpectEq(outVoice.state, voice.state, "voice state");
    ExpectEq(outVoice.deviceInstanceId, voice.deviceInstanceId, "voice device");
    ExpectEq(outVoice.protocolVersion, voice.protocolVersion, "voice proto");
    ExpectEq(outVoice.sessionId, voice.sessionId, "voice session");
}

void TestRejectOversizedField() {
    // Craft a length-delimited string field (tag 2 = method) with a huge length.
    // Tag (field 2, wire 2) = 0x12, then a multi-byte varint length > 1 MiB.
    axonkey::rpc::Bytes evil;
    evil.push_back(0x12);
    // varint 2 MiB = 0x80 0x80 0x80 0x01 roughly — use 0x80 0x80 0x80 0x01 = 0x200000
    evil.push_back(0x80);
    evil.push_back(0x80);
    evil.push_back(0x80);
    evil.push_back(0x01);
    axonkey::rpc::Request out;
    Expect(!axonkey::rpc::Parse(evil, out), "reject oversized method string");
}

void TestKnownWireGain() {
    // SetAudioGain gain_db=2 -> field 1 varint: 08 02
    const axonkey::rpc::Bytes classic{0x08, 0x02};
    axonkey::rpc::SetAudioGain gain;
    Expect(axonkey::rpc::Parse(classic, gain), "classic gain parse");
    ExpectEq(gain.gainDb, std::int32_t{2}, "classic gain value");
    const auto encoded = axonkey::rpc::Serialize(axonkey::rpc::SetAudioGain{2});
    ExpectEq(encoded, classic, "classic gain encode matches hand wire");
}

} // namespace

int main() {
    TestRequestRoundTrip();
    TestServiceInfoRoundTrip();
    TestServiceStatusRoundTrip();
    TestDeviceListRoundTrip();
    TestAudioLevelRoundTrip();
    TestKeyboardEventRoundTrip();
    TestSubscribeAndGain();
    TestResponseEventVoice();
    TestRejectOversizedField();
    TestKnownWireGain();
    if (failures) {
        std::fprintf(stderr, "%d test(s) failed\n", failures);
        return 1;
    }
    std::puts("axonkey_rpc_tests: all passed");
    return 0;
}
