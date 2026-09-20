//! Client for the native service's local protobuf endpoint.
//!
//! This deliberately uses only the protobuf wire primitives needed by the
//! public contract. It keeps the desktop binary independent from a generated
//! C++ runtime while remaining interoperable with `protobuf/axonkey_service.proto`.

use serde::Serialize;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;

const PIPE: &str = r"\\.\pipe\AxonkeyService.v1";

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo { pub name: String, pub version: String, pub protocol_version: String, pub pipe_name: String }
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device { pub instance_id: String, pub endpoint_path: String, pub driver_mounted: bool, pub input_blocked: bool, pub data_forward_enabled: bool, pub connected: bool }
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceStatus { pub state: String, pub device_instance_id: String, pub connected: bool, pub active: bool, pub microphone_open: bool, pub protocol_version: u32, pub session_id: u32 }
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioLevel { pub peak: f32, pub rms: f32, pub timestamp_ms: u64 }
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardEvent { pub device_instance_id: String, pub report: Vec<u8>, pub timestamp_ms: u64 }

static EVENTS_RUNNING: AtomicBool = AtomicBool::new(false);

fn varint(mut value: u64, out: &mut Vec<u8>) { while value >= 0x80 { out.push(value as u8 | 0x80); value >>= 7; } out.push(value as u8); }
fn key(field: u32, wire: u8, out: &mut Vec<u8>) { varint((field as u64) << 3 | wire as u64, out); }
fn string(field: u32, value: &str, out: &mut Vec<u8>) { key(field, 2, out); varint(value.len() as u64, out); out.extend_from_slice(value.as_bytes()); }
fn bytes(field: u32, value: &[u8], out: &mut Vec<u8>) { key(field, 2, out); varint(value.len() as u64, out); out.extend_from_slice(value); }
fn request(id: u64, method: &str, payload: Vec<u8>) -> Vec<u8> { let mut out = Vec::new(); key(1, 0, &mut out); varint(id, &mut out); string(2, method, &mut out); if !payload.is_empty() { bytes(3, &payload, &mut out); } out }

struct Reader<'a> { data: &'a [u8], pos: usize }
impl<'a> Reader<'a> {
    fn varint(&mut self) -> Option<u64> { let mut value = 0; for shift in (0..64).step_by(7) { let byte = *self.data.get(self.pos)?; self.pos += 1; value |= u64::from(byte & 0x7f) << shift; if byte & 0x80 == 0 { return Some(value); } } None }
    fn field(&mut self) -> Option<(u32, u8)> { let key = self.varint()?; Some(((key >> 3) as u32, (key & 7) as u8)) }
    fn raw(&mut self) -> Option<&'a [u8]> { let len = self.varint()? as usize; let end = self.pos.checked_add(len)?; let value = self.data.get(self.pos..end)?; self.pos = end; Some(value) }
    fn string(&mut self) -> Option<String> { String::from_utf8(self.raw()?.to_vec()).ok() }
    fn skip(&mut self, wire: u8) -> Option<()> { match wire { 0 => self.varint().map(|_| ()), 1 => { self.pos += 8; (self.pos <= self.data.len()).then_some(()) }, 2 => self.raw().map(|_| ()), 5 => { self.pos += 4; (self.pos <= self.data.len()).then_some(()) }, _ => None } }
}

fn parse_response(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = Reader { data: bytes, pos: 0 }; let mut ok = false; let mut error = None; let mut payload = Vec::new();
    while let Some((field, wire)) = reader.field() { match field { 2 => ok = reader.varint().ok_or("invalid response")? != 0, 3 => error = Some(reader.string().ok_or("invalid error")?), 4 => payload = reader.raw().ok_or("invalid payload")?.to_vec(), _ => reader.skip(wire).ok_or("invalid response field")?, } }
    if ok { Ok(payload) } else { Err(error.unwrap_or_else(|| "Axonkey service request failed".into())) }
}

fn call(method: &str, payload: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut pipe = std::fs::OpenOptions::new().read(true).write(true).open(PIPE).map_err(|e| format!("无法连接 AxonkeyService：{e}"))?;
    let frame = request(1, method, payload); pipe.write_all(&(frame.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?; pipe.write_all(&frame).map_err(|e| e.to_string())?; pipe.flush().map_err(|e| e.to_string())?;
    let mut size = [0; 4]; pipe.read_exact(&mut size).map_err(|e| e.to_string())?; let length = u32::from_le_bytes(size) as usize; if length > 1024 * 1024 { return Err("AxonkeyService 返回的数据过大".into()); }
    let mut response = vec![0; length]; pipe.read_exact(&mut response).map_err(|e| e.to_string())?; parse_response(&response)
}

pub fn service_info() -> Result<ServiceInfo, String> { let bytes = call("GetServiceInfo", Vec::new())?; let mut value = ServiceInfo::default(); parse_message(&bytes, |field, wire, reader| { match field { 1 => value.name = reader.string().ok_or("invalid name")?, 2 => value.version = reader.string().ok_or("invalid version")?, 3 => value.protocol_version = reader.string().ok_or("invalid protocol")?, 4 => value.pipe_name = reader.string().ok_or("invalid pipe")?, _ => reader.skip(wire).ok_or("invalid info field")?, }; Ok(()) })?; Ok(value) }
pub fn set_audio_gain(gain_db: i32) -> Result<(), String> { let mut payload = Vec::new(); key(1, 0, &mut payload); varint(gain_db as i64 as u64, &mut payload); call("SetAudioGain", payload).map(|_| ()) }
pub fn devices() -> Result<Vec<Device>, String> { let bytes = call("GetDevices", Vec::new())?; let mut result = Vec::new(); parse_message(&bytes, |field, wire, reader| { if field != 1 { reader.skip(wire).ok_or("invalid device list")?; return Ok(()); } let raw = reader.raw().ok_or("invalid device")?; let mut value = Device::default(); parse_message(raw, |f,w,r| { match f { 1 => value.instance_id=r.string().ok_or("invalid instance")?, 2 => value.endpoint_path=r.string().ok_or("invalid endpoint")?, 3..=6 => { let b=r.varint().ok_or("invalid device flag")? != 0; match f {3=>value.driver_mounted=b,4=>value.input_blocked=b,5=>value.data_forward_enabled=b,6=>value.connected=b,_=>{}} }, _=>r.skip(w).ok_or("invalid device field")?,}; Ok(()) })?; result.push(value); Ok(()) })?; Ok(result) }
pub fn voice_status() -> Result<VoiceStatus, String> { let bytes=call("GetVoiceStatus",Vec::new())?; let mut v=VoiceStatus::default(); parse_message(&bytes, |f,w,r| {match f {1=>v.state=r.string().ok_or("invalid state")?,2=>v.device_instance_id=r.string().ok_or("invalid device")?,3=>v.connected=r.varint().ok_or("invalid connected")?!=0,4=>v.active=r.varint().ok_or("invalid active")?!=0,5=>v.microphone_open=r.varint().ok_or("invalid microphone")?!=0,6=>v.protocol_version=r.varint().ok_or("invalid protocol")? as u32,7=>v.session_id=r.varint().ok_or("invalid session")? as u32,_=>r.skip(w).ok_or("invalid voice field")?,};Ok(())})?;Ok(v)}
pub fn audio_level() -> Result<AudioLevel, String> { let bytes=call("GetAudioLevel",Vec::new())?; let mut v=AudioLevel::default(); parse_message(&bytes, |f,w,r| {match f {1=>v.peak=r.f32().ok_or("invalid peak")?,2=>v.rms=r.f32().ok_or("invalid rms")?,3=>v.timestamp_ms=r.varint().ok_or("invalid timestamp")?,_=>r.skip(w).ok_or("invalid level field")?,};Ok(())})?;Ok(v)}

fn bool_field(field: u32, value: bool, out: &mut Vec<u8>) { key(field, 0, out); varint(u64::from(value), out); }

/// Starts a long-lived subscription and forwards native service events through
/// Tauri events. Only one subscription is kept per desktop process.
pub fn subscribe_events(app: tauri::AppHandle) -> Result<(), String> {
    if EVENTS_RUNNING.swap(true, Ordering::AcqRel) { return Ok(()); }
    let result = (|| {
        let mut pipe = std::fs::OpenOptions::new().read(true).write(true).open(PIPE)
            .map_err(|e| format!("无法连接 AxonkeyService：{e}"))?;
        let mut subscription = Vec::new();
        bool_field(1, true, &mut subscription); bool_field(2, true, &mut subscription); bool_field(3, true, &mut subscription);
        let frame = request(1, "Subscribe", subscription);
        pipe.write_all(&(frame.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
        pipe.write_all(&frame).map_err(|e| e.to_string())?; pipe.flush().map_err(|e| e.to_string())?;
        let mut size = [0; 4]; pipe.read_exact(&mut size).map_err(|e| e.to_string())?;
        let length = u32::from_le_bytes(size) as usize; if length > 1024 * 1024 { return Err("AxonkeyService 返回的数据过大".into()); }
        let mut response = vec![0; length]; pipe.read_exact(&mut response).map_err(|e| e.to_string())?; parse_response(&response)?;
        std::thread::Builder::new().name("Axonkey service protobuf events".into()).spawn(move || {
            loop {
                let mut size = [0; 4]; if pipe.read_exact(&mut size).is_err() { break; }
                let length = u32::from_le_bytes(size) as usize; if length > 1024 * 1024 { break; }
                let mut frame = vec![0; length]; if pipe.read_exact(&mut frame).is_err() { break; }
                let mut event_type = String::new(); let mut payload = Vec::new();
                if parse_message(&frame, |field, wire, reader| { match field { 1 => event_type = reader.string().ok_or("invalid event type")?, 2 => payload = reader.raw().ok_or("invalid event payload")?.to_vec(), _ => reader.skip(wire).ok_or("invalid event field")?, }; Ok(()) }).is_err() { break; }
                match event_type.as_str() {
                    "keyboard" => if let Ok(event) = parse_keyboard(&payload) { let _ = app.emit("axonkey-service-keyboard", event); },
                    "audio_level" => if let Ok(event) = parse_audio(&payload) { let _ = app.emit("axonkey-service-audio-level", event); },
                    "voice_status" => if let Ok(event) = parse_voice(&payload) { let _ = app.emit("axonkey-service-voice-status", event); },
                    _ => {}
                }
            }
            EVENTS_RUNNING.store(false, Ordering::Release);
        }).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() { EVENTS_RUNNING.store(false, Ordering::Release); }
    result
}

fn parse_keyboard(bytes: &[u8]) -> Result<KeyboardEvent, String> { let mut v=KeyboardEvent::default(); parse_message(bytes, |f,w,r| {match f {1=>v.device_instance_id=r.string().ok_or("invalid device")?,2=>v.report=r.raw().ok_or("invalid report")?.to_vec(),3=>v.timestamp_ms=r.varint().ok_or("invalid timestamp")?,_=>r.skip(w).ok_or("invalid keyboard field")?,};Ok(())})?;Ok(v) }
fn parse_audio(bytes: &[u8]) -> Result<AudioLevel, String> { let mut v=AudioLevel::default(); parse_message(bytes, |f,w,r| {match f {1=>v.peak=r.f32().ok_or("invalid peak")?,2=>v.rms=r.f32().ok_or("invalid rms")?,3=>v.timestamp_ms=r.varint().ok_or("invalid timestamp")?,_=>r.skip(w).ok_or("invalid audio field")?,};Ok(())})?;Ok(v) }
fn parse_voice(bytes: &[u8]) -> Result<VoiceStatus, String> { let mut v=VoiceStatus::default(); parse_message(bytes, |f,w,r| {match f {1=>v.state=r.string().ok_or("invalid state")?,2=>v.device_instance_id=r.string().ok_or("invalid device")?,3=>v.connected=r.varint().ok_or("invalid connected")?!=0,4=>v.active=r.varint().ok_or("invalid active")?!=0,5=>v.microphone_open=r.varint().ok_or("invalid microphone")?!=0,6=>v.protocol_version=r.varint().ok_or("invalid protocol")? as u32,7=>v.session_id=r.varint().ok_or("invalid session")? as u32,_=>r.skip(w).ok_or("invalid voice field")?,};Ok(())})?;Ok(v) }

fn parse_message<F>(bytes: &[u8], mut callback: F) -> Result<(), String> where F: FnMut(u32, u8, &mut Reader<'_>) -> Result<(), String> { let mut reader=Reader{data:bytes,pos:0}; while let Some((field,wire))=reader.field(){callback(field,wire,&mut reader)?;} Ok(()) }
trait FloatReader { fn f32(&mut self) -> Option<f32>; }
impl<'a> FloatReader for Reader<'a> { fn f32(&mut self)->Option<f32>{let bytes=self.data.get(self.pos..self.pos+4)?;self.pos+=4;Some(f32::from_le_bytes(bytes.try_into().ok()?))} }
