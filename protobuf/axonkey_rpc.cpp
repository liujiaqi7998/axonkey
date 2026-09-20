#include "axonkey_rpc.h"
#include <cstring>

namespace axonkey::rpc {
namespace {
void Varint(Bytes& out, std::uint64_t value) { while (value > 0x7f) { out.push_back(static_cast<std::uint8_t>(value) | 0x80); value >>= 7; } out.push_back(static_cast<std::uint8_t>(value)); }
void Key(Bytes& out, std::uint32_t field, std::uint8_t wire) { Varint(out, (static_cast<std::uint64_t>(field) << 3) | wire); }
void U64(Bytes& out, std::uint32_t field, std::uint64_t value) { Key(out, field, 0); Varint(out, value); }
void Bool(Bytes& out, std::uint32_t field, bool value) { U64(out, field, value ? 1 : 0); }
void I32(Bytes& out, std::uint32_t field, std::int32_t value) { U64(out, field, static_cast<std::uint64_t>(static_cast<std::int64_t>(value))); }
void Raw(Bytes& out, std::uint32_t field, const std::uint8_t* data, size_t size) { Key(out, field, 2); Varint(out, size); out.insert(out.end(), data, data + size); }
void Str(Bytes& out, std::uint32_t field, const std::string& value) { Raw(out, field, reinterpret_cast<const std::uint8_t*>(value.data()), value.size()); }
void Msg(Bytes& out, std::uint32_t field, const Bytes& value) { Raw(out, field, value.data(), value.size()); }
void F32(Bytes& out, std::uint32_t field, float value) { Key(out, field, 5); std::uint32_t bits; std::memcpy(&bits, &value, sizeof(bits)); for (int i = 0; i < 4; ++i) out.push_back(static_cast<std::uint8_t>(bits >> (8 * i))); }

class Reader {
public:
    explicit Reader(const Bytes& bytes) : data_(bytes.data()), size_(bytes.size()) {}
    bool Next(std::uint32_t& field, std::uint8_t& wire) { std::uint64_t key; if (!Var(key)) return false; field = static_cast<std::uint32_t>(key >> 3); wire = static_cast<std::uint8_t>(key & 7); return field != 0; }
    bool Var(std::uint64_t& value) { value = 0; for (unsigned shift = 0; shift < 64; shift += 7) { if (pos_ >= size_) return false; auto byte = data_[pos_++]; value |= static_cast<std::uint64_t>(byte & 0x7f) << shift; if (!(byte & 0x80)) return true; } return false; }
    bool Length(Bytes& value) { std::uint64_t length; if (!Var(length) || length > size_ - pos_) return false; value.assign(data_ + pos_, data_ + pos_ + length); pos_ += static_cast<size_t>(length); return true; }
    bool String(std::string& value) { Bytes raw; if (!Length(raw)) return false; value.assign(reinterpret_cast<const char*>(raw.data()), raw.size()); return true; }
    bool Skip(std::uint8_t wire) { switch (wire) { case 0: { std::uint64_t ignored; return Var(ignored); } case 1: if (size_ - pos_ < 8) return false; pos_ += 8; return true; case 2: { Bytes ignored; return Length(ignored); } case 5: if (size_ - pos_ < 4) return false; pos_ += 4; return true; default: return false; } }
    bool Float(float& value) { if (size_ - pos_ < 4) return false; std::uint32_t bits = data_[pos_] | (static_cast<std::uint32_t>(data_[pos_ + 1]) << 8) | (static_cast<std::uint32_t>(data_[pos_ + 2]) << 16) | (static_cast<std::uint32_t>(data_[pos_ + 3]) << 24); pos_ += 4; std::memcpy(&value, &bits, sizeof(value)); return true; }
private: const std::uint8_t* data_; size_t size_, pos_ = 0;
};

template<class Fn> bool Read(const Bytes& bytes, Fn&& fn) { Reader reader(bytes); std::uint32_t field; std::uint8_t wire; while (reader.Next(field, wire)) if (!fn(reader, field, wire)) return false; return true; }
bool ReadText(Reader& r, std::uint8_t wire, std::string& value) { return wire == 2 && r.String(value); }
bool ReadBytes(Reader& r, std::uint8_t wire, Bytes& value) { return wire == 2 && r.Length(value); }
bool ReadVar(Reader& r, std::uint8_t wire, std::uint64_t& value) { return wire == 0 && r.Var(value); }
}

bool Parse(const Bytes& bytes, Request& v) { return Read(bytes, [&](Reader& r, auto f, auto w) { if (f == 1) { std::uint64_t x; if (!ReadVar(r,w,x)) return false; v.requestId=x; return true; } if (f == 2) return ReadText(r,w,v.method); if (f == 3) return ReadBytes(r,w,v.payload); return r.Skip(w); }); }
bool Parse(const Bytes& bytes, SetAudioGain& v) { return Read(bytes, [&](Reader& r, auto f, auto w) { if (f == 1) { std::uint64_t x; if (!ReadVar(r,w,x)) return false; v.gainDb=static_cast<std::int32_t>(x); return true; } return r.Skip(w); }); }
bool Parse(const Bytes& bytes, Subscribe& v) { return Read(bytes, [&](Reader& r, auto f, auto w) { if (f >= 1 && f <= 3) { std::uint64_t x; if (!ReadVar(r,w,x)) return false; bool b=x != 0; if (f==1)v.keyboard=b; if(f==2)v.audioLevel=b; if(f==3)v.voiceStatus=b; return true; } return r.Skip(w); }); }
bool Parse(const Bytes& bytes, DeviceList& v) { return Read(bytes, [&](Reader& r, auto f, auto w) { if (f != 1) return r.Skip(w); Bytes raw; if (!ReadBytes(r,w,raw)) return false; Device d; if (!Read(raw,[&](Reader& rr,auto ff,auto ww){ if(ff==1)return ReadText(rr,ww,d.instanceId); if(ff==2)return ReadText(rr,ww,d.endpointPath); if(ff>=3&&ff<=6){std::uint64_t x;if(!ReadVar(rr,ww,x))return false; if(ff==3)d.driverMounted=x; if(ff==4)d.inputBlocked=x; if(ff==5)d.dataForwardEnabled=x; if(ff==6)d.connected=x; return true;} return rr.Skip(ww); })) return false; v.devices.push_back(std::move(d)); return true; }); }
bool Parse(const Bytes& bytes, VoiceStatus& v) { return Read(bytes, [&](Reader& r,auto f,auto w){if(f==1)return ReadText(r,w,v.state);if(f==2)return ReadText(r,w,v.deviceInstanceId);if(f>=3&&f<=7){std::uint64_t x;if(!ReadVar(r,w,x))return false;if(f==3)v.connected=x;if(f==4)v.active=x;if(f==5)v.microphoneOpen=x;if(f==6)v.protocolVersion=static_cast<std::uint32_t>(x);if(f==7)v.sessionId=static_cast<std::uint32_t>(x);return true;}return r.Skip(w);}); }
bool Parse(const Bytes& bytes, AudioLevel& v) { return Read(bytes, [&](Reader& r,auto f,auto w){if(f==1)return w==5&&r.Float(v.peak);if(f==2)return w==5&&r.Float(v.rms);if(f==3){std::uint64_t x;if(!ReadVar(r,w,x))return false;v.timestampMs=x;return true;}return r.Skip(w);}); }
bool Parse(const Bytes& bytes, KeyboardEvent& v) { return Read(bytes, [&](Reader& r,auto f,auto w){if(f==1)return ReadText(r,w,v.deviceInstanceId);if(f==2)return ReadBytes(r,w,v.report);if(f==3){std::uint64_t x;if(!ReadVar(r,w,x))return false;v.timestampMs=x;return true;}return r.Skip(w);}); }

Bytes Serialize(const Request& v){Bytes o;U64(o,1,v.requestId);Str(o,2,v.method);if(!v.payload.empty())Msg(o,3,v.payload);return o;}
Bytes Serialize(const Response& v){Bytes o;U64(o,1,v.requestId);Bool(o,2,v.success);if(!v.error.empty())Str(o,3,v.error);if(!v.payload.empty())Msg(o,4,v.payload);return o;}
Bytes Serialize(const EventEnvelope& v){Bytes o;Str(o,1,v.type);if(!v.payload.empty())Msg(o,2,v.payload);return o;}
Bytes Serialize(const ServiceInfo& v){Bytes o;Str(o,1,v.name);Str(o,2,v.version);Str(o,3,v.protocolVersion);Str(o,4,v.pipeName);return o;}
Bytes Serialize(const OperationResult& v){Bytes o;Bool(o,1,v.success);if(!v.error.empty())Str(o,2,v.error);return o;}
Bytes Serialize(const DeviceList& v){Bytes o;for(const auto& d:v.devices){Bytes x;Str(x,1,d.instanceId);Str(x,2,d.endpointPath);Bool(x,3,d.driverMounted);Bool(x,4,d.inputBlocked);Bool(x,5,d.dataForwardEnabled);Bool(x,6,d.connected);Msg(o,1,x);}return o;}
Bytes Serialize(const VoiceStatus& v){Bytes o;Str(o,1,v.state);Str(o,2,v.deviceInstanceId);Bool(o,3,v.connected);Bool(o,4,v.active);Bool(o,5,v.microphoneOpen);U64(o,6,v.protocolVersion);U64(o,7,v.sessionId);return o;}
Bytes Serialize(const AudioLevel& v){Bytes o;F32(o,1,v.peak);F32(o,2,v.rms);U64(o,3,v.timestampMs);return o;}
Bytes Serialize(const KeyboardEvent& v){Bytes o;Str(o,1,v.deviceInstanceId);if(!v.report.empty())Raw(o,2,v.report.data(),v.report.size());U64(o,3,v.timestampMs);return o;}
}
