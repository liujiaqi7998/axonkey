const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDICES: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

pub struct Decoder {
    pending: Vec<u8>,
    frame: Vec<i16>,
    frame_bytes: usize,
    predictor: i32,
    step_index: i32,
    sync: Option<(i32, i32)>,
    lookahead: Option<i16>,
}
impl Default for Decoder {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            frame: Vec::new(),
            frame_bytes: 120,
            predictor: 0,
            step_index: 0,
            sync: None,
            lookahead: None,
        }
    }
}
impl Decoder {
    pub fn reset(&mut self) {
        self.pending.clear();
        self.lookahead = None;
        self.predictor = 0;
        self.step_index = 0;
        self.sync = None;
    }
    pub fn set_frame_bytes(&mut self, bytes: u16) {
        self.frame_bytes = usize::from(bytes).max(1);
    }
    pub fn synchronize(&mut self, predictor: i16, index: u8) {
        self.pending.clear();
        self.lookahead = None;
        self.sync = Some((i32::from(predictor), i32::from(index).min(88)));
    }
    pub fn flush(&mut self, output: &mut Vec<i16>) {
        if let Some(sample) = self.lookahead.take() {
            output.extend([sample; 3]);
        }
    }
    pub fn append(&mut self, packet: &[u8], output: &mut Vec<i16>) {
        self.pending.extend_from_slice(packet);
        let consume = self.pending.len() / self.frame_bytes * self.frame_bytes;
        for offset in (0..consume).step_by(self.frame_bytes) {
            if let Some((predictor, step)) = self.sync.take() {
                self.predictor = predictor;
                self.step_index = step;
            }
            self.frame.clear();
            for index in offset..offset + self.frame_bytes {
                let byte = self.pending[index];
                for nibble in [byte >> 4, byte & 15] {
                    let step = STEPS[self.step_index as usize];
                    let difference = (step >> 3)
                        + if nibble & 1 != 0 { step >> 2 } else { 0 }
                        + if nibble & 2 != 0 { step >> 1 } else { 0 }
                        + if nibble & 4 != 0 { step } else { 0 };
                    self.predictor = (self.predictor
                        + if nibble & 8 != 0 {
                            -difference
                        } else {
                            difference
                        })
                    .clamp(-32768, 32767);
                    self.step_index =
                        (self.step_index + INDICES[(nibble & 7) as usize]).clamp(0, 88);
                    self.frame.push(self.predictor as i16);
                }
            }
            for index in 0..self.frame.len() {
                let sample = if index > 0 && index + 1 < self.frame.len() {
                    ((i32::from(self.frame[index - 1])
                        + 2 * i32::from(self.frame[index])
                        + i32::from(self.frame[index + 1]))
                        >> 2) as i16
                } else {
                    self.frame[index]
                };
                if let Some(previous) = self.lookahead.replace(sample) {
                    output.push(previous);
                    output.push(((i32::from(previous) * 2 + i32::from(sample)) / 3) as i16);
                    output.push(((i32::from(previous) + i32::from(sample) * 2) / 3) as i16);
                }
            }
        }
        if consume != 0 {
            self.pending.drain(..consume);
        }
    }
}
