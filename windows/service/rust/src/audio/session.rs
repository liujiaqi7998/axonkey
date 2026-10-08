use super::{
    decoder::Decoder,
    gain::{level, Gain},
};
use crate::Result;

pub trait Microphone {
    fn start(&mut self) -> Result<()>;
    fn reset(&mut self) -> Result<()>;
    fn push(&mut self, samples: &[i16]) -> Result<()>;
    fn stop(&mut self, drain: bool);
}
#[derive(Default, Debug)]
pub struct Effects {
    pub command: Option<Vec<u8>>,
    pub level: Option<(f32, f32)>,
    pub issue: Option<(&'static str, crate::Error)>,
}
pub struct Session<M: Microphone> {
    pub microphone: M,
    decoder: Decoder,
    gain: Gain,
    samples: Vec<i16>,
    capabilities: bool,
    pub active: bool,
    pub microphone_open: bool,
    rejected: bool,
    ended: bool,
    pub protocol_version: u16,
    pub session_id: u8,
}
impl<M: Microphone> Session<M> {
    pub fn new(microphone: M, gain: i32) -> Self {
        Self {
            microphone,
            decoder: Decoder::default(),
            gain: Gain::new(gain),
            samples: Vec::new(),
            capabilities: false,
            active: false,
            microphone_open: false,
            rejected: false,
            ended: false,
            protocol_version: 0x100,
            session_id: 0,
        }
    }
    pub fn set_gain(&mut self, db: i32) {
        self.gain.set(db);
    }
    fn open(&mut self, effects: &mut Effects) -> bool {
        if self.rejected {
            return false;
        }
        if self.microphone_open {
            return true;
        }
        match self.microphone.start() {
            Ok(()) => {
                self.microphone_open = true;
                true
            }
            Err(error) => {
                self.rejected = true;
                effects.issue = Some(("virtual_microphone_unavailable", error));
                false
            }
        }
    }
    fn fail(&mut self) {
        self.rejected = true;
        self.microphone_open = false;
        self.microphone.stop(false);
    }
    fn push(&mut self, effects: &mut Effects) -> bool {
        if self.samples.is_empty() {
            return true;
        }
        self.gain.apply(&mut self.samples);
        effects.level = Some(level(&self.samples));
        if let Err(error) = self.microphone.push(&self.samples) {
            effects.issue = Some(("virtual_microphone_write_failed", error));
            self.fail();
            return false;
        }
        true
    }
    pub fn control(&mut self, bytes: &[u8]) -> Effects {
        let mut effects = Effects::default();
        match bytes.first().copied() {
            Some(0x0b) if bytes.len() >= 7 => {
                self.protocol_version = u16::from_be_bytes([bytes[1], bytes[2]]);
                let codecs = if self.protocol_version >= 0x100 && bytes[3] == 0 {
                    bytes[4]
                } else {
                    bytes[3]
                };
                self.capabilities = codecs & 2 != 0;
                let frame = u16::from_be_bytes([bytes[5], bytes[6]]);
                self.decoder
                    .set_frame_bytes(if frame == 0 { 120 } else { frame });
            }
            Some(0x08) if self.capabilities && (!self.active || self.rejected) => {
                self.active = true;
                self.ended = false;
                self.rejected = false;
                self.session_id = 0;
                self.decoder.reset();
                if self.open(&mut effects) {
                    effects.command = Some(if self.protocol_version >= 0x100 {
                        vec![0x0c, 0]
                    } else {
                        vec![0x0c, 0, 2]
                    });
                }
            }
            Some(0x04) if self.capabilities && (bytes.len() < 3 || bytes[2] == 2) => {
                if !self.active {
                    self.rejected = false;
                }
                self.active = true;
                self.ended = false;
                self.session_id = bytes.get(3).copied().unwrap_or(0);
                self.decoder.reset();
                if self.microphone_open {
                    if let Err(error) = self.microphone.reset() {
                        effects.issue = Some(("virtual_microphone_reset_failed", error));
                        self.fail();
                    }
                } else {
                    self.open(&mut effects);
                }
            }
            Some(0x0a) if bytes.len() >= 7 && !self.rejected => {
                self.decoder
                    .synchronize(i16::from_be_bytes([bytes[4], bytes[5]]), bytes[6]);
            }
            Some(0) => {
                if self.microphone_open {
                    self.samples.clear();
                    self.decoder.flush(&mut self.samples);
                    if self.push(&mut effects) {
                        self.microphone.stop(true);
                    }
                }
                self.active = false;
                self.microphone_open = false;
                self.rejected = false;
                self.ended = true;
                self.decoder.reset();
            }
            _ => {}
        }
        effects
    }
    pub fn audio(&mut self, bytes: &[u8]) -> Effects {
        let mut effects = Effects::default();
        if bytes.is_empty() || !self.capabilities || self.rejected || self.ended {
            return effects;
        }
        self.active = true;
        if self.open(&mut effects) {
            self.samples.clear();
            self.decoder.append(bytes, &mut self.samples);
            self.push(&mut effects);
        }
        effects
    }
    pub fn close_command(&self) -> Option<Vec<u8>> {
        self.active.then(|| {
            if self.protocol_version >= 0x100 {
                vec![0x0d, self.session_id]
            } else {
                vec![0x0d]
            }
        })
    }
}
impl<M: Microphone> Drop for Session<M> {
    fn drop(&mut self) {
        self.microphone.stop(false);
    }
}
