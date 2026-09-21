#include "axonkey_rpc.h"

#include "axonkey_service.pb.h"
#include <pb_decode.h>
#include <pb_encode.h>

#include <cstring>
#include <limits>

namespace axonkey::rpc {
namespace {

// Named-pipe frames are capped at 1 MiB in RpcServer; reject oversized fields.
constexpr size_t kMaxFieldBytes = 1024 * 1024;

bool DecodeString(pb_istream_t* stream, const pb_field_t* /*field*/, void** arg) {
    auto* out = static_cast<std::string*>(*arg);
    const size_t length = stream->bytes_left;
    if (length > kMaxFieldBytes) return false;
    out->resize(length);
    return length == 0 || pb_read(stream, reinterpret_cast<pb_byte_t*>(out->data()), length);
}

bool EncodeString(pb_ostream_t* stream, const pb_field_t* field, void* const* arg) {
    const auto* value = static_cast<const std::string*>(*arg);
    if (value->empty()) return true;
    if (!pb_encode_tag_for_field(stream, field)) return false;
    return pb_encode_string(stream, reinterpret_cast<const pb_byte_t*>(value->data()), value->size());
}

bool DecodeBytes(pb_istream_t* stream, const pb_field_t* /*field*/, void** arg) {
    auto* out = static_cast<Bytes*>(*arg);
    const size_t length = stream->bytes_left;
    if (length > kMaxFieldBytes) return false;
    out->resize(length);
    return length == 0 || pb_read(stream, out->data(), length);
}

bool EncodeBytes(pb_ostream_t* stream, const pb_field_t* field, void* const* arg) {
    const auto* value = static_cast<const Bytes*>(*arg);
    if (value->empty()) return true;
    if (!pb_encode_tag_for_field(stream, field)) return false;
    return pb_encode_string(stream, value->data(), value->size());
}

void BindString(pb_callback_t& callback, std::string& value) {
    callback.funcs.decode = DecodeString;
    callback.arg = &value;
}

void BindStringEncode(pb_callback_t& callback, const std::string& value) {
    callback.funcs.encode = EncodeString;
    callback.arg = const_cast<std::string*>(&value);
}

void BindBytes(pb_callback_t& callback, Bytes& value) {
    callback.funcs.decode = DecodeBytes;
    callback.arg = &value;
}

void BindBytesEncode(pb_callback_t& callback, const Bytes& value) {
    callback.funcs.encode = EncodeBytes;
    callback.arg = const_cast<Bytes*>(&value);
}

template <typename Msg, typename BindFn>
bool DecodeMessage(const Bytes& bytes, const pb_msgdesc_t* fields, Msg& msg, BindFn&& bind) {
    msg = {};
    bind(msg);
    pb_istream_t stream = pb_istream_from_buffer(
        bytes.empty() ? nullptr : bytes.data(), bytes.size());
    return pb_decode(&stream, fields, &msg);
}

template <typename Msg, typename BindFn>
Bytes EncodeMessage(const pb_msgdesc_t* fields, Msg& msg, BindFn&& bind) {
    bind(msg);
    size_t size = 0;
    if (!pb_get_encoded_size(&size, fields, &msg)) return {};
    Bytes out(size);
    pb_ostream_t stream = pb_ostream_from_buffer(out.empty() ? nullptr : out.data(), out.size());
    if (!pb_encode(&stream, fields, &msg)) return {};
    out.resize(stream.bytes_written);
    return out;
}

bool DecodeDevice(pb_istream_t* stream, const pb_field_t* /*field*/, void** arg) {
    auto* devices = static_cast<std::vector<Device>*>(*arg);
    if (devices->size() >= 16) return false;
    Device device;
    axonkey_service_v1_Device msg = axonkey_service_v1_Device_init_zero;
    BindString(msg.instance_id, device.instanceId);
    BindString(msg.endpoint_path, device.endpointPath);
    BindString(msg.description_name, device.descriptionName);
    if (!pb_decode(stream, axonkey_service_v1_Device_fields, &msg)) return false;
    device.driverMounted = msg.driver_mounted;
    device.inputBlocked = msg.input_blocked;
    device.dataForwardEnabled = msg.data_forward_enabled;
    device.connected = msg.connected;
    if (msg.has_battery_level && msg.battery_level <= 100)
        device.batteryLevel = static_cast<std::uint8_t>(msg.battery_level);
    devices->push_back(std::move(device));
    return true;
}

bool EncodeDevice(pb_ostream_t* stream, const pb_field_t* field, void* const* arg) {
    const auto* devices = static_cast<const std::vector<Device>*>(*arg);
    for (const auto& device : *devices) {
        if (!pb_encode_tag_for_field(stream, field)) return false;
        axonkey_service_v1_Device msg = axonkey_service_v1_Device_init_zero;
        BindStringEncode(msg.instance_id, device.instanceId);
        BindStringEncode(msg.endpoint_path, device.endpointPath);
        msg.driver_mounted = device.driverMounted;
        msg.input_blocked = device.inputBlocked;
        msg.data_forward_enabled = device.dataForwardEnabled;
        msg.connected = device.connected;
        if (device.batteryLevel.has_value()) {
            msg.has_battery_level = true;
            msg.battery_level = *device.batteryLevel;
        }
        BindStringEncode(msg.description_name, device.descriptionName);
        if (!pb_encode_submessage(stream, axonkey_service_v1_Device_fields, &msg)) return false;
    }
    return true;
}

} // namespace

bool Parse(const Bytes& bytes, Request& value) {
    value = {};
    axonkey_service_v1_Request msg = axonkey_service_v1_Request_init_zero;
    if (!DecodeMessage(bytes, axonkey_service_v1_Request_fields, msg, [&](auto& m) {
            BindString(m.method, value.method);
            BindBytes(m.payload, value.payload);
        })) return false;
    value.requestId = msg.request_id;
    return true;
}

bool Parse(const Bytes& bytes, Response& value) {
    value = {};
    axonkey_service_v1_Response msg = axonkey_service_v1_Response_init_zero;
    if (!DecodeMessage(bytes, axonkey_service_v1_Response_fields, msg, [&](auto& m) {
            BindString(m.error, value.error);
            BindBytes(m.payload, value.payload);
        })) return false;
    value.requestId = msg.request_id;
    value.success = msg.success;
    return true;
}

bool Parse(const Bytes& bytes, SetAudioGain& value) {
    value = {};
    axonkey_service_v1_SetAudioGainRequest msg = axonkey_service_v1_SetAudioGainRequest_init_zero;
    pb_istream_t stream = pb_istream_from_buffer(
        bytes.empty() ? nullptr : bytes.data(), bytes.size());
    if (!pb_decode(&stream, axonkey_service_v1_SetAudioGainRequest_fields, &msg)) return false;
    value.gainDb = msg.gain_db;
    return true;
}

bool Parse(const Bytes& bytes, Subscribe& value) {
    value = {};
    axonkey_service_v1_SubscribeRequest msg = axonkey_service_v1_SubscribeRequest_init_zero;
    pb_istream_t stream = pb_istream_from_buffer(
        bytes.empty() ? nullptr : bytes.data(), bytes.size());
    if (!pb_decode(&stream, axonkey_service_v1_SubscribeRequest_fields, &msg)) return false;
    value.keyboard = msg.keyboard;
    value.audioLevel = msg.audio_level;
    value.voiceStatus = msg.voice_status;
    return true;
}

bool Parse(const Bytes& bytes, DeviceList& value) {
    value = {};
    axonkey_service_v1_DeviceList msg = axonkey_service_v1_DeviceList_init_zero;
    msg.devices.funcs.decode = DecodeDevice;
    msg.devices.arg = &value.devices;
    pb_istream_t stream = pb_istream_from_buffer(
        bytes.empty() ? nullptr : bytes.data(), bytes.size());
    return pb_decode(&stream, axonkey_service_v1_DeviceList_fields, &msg);
}

bool Parse(const Bytes& bytes, VoiceStatus& value) {
    value = {};
    axonkey_service_v1_VoiceStatus msg = axonkey_service_v1_VoiceStatus_init_zero;
    if (!DecodeMessage(bytes, axonkey_service_v1_VoiceStatus_fields, msg, [&](auto& m) {
            BindString(m.state, value.state);
            BindString(m.device_instance_id, value.deviceInstanceId);
        })) return false;
    value.connected = msg.connected;
    value.active = msg.active;
    value.microphoneOpen = msg.microphone_open;
    value.protocolVersion = msg.protocol_version;
    value.sessionId = msg.session_id;
    return true;
}

bool Parse(const Bytes& bytes, AudioLevel& value) {
    value = {};
    axonkey_service_v1_AudioLevel msg = axonkey_service_v1_AudioLevel_init_zero;
    pb_istream_t stream = pb_istream_from_buffer(
        bytes.empty() ? nullptr : bytes.data(), bytes.size());
    if (!pb_decode(&stream, axonkey_service_v1_AudioLevel_fields, &msg)) return false;
    value.peak = msg.peak;
    value.rms = msg.rms;
    value.timestampMs = msg.timestamp_ms;
    return true;
}

bool Parse(const Bytes& bytes, KeyboardEvent& value) {
    value = {};
    axonkey_service_v1_KeyboardEvent msg = axonkey_service_v1_KeyboardEvent_init_zero;
    if (!DecodeMessage(bytes, axonkey_service_v1_KeyboardEvent_fields, msg, [&](auto& m) {
            BindString(m.device_instance_id, value.deviceInstanceId);
            BindBytes(m.report, value.report);
        })) return false;
    value.timestampMs = msg.timestamp_ms;
    return true;
}

bool Parse(const Bytes& bytes, EventEnvelope& value) {
    value = {};
    axonkey_service_v1_Event msg = axonkey_service_v1_Event_init_zero;
    return DecodeMessage(bytes, axonkey_service_v1_Event_fields, msg, [&](auto& m) {
        BindString(m.type, value.type);
        BindBytes(m.payload, value.payload);
    });
}

bool Parse(const Bytes& bytes, ServiceInfo& value) {
    value = {};
    axonkey_service_v1_ServiceInfo msg = axonkey_service_v1_ServiceInfo_init_zero;
    return DecodeMessage(bytes, axonkey_service_v1_ServiceInfo_fields, msg, [&](auto& m) {
        BindString(m.name, value.name);
        BindString(m.version, value.version);
        BindString(m.protocol_version, value.protocolVersion);
        BindString(m.pipe_name, value.pipeName);
    });
}

bool Parse(const Bytes& bytes, ServiceStatus& value) {
    value = {};
    axonkey_service_v1_ServiceStatus msg = axonkey_service_v1_ServiceStatus_init_zero;
    pb_istream_t stream = pb_istream_from_buffer(
        bytes.empty() ? nullptr : bytes.data(), bytes.size());
    if (!pb_decode(&stream, axonkey_service_v1_ServiceStatus_fields, &msg)) return false;
    value.enabled = msg.enabled;
    return true;
}

bool Parse(const Bytes& bytes, SetServiceStatus& value) {
    value = {};
    axonkey_service_v1_SetServiceStatusRequest msg = axonkey_service_v1_SetServiceStatusRequest_init_zero;
    pb_istream_t stream = pb_istream_from_buffer(
        bytes.empty() ? nullptr : bytes.data(), bytes.size());
    if (!pb_decode(&stream, axonkey_service_v1_SetServiceStatusRequest_fields, &msg)) return false;
    value.enabled = msg.enabled;
    return true;
}

bool Parse(const Bytes& bytes, OperationResult& value) {
    value = {};
    axonkey_service_v1_OperationResult msg = axonkey_service_v1_OperationResult_init_zero;
    if (!DecodeMessage(bytes, axonkey_service_v1_OperationResult_fields, msg, [&](auto& m) {
            BindString(m.error, value.error);
        })) return false;
    value.success = msg.success;
    return true;
}

Bytes Serialize(const Request& value) {
    axonkey_service_v1_Request msg = axonkey_service_v1_Request_init_zero;
    msg.request_id = value.requestId;
    return EncodeMessage(axonkey_service_v1_Request_fields, msg, [&](auto& m) {
        BindStringEncode(m.method, value.method);
        BindBytesEncode(m.payload, value.payload);
    });
}

Bytes Serialize(const Response& value) {
    axonkey_service_v1_Response msg = axonkey_service_v1_Response_init_zero;
    msg.request_id = value.requestId;
    msg.success = value.success;
    return EncodeMessage(axonkey_service_v1_Response_fields, msg, [&](auto& m) {
        BindStringEncode(m.error, value.error);
        BindBytesEncode(m.payload, value.payload);
    });
}

Bytes Serialize(const EventEnvelope& value) {
    axonkey_service_v1_Event msg = axonkey_service_v1_Event_init_zero;
    return EncodeMessage(axonkey_service_v1_Event_fields, msg, [&](auto& m) {
        BindStringEncode(m.type, value.type);
        BindBytesEncode(m.payload, value.payload);
    });
}

Bytes Serialize(const ServiceInfo& value) {
    axonkey_service_v1_ServiceInfo msg = axonkey_service_v1_ServiceInfo_init_zero;
    return EncodeMessage(axonkey_service_v1_ServiceInfo_fields, msg, [&](auto& m) {
        BindStringEncode(m.name, value.name);
        BindStringEncode(m.version, value.version);
        BindStringEncode(m.protocol_version, value.protocolVersion);
        BindStringEncode(m.pipe_name, value.pipeName);
    });
}

Bytes Serialize(const ServiceStatus& value) {
    axonkey_service_v1_ServiceStatus msg = axonkey_service_v1_ServiceStatus_init_zero;
    msg.enabled = value.enabled;
    size_t size = 0;
    if (!pb_get_encoded_size(&size, axonkey_service_v1_ServiceStatus_fields, &msg)) return {};
    Bytes out(size);
    pb_ostream_t stream = pb_ostream_from_buffer(out.empty() ? nullptr : out.data(), out.size());
    if (!pb_encode(&stream, axonkey_service_v1_ServiceStatus_fields, &msg)) return {};
    out.resize(stream.bytes_written);
    return out;
}

Bytes Serialize(const SetServiceStatus& value) {
    axonkey_service_v1_SetServiceStatusRequest msg = axonkey_service_v1_SetServiceStatusRequest_init_zero;
    msg.enabled = value.enabled;
    size_t size = 0;
    if (!pb_get_encoded_size(&size, axonkey_service_v1_SetServiceStatusRequest_fields, &msg)) return {};
    Bytes out(size);
    pb_ostream_t stream = pb_ostream_from_buffer(out.empty() ? nullptr : out.data(), out.size());
    if (!pb_encode(&stream, axonkey_service_v1_SetServiceStatusRequest_fields, &msg)) return {};
    out.resize(stream.bytes_written);
    return out;
}

Bytes Serialize(const OperationResult& value) {
    axonkey_service_v1_OperationResult msg = axonkey_service_v1_OperationResult_init_zero;
    msg.success = value.success;
    return EncodeMessage(axonkey_service_v1_OperationResult_fields, msg, [&](auto& m) {
        BindStringEncode(m.error, value.error);
    });
}

Bytes Serialize(const DeviceList& value) {
    axonkey_service_v1_DeviceList msg = axonkey_service_v1_DeviceList_init_zero;
    msg.devices.funcs.encode = EncodeDevice;
    msg.devices.arg = const_cast<std::vector<Device>*>(&value.devices);
    size_t size = 0;
    if (!pb_get_encoded_size(&size, axonkey_service_v1_DeviceList_fields, &msg)) return {};
    Bytes out(size);
    pb_ostream_t stream = pb_ostream_from_buffer(out.empty() ? nullptr : out.data(), out.size());
    if (!pb_encode(&stream, axonkey_service_v1_DeviceList_fields, &msg)) return {};
    out.resize(stream.bytes_written);
    return out;
}

Bytes Serialize(const VoiceStatus& value) {
    axonkey_service_v1_VoiceStatus msg = axonkey_service_v1_VoiceStatus_init_zero;
    msg.connected = value.connected;
    msg.active = value.active;
    msg.microphone_open = value.microphoneOpen;
    msg.protocol_version = value.protocolVersion;
    msg.session_id = value.sessionId;
    return EncodeMessage(axonkey_service_v1_VoiceStatus_fields, msg, [&](auto& m) {
        BindStringEncode(m.state, value.state);
        BindStringEncode(m.device_instance_id, value.deviceInstanceId);
    });
}

Bytes Serialize(const AudioLevel& value) {
    axonkey_service_v1_AudioLevel msg = axonkey_service_v1_AudioLevel_init_zero;
    msg.peak = value.peak;
    msg.rms = value.rms;
    msg.timestamp_ms = value.timestampMs;
    size_t size = 0;
    if (!pb_get_encoded_size(&size, axonkey_service_v1_AudioLevel_fields, &msg)) return {};
    Bytes out(size);
    pb_ostream_t stream = pb_ostream_from_buffer(out.empty() ? nullptr : out.data(), out.size());
    if (!pb_encode(&stream, axonkey_service_v1_AudioLevel_fields, &msg)) return {};
    out.resize(stream.bytes_written);
    return out;
}

Bytes Serialize(const KeyboardEvent& value) {
    axonkey_service_v1_KeyboardEvent msg = axonkey_service_v1_KeyboardEvent_init_zero;
    msg.timestamp_ms = value.timestampMs;
    return EncodeMessage(axonkey_service_v1_KeyboardEvent_fields, msg, [&](auto& m) {
        BindStringEncode(m.device_instance_id, value.deviceInstanceId);
        BindBytesEncode(m.report, value.report);
    });
}

Bytes Serialize(const SetAudioGain& value) {
    axonkey_service_v1_SetAudioGainRequest msg = axonkey_service_v1_SetAudioGainRequest_init_zero;
    msg.gain_db = value.gainDb;
    size_t size = 0;
    if (!pb_get_encoded_size(&size, axonkey_service_v1_SetAudioGainRequest_fields, &msg)) return {};
    Bytes out(size);
    pb_ostream_t stream = pb_ostream_from_buffer(out.empty() ? nullptr : out.data(), out.size());
    if (!pb_encode(&stream, axonkey_service_v1_SetAudioGainRequest_fields, &msg)) return {};
    out.resize(stream.bytes_written);
    return out;
}

Bytes Serialize(const Subscribe& value) {
    axonkey_service_v1_SubscribeRequest msg = axonkey_service_v1_SubscribeRequest_init_zero;
    msg.keyboard = value.keyboard;
    msg.audio_level = value.audioLevel;
    msg.voice_status = value.voiceStatus;
    size_t size = 0;
    if (!pb_get_encoded_size(&size, axonkey_service_v1_SubscribeRequest_fields, &msg)) return {};
    Bytes out(size);
    pb_ostream_t stream = pb_ostream_from_buffer(out.empty() ? nullptr : out.data(), out.size());
    if (!pb_encode(&stream, axonkey_service_v1_SubscribeRequest_fields, &msg)) return {};
    out.resize(stream.bytes_written);
    return out;
}

} // namespace axonkey::rpc
