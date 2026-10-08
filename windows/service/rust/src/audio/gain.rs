pub const DEFAULT_GAIN: i32 = 2;
pub const MIN_GAIN: i32 = -30;
pub const MAX_GAIN: i32 = 30;

pub struct Gain {
    db: i32,
    multiplier: f64,
}
impl Gain {
    pub fn new(db: i32) -> Self {
        let db = if (MIN_GAIN..=MAX_GAIN).contains(&db) {
            db
        } else {
            DEFAULT_GAIN
        };
        Self {
            db,
            multiplier: 10_f64.powf(f64::from(db) / 20.0),
        }
    }
    pub fn set(&mut self, db: i32) {
        if self.db != db {
            *self = Self::new(db);
        }
    }
    pub fn apply(&self, samples: &mut [i16]) {
        if self.multiplier == 1.0 {
            return;
        }
        for sample in samples {
            *sample = (f64::from(*sample) * self.multiplier)
                .clamp(-32768.0, 32767.0)
                .round() as i16;
        }
    }
}
pub fn level(samples: &[i16]) -> (f32, f32) {
    let mut peak = 0_f32;
    let mut sum = 0_f64;
    for sample in samples {
        let value = f32::from(*sample) / 32768.0;
        peak = peak.max(value.abs());
        sum += f64::from(value) * f64::from(value);
    }
    (
        peak,
        if samples.is_empty() {
            0.0
        } else {
            (sum / samples.len() as f64).sqrt() as f32
        },
    )
}
