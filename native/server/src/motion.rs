use std::f64::consts::PI;
use std::fs;
use std::io::Read;
use std::path::Path;

pub const MAX_FILE_BYTES: u64 = 64 << 20;

const ROOT_EPSILON: f64 = 1e-9;
const ZERO_EPSILON: f64 = 1e-12;

enum Segment {
    Linear([f64; 2], [f64; 2]),
    Bezier([[f64; 2]; 4]),
    Stepped([f64; 2], [f64; 2]),
}

pub struct Curve {
    pub id: String,
    start: [f64; 2],
    segments: Vec<Segment>,
}

pub struct Motion {
    pub name: String,
    pub duration: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    pub curves: Vec<Curve>,
    pub bones: Vec<(usize, BoneTrack)>,
    pub look_at: Option<BoneTrack>,
}

pub struct BoneTrack {
    pub times: Vec<f32>,
    pub keys: Vec<[f32; 4]>,
    pub mode: u8,
}

impl BoneTrack {
    pub fn rotation(&self, time: f32) -> [f32; 4] {
        let stride = if self.mode == 2 { 3 } else { 1 };
        let value = if self.mode == 2 { 1 } else { 0 };
        let next = self.times.partition_point(|at| *at <= time);
        if next == 0 || next == self.times.len() || self.mode == 1 {
            return self.keys[(next.max(1) - 1) * stride + value];
        }
        let (start, end) = (self.times[next - 1], self.times[next]);
        let span = end - start;
        let t = if span > 0.0 { (time - start) / span } else { 0.0 };
        let (from, to) = (self.keys[(next - 1) * stride + value], self.keys[next * stride + value]);
        if self.mode == 0 {
            return hv::quaternion_slerp(from, to, t);
        }
        let (out_tangent, in_tangent) = (self.keys[(next - 1) * stride + 2], self.keys[next * stride]);
        let (t2, t3) = (t * t, t * t * t);
        let mixed = [0, 1, 2, 3].map(|axis| (2.0 * t3 - 3.0 * t2 + 1.0) * from[axis] + (t3 - 2.0 * t2 + t) * span * out_tangent[axis] + (-2.0 * t3 + 3.0 * t2) * to[axis] + (t3 - t2) * span * in_tangent[axis]);
        hv::quaternion_normalize(mixed)
    }
}

pub fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > max {
        return Err(format!("larger than {} bytes", max));
    }
    Ok(bytes)
}

fn solve_cubic(cubic: f64, quadratic: f64, linear: f64, constant: f64) -> f64 {
    let (roots, count) = if cubic.abs() < ZERO_EPSILON {
        if quadratic.abs() < ZERO_EPSILON {
            return if linear.abs() < ZERO_EPSILON { 0.0 } else { (-constant / linear).clamp(0.0, 1.0) };
        }
        let root = (linear * linear - 4.0 * quadratic * constant).max(0.0).sqrt();
        ([(-linear + root) / (2.0 * quadratic), (-linear - root) / (2.0 * quadratic), 0.0], 2)
    } else {
        let shift = quadratic / (3.0 * cubic);
        let depressed_linear = (3.0 * cubic * linear - quadratic * quadratic) / (3.0 * cubic * cubic);
        let depressed_constant = (2.0 * quadratic.powi(3) - 9.0 * cubic * quadratic * linear + 27.0 * cubic * cubic * constant) / (27.0 * cubic.powi(3));
        let discriminant = depressed_constant * depressed_constant / 4.0 + depressed_linear.powi(3) / 27.0;
        if discriminant > 0.0 {
            let root = discriminant.sqrt();
            ([(-depressed_constant / 2.0 + root).cbrt() + (-depressed_constant / 2.0 - root).cbrt() - shift, 0.0, 0.0], 1)
        } else if depressed_linear.abs() < ZERO_EPSILON {
            ([-shift, 0.0, 0.0], 1)
        } else if discriminant == 0.0 {
            ([3.0 * depressed_constant / depressed_linear - shift, -3.0 * depressed_constant / (2.0 * depressed_linear) - shift, 0.0], 2)
        } else {
            let radius = 2.0 * (-depressed_linear / 3.0).sqrt();
            let angle = ((3.0 * depressed_constant / (2.0 * depressed_linear)) * (-3.0 / depressed_linear).sqrt()).clamp(-1.0, 1.0).acos() / 3.0;
            ([0.0, 1.0, 2.0].map(|branch| radius * (angle - 2.0 * PI * branch / 3.0).cos() - shift), 3)
        }
    };
    roots[..count].iter().copied().find(|root| (-ROOT_EPSILON..=1.0 + ROOT_EPSILON).contains(root)).unwrap_or(roots[0]).clamp(0.0, 1.0)
}

fn bezier_value(points: &[[f64; 2]; 4], time: f64) -> f64 {
    let [first, second, third, fourth] = points.map(|point| point[0]);
    let cubic = -first + 3.0 * second - 3.0 * third + fourth;
    let quadratic = 3.0 * first - 6.0 * second + 3.0 * third;
    let linear = -3.0 * first + 3.0 * second;
    let progress = solve_cubic(cubic, quadratic, linear, first - time);
    let remain = 1.0 - progress;
    remain.powi(3) * points[0][1] + 3.0 * remain * remain * progress * points[1][1] + 3.0 * remain * progress * progress * points[2][1] + progress.powi(3) * points[3][1]
}

impl Curve {
    pub fn keyed(id: String, times: &[f32], keys: &[f64], mode: u8) -> Curve {
        let stride = if mode == 2 { 3 } else { 1 };
        let value = if mode == 2 { 1 } else { 0 };
        let segments = (1..times.len()).map(|index| {
            let begin = [times[index - 1] as f64, keys[(index - 1) * stride + value]];
            let end = [times[index] as f64, keys[index * stride + value]];
            let third = (end[0] - begin[0]) / 3.0;
            match mode {
                1 => Segment::Stepped(begin, end),
                2 => Segment::Bezier([begin, [begin[0] + third, begin[1] + third * keys[(index - 1) * 3 + 2]], [end[0] - third, end[1] - third * keys[index * 3]], end]),
                _ => Segment::Linear(begin, end),
            }
        });
        Curve { id, start: [times[0] as f64, keys[value]], segments: segments.collect() }
    }

    pub fn value(&self, time: f64) -> f64 {
        if time <= self.start[0] {
            return self.start[1];
        }
        for segment in &self.segments {
            let end = match segment {
                Segment::Linear(_, end) | Segment::Stepped(_, end) => *end,
                Segment::Bezier(points) => points[3],
            };
            if time > end[0] {
                continue;
            }
            return match segment {
                Segment::Linear(begin, end) if end[0] > begin[0] => begin[1] + (end[1] - begin[1]) * (time - begin[0]) / (end[0] - begin[0]),
                Segment::Linear(_, end) => end[1],
                Segment::Bezier(points) => bezier_value(points, time),
                Segment::Stepped(begin, _) => begin[1],
            };
        }
        match self.segments.last() {
            Some(Segment::Linear(_, end) | Segment::Stepped(_, end)) => end[1],
            Some(Segment::Bezier(points)) => points[3][1],
            None => self.start[1],
        }
    }
}
