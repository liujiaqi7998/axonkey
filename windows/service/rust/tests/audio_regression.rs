//! Immutable C++ output baselines retain migration coverage after deleting C++.
use axonkey_service::{
    audio::{
        decoder::Decoder,
        gain::Gain,
        session::{Effects, Microphone, Session},
    },
    Error, Result,
};
use sha2::{Digest, Sha256};

#[derive(Default)]
struct Trace(Sha256);
impl Trace {
    fn pcm(&mut self, samples: &[i16]) {
        self.0.update((samples.len() as u64).to_le_bytes());
        for sample in samples {
            self.0.update(sample.to_le_bytes());
        }
    }
    fn state(&mut self, values: [u32; 14]) {
        for value in values {
            self.0.update(value.to_le_bytes());
        }
    }
    fn verify(self, key: &str) {
        let expected = include_str!("fixtures/audio.sha256")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once(' '))
            .find(|(name, _)| *name == key)
            .unwrap_or_else(|| panic!("missing baseline {key}"));
        assert_eq!(
            format!("{:x}", self.0.finalize()),
            expected.1,
            "C++ baseline mismatch: {key}"
        );
    }
}

#[test]
fn decoder_matches_cpp_baseline_across_packets_sync_reset_and_flush() {
    let bytes: Vec<u8> = (0..8192).map(|i| ((i * 73 + i / 17) & 255) as u8).collect();
    for frame in [1, 2, 7, 120, 255, 1024] {
        for chunk in [1, 7, 119, 120, 241, 4096] {
            let mut trace = Trace::default();
            let mut decoder = Decoder::default();
            decoder.set_frame_bytes(frame);
            trace.pcm(&[]);
            for (i, packet) in bytes.chunks(chunk).enumerate() {
                if i % 37 == 11 {
                    decoder.synchronize(-30123, 255);
                    trace.pcm(&[]);
                }
                if i % 71 == 9 {
                    decoder.reset();
                    trace.pcm(&[]);
                }
                let mut pcm = Vec::new();
                decoder.append(packet, &mut pcm);
                trace.pcm(&pcm);
            }
            for _ in 0..2 {
                let mut tail = Vec::new();
                decoder.flush(&mut tail);
                trace.pcm(&tail);
            }
            trace.verify(&format!("decoder-{frame}-{chunk}"));
        }
    }
}

#[test]
fn gain_matches_cpp_baseline_for_every_pcm16_sample_and_gain() {
    let source: Vec<i16> = (i16::MIN..=i16::MAX).collect();
    for db in (-30..=30).chain([-100, 100]) {
        let mut samples = source.clone();
        Gain::new(db).apply(&mut samples);
        let mut trace = Trace::default();
        trace.pcm(&samples);
        trace.verify(&format!("gain-{db}"));
    }
}

#[derive(Default)]
struct Mic {
    failure: u32,
    starts: u32,
    resets: u32,
    pushes: u32,
    stops: u32,
    drains: u32,
    pcm: Vec<i16>,
}
impl Mic {
    fn result(&self, operation: u32) -> Result<()> {
        if self.failure == operation {
            Err(Error::new(31, "injected"))
        } else {
            Ok(())
        }
    }
}
impl Microphone for Mic {
    fn start(&mut self) -> Result<()> {
        self.starts += 1;
        self.result(1)
    }
    fn reset(&mut self) -> Result<()> {
        self.resets += 1;
        self.result(2)
    }
    fn push(&mut self, pcm: &[i16]) -> Result<()> {
        self.pushes += 1;
        self.pcm.extend_from_slice(pcm);
        self.result(3)
    }
    fn stop(&mut self, drain: bool) {
        self.stops += 1;
        self.drains += u32::from(drain);
    }
}
fn step(trace: &mut Trace, session: &mut Session<Mic>, kind: u32, failure: u32, packet: &[u8]) {
    session.microphone.failure = failure;
    session.microphone.pcm.clear();
    let effect = match kind {
        0 => session.control(packet),
        1 => session.audio(packet),
        _ => Effects {
            command: session.close_command(),
            ..Default::default()
        },
    };
    let command = effect.command.unwrap_or_default();
    let packed = command
        .iter()
        .enumerate()
        .fold(0, |a, (i, b)| a | (u32::from(*b) << (8 * i)));
    let issue = effect.issue.map_or(0, |(code, _)| match code {
        "virtual_microphone_unavailable" => 1,
        "virtual_microphone_reset_failed" => 2,
        _ => 3,
    });
    let (peak, rms) = effect.level.unwrap_or_default();
    let mic = &session.microphone;
    trace.state([
        u32::from(session.active),
        u32::from(session.microphone_open),
        u32::from(session.protocol_version),
        u32::from(session.session_id),
        mic.starts,
        mic.resets,
        mic.pushes,
        mic.stops,
        mic.drains,
        issue,
        packed,
        command.len() as u32,
        peak.to_bits(),
        rms.to_bits(),
    ]);
    trace.pcm(&mic.pcm);
}

#[test]
fn voice_protocol_matches_cpp_baseline_including_rejections_and_late_audio() {
    for version in [0, 1] {
        for gain in [-30, 0, 2, 30] {
            let mut trace = Trace::default();
            let mut session = Session::new(Mic::default(), gain);
            let audio = [0x78; 120];
            step(&mut trace, &mut session, 1, 0, &audio);
            step(
                &mut trace,
                &mut session,
                0,
                0,
                &[0x0b, version, 0, 2, 0, 0, 120],
            );
            for failure in [0, 1, 2, 3] {
                for (kind, mode, packet) in [
                    (0, failure, &[8][..]),
                    (0, 0, &[4, 0, 2, 42]),
                    (1, 0, &audio),
                    (0, 0, &[8]),
                    (0, failure, &[4, 0, 2, 43]),
                    (1, failure, &audio),
                    (0, 0, &[10, 0, 0, 0, 0x80, 1, 88]),
                    (1, 0, &audio),
                    (2, 0, &[]),
                    (0, 0, &[0, 255]),
                    (1, 0, &audio),
                    (0, 0, &[8]),
                    (1, 0, &audio),
                    (0, 0, &[0]),
                ] {
                    step(&mut trace, &mut session, kind, mode, packet);
                }
            }
            let controls: &[&[u8]] = &[
                &[],
                &[4],
                &[4, 0, 1],
                &[8],
                &[0],
                &[0xb, 0, 0, 0, 0, 0, 0],
                &[0xb, 1, 0, 0, 2, 0, 7],
                &[10, 0, 0, 0, 255, 255, 255],
            ];
            for i in 0..500 {
                step(
                    &mut trace,
                    &mut session,
                    0,
                    (i % 4) as u32,
                    controls[(i * 37 + i / 13) % controls.len()],
                );
                step(&mut trace, &mut session, 1, 0, &audio[..i % 120]);
            }
            trace.verify(&format!("session-{version}-{gain}"));
        }
    }
}
