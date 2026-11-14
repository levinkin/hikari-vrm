use std::ffi::{c_char, c_void};

#[repr(C)]
pub struct Engine {
    _private: [u8; 0],
}

#[repr(C)]
pub struct Window {
    _private: [u8; 0],
}

pub type EngineHook = extern "C" fn(context: *mut c_void);

extern "C" {
    pub fn engine_create(vrm_path: *const c_char, width: i32, height: i32, error: *mut c_char, error_capacity: i32) -> *mut Engine;
    pub fn engine_destroy(e: *mut Engine);
    pub fn engine_gpu_name(e: *mut Engine) -> *const c_char;
    pub fn engine_has_param(e: *mut Engine, id: *const c_char) -> bool;
    pub fn engine_param_index(e: *mut Engine, id: *const c_char) -> i32;
    pub fn engine_param_count(e: *mut Engine) -> i32;
    pub fn engine_param_at(e: *mut Engine, index: i32, minimum: *mut f32, maximum: *mut f32, default_value: *mut f32) -> *const c_char;
    pub fn engine_param_set(e: *mut Engine, index: i32, value: f32);
    pub fn engine_bone_poses(e: *mut Engine) -> *mut f32;
    pub fn engine_update(e: *mut Engine, dt_ms: f32, before_model_update: EngineHook, context: *mut c_void);
    pub fn engine_sweep(e: *mut Engine);
    pub fn engine_background_load(e: *mut Engine, bytes: *const u8, length: usize, expected_type: *const c_char) -> i32;
    pub fn engine_background_clear(e: *mut Engine);
    pub fn engine_view_set(e: *mut Engine, zoom: f32, x: f32, y: f32, rot: f32) -> bool;
    pub fn engine_render(e: *mut Engine, width: i32, height: i32, zoom: f32, x: f32, y: f32, rot: f32, background_clock_ms: f32) -> *const u8;
    pub fn window_open(title: *const c_char, width: i32, height: i32) -> *mut Window;
    pub fn window_poll(w: *mut Window, width: *mut i32, height: *mut i32, frame: EngineHook, context: *mut c_void) -> bool;
    pub fn window_close(w: *mut Window);
    pub fn window_ask_save(w: *mut Window, model: *const c_char) -> i32;
    pub fn window_input(w: *mut Window, pan_x: *mut f32, pan_y: *mut f32, zoom: *mut f32, cursor_x: *mut f32, cursor_y: *mut f32, turn: *mut f32, keys: *mut i32) -> bool;
    pub fn engine_present(e: *mut Engine, w: *mut Window, background_clock_ms: f32, frame_seconds: f32, share: bool) -> bool;
}
