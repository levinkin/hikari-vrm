use crate::model::{Material, Model, Primitive, Texture, build_params, link_views};
use crate::{load, update};
use std::ffi::{CStr, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::slice;

fn write_error(error: *mut c_char, capacity: usize, message: &str) {
    if error.is_null() || capacity == 0 {
        return;
    }
    let length = message.len().min(capacity - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(message.as_ptr(), error as *mut u8, length);
        *error.add(length) = 0;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_load(bytes: *const u8, length: usize, error: *mut c_char, error_capacity: usize) -> *mut Model {
    if bytes.is_null() {
        write_error(error, error_capacity, "no bytes");
        return null_mut();
    }
    let data = unsafe { slice::from_raw_parts(bytes, length) };
    let built = catch_unwind(AssertUnwindSafe(|| {
        let mut model = load::load(data)?;
        build_params(&mut model);
        update::prepare(&mut model)?;
        link_views(&mut model);
        Ok::<Model, String>(model)
    }));
    match built {
        Ok(Ok(model)) => Box::into_raw(Box::new(model)),
        Ok(Err(message)) => {
            write_error(error, error_capacity, &message);
            null_mut()
        }
        Err(_) => {
            write_error(error, error_capacity, "internal error while loading");
            null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_free(model: *mut Model) {
    if !model.is_null() {
        drop(unsafe { Box::from_raw(model) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_param_count(model: *const Model) -> i32 {
    unsafe { (*model).params.len() as i32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_param_id(model: *const Model, index: i32) -> *const c_char {
    let model = unsafe { &*model };
    match model.params.get(index as usize) {
        Some(param) => param.id.as_ptr(),
        None => null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_param_range(model: *const Model, index: i32, minimum: *mut f32, maximum: *mut f32, default_value: *mut f32) {
    let model = unsafe { &*model };
    if let Some(param) = model.params.get(index as usize) {
        unsafe {
            *minimum = param.minimum;
            *maximum = param.maximum;
            *default_value = param.default_value;
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_param_values(model: *mut Model) -> *mut f32 {
    unsafe { (*model).param_values.as_mut_ptr() }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_bone_poses(model: *mut Model) -> *mut f32 {
    unsafe { (*model).bone_poses.as_mut_ptr() }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_physics_set(model: *mut Model, strength: f32, wind: f32) {
    let model = unsafe { &mut *model };
    model.physics_strength = strength;
    model.wind = wind;
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_update(model: *mut Model, seconds: f32) -> u32 {
    update::update(unsafe { &mut *model }, seconds)
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_texture_count(model: *const Model) -> i32 {
    unsafe { (*model).textures.len() as i32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_texture_at(model: *const Model, index: i32) -> *const Texture {
    let model = unsafe { &*model };
    match model.textures.get(index as usize) {
        Some(texture) => texture,
        None => null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_textures_release(model: *mut Model) {
    let model = unsafe { &mut *model };
    for image in &mut model.images {
        image.bytes = Vec::new();
    }
    for texture in &mut model.textures {
        texture.bytes = null();
        texture.length = 0;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_geometry_release(model: *mut Model) {
    let model = unsafe { &mut *model };
    for (data, primitive) in model.primitive_data.iter_mut().zip(&mut model.primitives) {
        data.positions = Vec::new();
        data.normals = Vec::new();
        data.uvs = Vec::new();
        data.joints = Vec::new();
        data.weights = Vec::new();
        data.indices = Vec::new();
        data.target_positions = Vec::new();
        data.target_normals = Vec::new();
        primitive.positions = null();
        primitive.normals = null();
        primitive.uvs = null();
        primitive.joints = null();
        primitive.weights = null();
        primitive.indices = null();
        primitive.target_positions = null();
        primitive.target_normals = null();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_material_count(model: *const Model) -> i32 {
    unsafe { (*model).materials.len() as i32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_material_at(model: *const Model, index: i32) -> *const Material {
    let model = unsafe { &*model };
    match model.materials.get(index as usize) {
        Some(material) => material,
        None => null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_primitive_count(model: *const Model) -> i32 {
    unsafe { (*model).primitives.len() as i32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_primitive_at(model: *const Model, index: i32) -> *const Primitive {
    let model = unsafe { &*model };
    match model.primitives.get(index as usize) {
        Some(primitive) => primitive,
        None => null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_skin_count(model: *const Model) -> i32 {
    unsafe { (*model).skins.len() as i32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_skin_joint_count(model: *const Model, skin: i32) -> i32 {
    let model = unsafe { &*model };
    match model.skins.get(skin as usize) {
        Some(skin) => skin.joints.len() as i32,
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_skin_joint_matrices(model: *const Model, skin: i32) -> *const f32 {
    let model = unsafe { &*model };
    match model.skins.get(skin as usize) {
        Some(skin) => skin.matrices.as_ptr(),
        None => null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_bounds(model: *const Model, minimum: *mut f32, maximum: *mut f32) {
    let model = unsafe { &*model };
    unsafe {
        std::ptr::copy_nonoverlapping(model.bounds[0].as_ptr(), minimum, 3);
        std::ptr::copy_nonoverlapping(model.bounds[1].as_ptr(), maximum, 3);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hv_bone_position(model: *const Model, humanoid_bone: *const c_char, position: *mut f32) -> bool {
    let model = unsafe { &*model };
    let Ok(name) = unsafe { CStr::from_ptr(humanoid_bone) }.to_str() else { return false };
    let Some(node) = model.bone(name) else { return false };
    let world = &model.nodes[node as usize].world;
    unsafe { std::ptr::copy_nonoverlapping(world[12..15].as_ptr(), position, 3) };
    true
}
