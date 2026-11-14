mod ffi;
mod json;
mod load;
mod math;
mod model;
mod update;

pub use load::{glb_floats, split_glb};
pub use math::{quaternion_from_euler, quaternion_multiply, quaternion_normalize, quaternion_rotate, quaternion_slerp};
pub use model::{BONE_NAMES, FIXED_PARAMS, HIPS_POSITION_PARAMS, bone_param_id};
pub use update::REST_POSE;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
