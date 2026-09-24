#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace axonkey::rpc {

using Bytes = std::vector<std::uint8_t>;

struct Request { std::uint64_t requestId = 0; std::string method; Bytes payload; };
struct Response { std::uint64_t requestId = 0; bool success = false; std::string error; Bytes payload; };
struct EventEnvelope { std::string type; Bytes payload; };

struct ServiceInfo {
    std::string name, version, protocolVersion, pipeName;
    std::int32_t audioGainDb = 0;
};
struct ServiceStatus { bool enabled = true; };
struct SetServiceStatus { bool enabled = true; };
struct SetServiceEnable { bool enabled = true; };
struct SetAudioGain { std::int32_t gainDb = 0; };
struct OperationResult { bool success = false; std::string error; };
struct Device {
    std::string instanceId, endpointPath;
    bool driverMounted = false, inputBlocked = false, dataForwardEnabled = false, connected = false;
    std::optional<std::uint8_t> batteryLevel;
    std::string descriptionName;
};
struct DeviceList { std::vector<Device> devices; };
struct VoiceStatus {
    std::string state, deviceInstanceId;
    bool connected = false, active = false, microphoneOpen = false;
    std::uint32_t protocolVersion = 0, sessionId = 0;
};
struct AudioLevel { float peak = 0, rms = 0; std::uint64_t timestampMs = 0; };
struct KeyboardEvent { std::string deviceInstanceId; Bytes report; std::uint64_t timestampMs = 0; };
struct ServiceIssue {
    std::string code, message, deviceInstanceId;
    std::uint32_t nativeError = 0;
    bool recoverable = true;
    std::uint64_t timestampMs = 0;
};
struct Subscribe { bool keyboard = false, audioLevel = false, voiceStatus = false, serviceIssues = false; };

// Wire codec backed by nanopb (see generated/axonkey_service.pb.*).
bool Parse(const Bytes& bytes, Request& value);
bool Parse(const Bytes& bytes, SetAudioGain& value);
bool Parse(const Bytes& bytes, Subscribe& value);
bool Parse(const Bytes& bytes, DeviceList& value);
bool Parse(const Bytes& bytes, VoiceStatus& value);
bool Parse(const Bytes& bytes, AudioLevel& value);
bool Parse(const Bytes& bytes, KeyboardEvent& value);
bool Parse(const Bytes& bytes, ServiceIssue& value);
bool Parse(const Bytes& bytes, Response& value);
bool Parse(const Bytes& bytes, EventEnvelope& value);
bool Parse(const Bytes& bytes, ServiceInfo& value);
bool Parse(const Bytes& bytes, ServiceStatus& value);
bool Parse(const Bytes& bytes, SetServiceStatus& value);
bool Parse(const Bytes& bytes, SetServiceEnable& value);
bool Parse(const Bytes& bytes, OperationResult& value);

Bytes Serialize(const Request& value);
Bytes Serialize(const Response& value);
Bytes Serialize(const EventEnvelope& value);
Bytes Serialize(const ServiceInfo& value);
Bytes Serialize(const ServiceStatus& value);
Bytes Serialize(const SetServiceStatus& value);
Bytes Serialize(const SetServiceEnable& value);
Bytes Serialize(const OperationResult& value);
Bytes Serialize(const DeviceList& value);
Bytes Serialize(const VoiceStatus& value);
Bytes Serialize(const AudioLevel& value);
Bytes Serialize(const KeyboardEvent& value);
Bytes Serialize(const ServiceIssue& value);
Bytes Serialize(const SetAudioGain& value);
Bytes Serialize(const Subscribe& value);

} // namespace axonkey::rpc
