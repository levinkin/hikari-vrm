use crate::engine_ffi;
use crate::motion::Motion;
use crate::vrma;
use serde_json::{json, Value};
use std::collections::hash_map::RandomState;
use std::f64::consts::PI;
use std::ffi::{c_void, CStr, CString};
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

const YAW_PER_ANGLE_X: f64 = 0.6;
const PITCH_PER_REST_ANGLE: f64 = 6.0;
const PITCH_PER_ANGLE_Y: f64 = 0.5;
const HEAD_SMOOTH_MS: f64 = 80.0;
const HEAD_TURN_SCALE: f32 = 0.6;
const EXPRESSION_FADE_MS: f64 = 300.0;
const EXPRESSION_HOLD_MS: f64 = 2000.0;
const BLINK_INTERVAL_MS: f64 = 4000.0;
const BLINK_CLOSING_MS: f64 = 100.0;
const BLINK_CLOSED_MS: f64 = 50.0;
const BLINK_OPENING_MS: f64 = 150.0;
const BREATH_PERIOD_MS: f64 = 4000.0;

struct Playing {
    motion: usize,
    started_ms: f64,
    end_ms: f64,
    looped: bool,
}

pub struct Behavior {
    engine: *mut engine_ffi::Engine,
    motions: Arc<Vec<Motion>>,
    bindings: Vec<Vec<i32>>,
    defaults: Vec<f32>,
    values: Vec<f32>,
    ahead: Vec<f32>,
    pub expressions: Vec<(String, i32)>,
    state_progress: Vec<f64>,
    flashes: Vec<(usize, f64)>,
    playing: Vec<Playing>,
    idle_motion: Option<usize>,
    poses: Vec<f32>,
    ahead_poses: Vec<f32>,
    pose_start: i32,
    pub shown: [String; 2],
    mouth_value: f64,
    pub bg_clock_ms: f64,
    asleep: bool,
    action_left_ms: f64,
    random_wait_ms: f64,
    pool: Vec<String>,
    action_every_ms: (f64, f64),
    blink_at_ms: f64,
    pub head: [u8; 4],
    head_at: [f64; 2],
    head_dt_ms: f64,
    pub head_lead_ms: f32,
    params: Vec<(i32, f32, bool)>,
    held: Vec<(String, f32)>,
    pub rest_angle: f64,
    angle_indices: [i32; 3],
    gaze_indices: [i32; 2],
    bone_present: Vec<bool>,
    eye_indices: [i32; 2],
    mouth_index: i32,
    breath_index: i32,
}

fn random_unit() -> f64 {
    let hasher = RandomState::new().build_hasher();
    hasher.finish() as u32 as f64 / (u32::MAX as f64 + 1.0)
}

fn param_index(engine: *mut engine_ffi::Engine, id: &str) -> i32 {
    match CString::new(id) {
        Ok(id) => unsafe { engine_ffi::engine_param_index(engine, id.as_ptr()) },
        Err(_) => -1,
    }
}

fn set_value(values: &mut [f32], index: i32, value: f32) {
    if let Some(slot) = usize::try_from(index).ok().and_then(|index| values.get_mut(index)) {
        *slot = value;
    }
}

fn timing(motion: &Motion, playing: &Playing, at_ms: f64) -> (f64, f64) {
    let elapsed_ms = (at_ms - playing.started_ms).max(0.0);
    let fade_in = if motion.fade_in > 0.0 { 0.5 - 0.5 * ((elapsed_ms / (motion.fade_in * 1000.0)).clamp(0.0, 1.0) * PI).cos() } else { 1.0 };
    let fade_out = if motion.fade_out > 0.0 && playing.end_ms.is_finite() { 0.5 - 0.5 * (((playing.end_ms - at_ms) / (motion.fade_out * 1000.0)).clamp(0.0, 1.0) * PI).cos() } else { 1.0 };
    let time = if playing.looped && motion.duration > 0.0 { (elapsed_ms / 1000.0).rem_euclid(motion.duration) } else { (elapsed_ms / 1000.0).min(motion.duration) };
    (time, fade_in * fade_out)
}

fn apply_motions(motions: &[Motion], bindings: &[Vec<i32>], gaze_indices: [i32; 2], playing: &[Playing], values: &mut [f32], at_ms: f64) {
    for playing in playing {
        let motion = &motions[playing.motion];
        let (time, weight) = timing(motion, playing, at_ms);
        for (curve, index) in motion.curves.iter().zip(&bindings[playing.motion]) {
            let current = usize::try_from(*index).ok().and_then(|index| values.get(index)).copied().unwrap_or(0.0) as f64;
            set_value(values, *index, (current + (curve.value(time) - current) * weight) as f32);
        }
        if let Some(track) = &motion.look_at {
            let gaze = vrma::gaze(track.rotation(time as f32));
            for (axis, index) in gaze_indices.into_iter().enumerate() {
                let current = usize::try_from(index).ok().and_then(|index| values.get(index)).copied().unwrap_or(0.0) as f64;
                set_value(values, index, (current + (gaze[axis] - current) * weight) as f32);
            }
        }
    }
}

fn apply_bones(motions: &[Motion], bone_present: &[bool], playing: &[Playing], poses: &mut [f32], at_ms: f64) {
    poses.fill(0.0);
    for playing in playing {
        let motion = &motions[playing.motion];
        let (time, weight) = timing(motion, playing, at_ms);
        for (slot, track) in motion.bones.iter().filter(|(slot, _)| bone_present[*slot]) {
            let pose = &mut poses[slot * 5..slot * 5 + 5];
            if pose[4] == 0.0 {
                let [yaw, pitch, roll] = hv::REST_POSE.iter().find(|rest| rest.0 == *slot).map_or([0.0; 3], |rest| rest.1);
                pose[..4].copy_from_slice(&hv::quaternion_from_euler(yaw.to_radians(), pitch.to_radians(), roll.to_radians()));
                pose[4] = 1.0;
            }
            let blended = hv::quaternion_slerp([pose[0], pose[1], pose[2], pose[3]], track.rotation(time as f32), weight as f32);
            pose[..4].copy_from_slice(&blended);
        }
    }
}

fn pose_head(poses: &[f32]) -> [f32; 2] {
    let mut rotation = [0.0, 0.0, 0.0, 1.0];
    for slot in [1, 0] {
        let pose = &poses[slot * 5..slot * 5 + 5];
        if pose[4] > 0.0 {
            rotation = hv::quaternion_multiply(rotation, [pose[0], pose[1], pose[2], pose[3]]);
        }
    }
    let forward = hv::quaternion_rotate(rotation, [0.0, 0.0, 1.0]);
    [forward[0].atan2(forward[2]).to_degrees(), forward[1].clamp(-1.0, 1.0).asin().to_degrees()]
}

extern "C" fn apply_params(context: *mut c_void) {
    let behavior = unsafe { &*(context as *const Behavior) };
    for (index, value) in behavior.values.iter().enumerate() {
        unsafe { engine_ffi::engine_param_set(behavior.engine, index as i32, *value) };
    }
    unsafe { std::ptr::copy_nonoverlapping(behavior.poses.as_ptr(), engine_ffi::engine_bone_poses(behavior.engine), behavior.poses.len()) };
}

impl Behavior {
    pub fn new(engine: *mut engine_ffi::Engine, motions: Arc<Vec<Motion>>, idle_loop: &str, rest_angle: f64) -> Behavior {
        let count = unsafe { engine_ffi::engine_param_count(engine) }.max(0);
        let pose_start = hv::BONE_NAMES.iter().map(|bone| param_index(engine, &hv::bone_param_id(bone, "X"))).find(|index| *index >= 0).unwrap_or(count);
        let mut defaults = Vec::with_capacity(count as usize);
        let mut expressions = Vec::new();
        for index in 0..count {
            let (mut minimum, mut maximum, mut default_value) = (0.0f32, 0.0f32, 0.0f32);
            let id = unsafe { engine_ffi::engine_param_at(engine, index, &mut minimum, &mut maximum, &mut default_value) };
            defaults.push(default_value);
            if id.is_null() {
                continue;
            }
            let id = unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned();
            if index < pose_start && !hv::FIXED_PARAMS.iter().any(|fixed| fixed.0 == id) {
                expressions.push((id, index));
            }
        }
        let bindings = motions.iter().map(|motion| motion.curves.iter().map(|curve| param_index(engine, &curve.id)).collect()).collect();
        let idle_motion = motions.iter().position(|motion| !idle_loop.is_empty() && motion.name == idle_loop);
        if idle_motion.is_some() {
            println!("hikari-vrm: idle loop {}", idle_loop);
        }
        let pitch_home = rest_angle * PITCH_PER_REST_ANGLE;
        let pitch = (pitch_home.round() as i16).to_be_bytes();
        Behavior {
            engine,
            motions,
            bindings,
            values: defaults.clone(),
            ahead: defaults.clone(),
            defaults,
            state_progress: vec![0.0; expressions.len()],
            expressions,
            flashes: Vec::new(),
            playing: Vec::new(),
            idle_motion,
            poses: vec![0.0; hv::BONE_NAMES.len() * 5],
            ahead_poses: vec![0.0; hv::BONE_NAMES.len() * 5],
            pose_start,
            shown: [String::new(), String::new()],
            mouth_value: -1.0,
            bg_clock_ms: 0.0,
            asleep: false,
            action_left_ms: 0.0,
            random_wait_ms: -1.0,
            pool: Vec::new(),
            action_every_ms: (15000.0, 15000.0),
            blink_at_ms: random_unit() * (2.0 * BLINK_INTERVAL_MS - 1000.0),
            head: [0, 0, pitch[0], pitch[1]],
            head_at: [0.0, pitch_home],
            head_dt_ms: 0.0,
            head_lead_ms: 0.0,
            params: Vec::new(),
            held: Vec::new(),
            rest_angle,
            angle_indices: ["ParamAngleX", "ParamAngleY", "ParamAngleZ"].map(|id| param_index(engine, id)),
            gaze_indices: ["ParamEyeBallX", "ParamEyeBallY"].map(|id| param_index(engine, id)),
            bone_present: hv::BONE_NAMES.iter().map(|bone| param_index(engine, &hv::bone_param_id(bone, "X")) >= 0).collect(),
            eye_indices: ["ParamEyeLOpen", "ParamEyeROpen"].map(|id| param_index(engine, id)),
            mouth_index: param_index(engine, "ParamMouthOpenY"),
            breath_index: param_index(engine, "ParamBreath"),
        }
    }

    fn advance_wait(&mut self, dt_ms: f64) {
        self.action_left_ms -= dt_ms;
        if self.mouth_value >= 0.0 || self.asleep || self.action_left_ms > 0.0 {
            self.random_wait_ms = -1.0;
        } else if self.random_wait_ms < 0.0 {
            let (min, max) = self.action_every_ms;
            self.random_wait_ms = min + (random_unit() * (max - min + 1.0)).floor();
        } else {
            self.random_wait_ms -= dt_ms;
            if self.random_wait_ms <= 0.0 {
                self.random_wait_ms = -1.0;
                if let Some(name) = self.start_action() {
                    println!("hikari-vrm: idle random action={}", name);
                }
            }
        }
    }

    pub fn hold_params(&mut self, params: &[(String, f32)]) {
        if self.held.as_slice() == params {
            return;
        }
        self.held = params.to_vec();
        self.params.clear();
        for (id, value) in params {
            let index = param_index(self.engine, id);
            self.params.push((index, *value, index >= self.pose_start));
        }
    }

    fn fade_out_playing(&mut self, keep: Option<usize>) {
        for playing in self.playing.iter_mut().filter(|playing| Some(playing.motion) != keep) {
            playing.end_ms = playing.end_ms.min(self.bg_clock_ms + self.motions[playing.motion].fade_out * 1000.0);
        }
    }

    fn play_motion(&mut self, motion: usize, looped: bool) -> f64 {
        self.fade_out_playing(self.idle_motion.filter(|_| !self.asleep));
        let duration_ms = self.motions[motion].duration * 1000.0;
        self.playing.push(Playing { motion, started_ms: self.bg_clock_ms, end_ms: if looped { f64::INFINITY } else { self.bg_clock_ms + duration_ms }, looped });
        if looped { 0.0 } else { duration_ms }
    }

    pub fn react(&mut self, kind: &str) -> Value {
        if let Some(motion) = self.motions.iter().position(|motion| motion.name == kind).filter(|motion| Some(*motion) != self.idle_motion) {
            self.action_left_ms = self.play_motion(motion, false);
            return json!({ "action": kind, "motion": kind, "expression": null });
        }
        if let Some(expression) = self.expressions.iter().position(|expression| expression.0 == kind) {
            self.flashes.retain(|flash| flash.0 != expression);
            self.flashes.push((expression, self.bg_clock_ms));
            self.action_left_ms = 2.0 * EXPRESSION_FADE_MS + EXPRESSION_HOLD_MS;
            return json!({ "action": kind, "motion": null, "expression": kind });
        }
        Value::Null
    }

    fn start_action(&mut self) -> Option<String> {
        if self.pool.is_empty() {
            return None;
        }
        let name = self.pool[(random_unit() * self.pool.len() as f64) as usize].clone();
        self.react(&name);
        Some(name)
    }

    pub fn set_actions(&mut self, names: &[String], every_seconds: (u64, u64)) {
        let next_interval = (every_seconds.0 as f64 * 1000.0, every_seconds.1 as f64 * 1000.0);
        if self.action_every_ms != next_interval {
            self.random_wait_ms = -1.0;
        }
        self.pool = names.to_vec();
        self.action_every_ms = next_interval;
    }

    pub fn sleep_begin(&mut self) -> Value {
        let Some(motion) = self.motions.iter().position(|motion| motion.name == "sleep") else { return Value::Null };
        self.asleep = true;
        self.flashes.clear();
        self.play_motion(motion, true);
        json!({ "action": "sleep", "motion": "sleep", "expression": null })
    }

    pub fn sleep_end(&mut self) {
        self.asleep = false;
        self.fade_out_playing(None);
    }

    fn eyes_held(&self) -> bool {
        self.playing.iter().any(|playing| {
            let motion = &self.motions[playing.motion];
            Some(playing.motion) != self.idle_motion && self.bg_clock_ms < playing.end_ms - motion.fade_out * 1000.0 && motion.curves.iter().any(|curve| curve.id.starts_with("blink"))
        })
    }

    fn blink_value(&mut self) -> f32 {
        let phase = self.bg_clock_ms - self.blink_at_ms;
        if phase < 0.0 {
            return 1.0;
        }
        if phase < BLINK_CLOSING_MS {
            return (1.0 - phase / BLINK_CLOSING_MS) as f32;
        }
        if phase < BLINK_CLOSING_MS + BLINK_CLOSED_MS {
            return 0.0;
        }
        if phase < BLINK_CLOSING_MS + BLINK_CLOSED_MS + BLINK_OPENING_MS {
            return ((phase - BLINK_CLOSING_MS - BLINK_CLOSED_MS) / BLINK_OPENING_MS) as f32;
        }
        self.blink_at_ms = self.bg_clock_ms + random_unit() * (2.0 * BLINK_INTERVAL_MS - 1000.0);
        1.0
    }

    fn compose(&mut self, dt_ms: f64) {
        let now = self.bg_clock_ms;
        self.playing.retain(|playing| playing.end_ms > now);
        self.flashes.retain(|flash| now - flash.1 < 2.0 * EXPRESSION_FADE_MS + EXPRESSION_HOLD_MS);
        if let Some(idle) = self.idle_motion.filter(|_| !self.asleep) {
            if self.playing.iter().all(|playing| now >= playing.end_ms - self.motions[playing.motion].fade_out * 1000.0) {
                self.play_motion(idle, true);
            }
        }
        self.values.copy_from_slice(&self.defaults);
        let step = dt_ms / EXPRESSION_FADE_MS;
        for (slot, (name, index)) in self.expressions.iter().enumerate() {
            let target = if !self.asleep && self.shown.iter().any(|shown| shown == name) { 1.0 } else { 0.0 };
            let progress = &mut self.state_progress[slot];
            *progress = if target > *progress { (*progress + step).min(target) } else { (*progress - step).max(target) };
            let mut weight = 0.5 - 0.5 * (progress.clamp(0.0, 1.0) * PI).cos();
            for (flash, started_ms) in &self.flashes {
                if *flash == slot {
                    let elapsed = now - started_ms;
                    weight = weight.max(0.5 - 0.5 * ((elapsed / EXPRESSION_FADE_MS).min((2.0 * EXPRESSION_FADE_MS + EXPRESSION_HOLD_MS - elapsed) / EXPRESSION_FADE_MS).clamp(0.0, 1.0) * PI).cos());
                }
            }
            set_value(&mut self.values, *index, weight as f32);
        }
        if self.idle_motion.is_none() {
            set_value(&mut self.values, self.breath_index, (0.5 - 0.5 * (now / BREATH_PERIOD_MS * 2.0 * PI).cos()) as f32);
        }
        apply_motions(&self.motions, &self.bindings, self.gaze_indices, &self.playing, &mut self.values, now);
        apply_bones(&self.motions, &self.bone_present, &self.playing, &mut self.poses, now);
        if !self.eyes_held() {
            let blink = self.blink_value();
            for index in self.eye_indices {
                set_value(&mut self.values, index, blink);
            }
        }
        let (head_values, head_poses) = if self.head_lead_ms > 0.0 && !self.asleep {
            self.ahead.copy_from_slice(&self.defaults);
            apply_motions(&self.motions, &self.bindings, self.gaze_indices, &self.playing, &mut self.ahead, now + self.head_lead_ms as f64);
            apply_bones(&self.motions, &self.bone_present, &self.playing, &mut self.ahead_poses, now + self.head_lead_ms as f64);
            (&self.ahead, &self.ahead_poses)
        } else {
            (&self.values, &self.poses)
        };
        let bone_head = pose_head(head_poses);
        let head = [0, 1].map(|axis| usize::try_from(self.angle_indices[axis]).ok().and_then(|index| head_values.get(index)).copied().unwrap_or(0.0) * HEAD_TURN_SCALE + bone_head[axis]);
        for index in self.angle_indices {
            let scaled = usize::try_from(index).ok().and_then(|index| self.values.get(index)).copied().unwrap_or(0.0) * HEAD_TURN_SCALE;
            set_value(&mut self.values, index, scaled);
        }
        self.capture_head(head);
        if self.mouth_value >= 0.0 {
            set_value(&mut self.values, self.mouth_index, self.mouth_value as f32);
        }
        for (index, value, posed) in &self.params {
            let base = if *posed { usize::try_from(*index).ok().and_then(|index| self.values.get(index)).copied().unwrap_or(0.0) } else { 0.0 };
            set_value(&mut self.values, *index, base + *value);
        }
    }

    fn capture_head(&mut self, head: [f32; 2]) {
        let (angle_x, angle_y) = (head[0] as f64, head[1] as f64);
        let target = if self.asleep { [0.0, 0.0] } else { [(-angle_x / YAW_PER_ANGLE_X * 10.0).clamp(-800.0, 800.0), (self.rest_angle * PITCH_PER_REST_ANGLE + angle_y / PITCH_PER_ANGLE_Y * 10.0).clamp(0.0, 600.0)] };
        let blend = 1.0 - (-self.head_dt_ms / HEAD_SMOOTH_MS).exp();
        self.head_at[0] += (target[0] - self.head_at[0]) * blend;
        self.head_at[1] += (target[1] - self.head_at[1]) * blend;
        let yaw = self.head_at[0].round() as i16;
        let pitch = self.head_at[1].round() as i16;
        self.head[..2].copy_from_slice(&yaw.to_be_bytes());
        self.head[2..].copy_from_slice(&pitch.to_be_bytes());
    }

    pub fn update(&mut self, dt_ms: f32, mouth: f32) {
        let dt = dt_ms as f64;
        self.head_dt_ms = dt;
        self.mouth_value = mouth as f64;
        self.bg_clock_ms += dt;
        self.advance_wait(dt);
        self.compose(dt);
        let engine = self.engine;
        let context = self as *mut Behavior as *mut c_void;
        unsafe { engine_ffi::engine_update(engine, dt_ms, apply_params, context) };
    }
}
