use std::ffi::CString;
use std::ptr::null;

pub const CHANGED_JOINTS: u32 = 1;
pub const CHANGED_MORPHS: u32 = 2;
pub const CHANGED_MATERIALS: u32 = 4;
pub const CHANGED_NODES: u32 = 8;

pub const MIME_PNG: i32 = 0;
pub const MIME_JPEG: i32 = 1;
pub const MATERIAL_PBR: i32 = 0;
pub const MATERIAL_UNLIT: i32 = 1;
pub const MATERIAL_MTOON: i32 = 2;
pub const ALPHA_OPAQUE: i32 = 0;
pub const ALPHA_MASK: i32 = 1;
pub const ALPHA_BLEND: i32 = 2;
pub const OUTLINE_NONE: i32 = 0;
pub const OUTLINE_WORLD: i32 = 1;
pub const OUTLINE_SCREEN: i32 = 2;

pub const BONE_NAMES: [&str; 55] = [
    "head",
    "neck",
    "spine",
    "chest",
    "upperChest",
    "leftEye",
    "rightEye",
    "leftShoulder",
    "rightShoulder",
    "leftUpperArm",
    "rightUpperArm",
    "leftLowerArm",
    "rightLowerArm",
    "leftHand",
    "rightHand",
    "hips",
    "jaw",
    "leftUpperLeg",
    "leftLowerLeg",
    "leftFoot",
    "leftToes",
    "rightUpperLeg",
    "rightLowerLeg",
    "rightFoot",
    "rightToes",
    "leftThumbMetacarpal",
    "leftThumbProximal",
    "leftThumbDistal",
    "leftIndexProximal",
    "leftIndexIntermediate",
    "leftIndexDistal",
    "leftMiddleProximal",
    "leftMiddleIntermediate",
    "leftMiddleDistal",
    "leftRingProximal",
    "leftRingIntermediate",
    "leftRingDistal",
    "leftLittleProximal",
    "leftLittleIntermediate",
    "leftLittleDistal",
    "rightThumbMetacarpal",
    "rightThumbProximal",
    "rightThumbDistal",
    "rightIndexProximal",
    "rightIndexIntermediate",
    "rightIndexDistal",
    "rightMiddleProximal",
    "rightMiddleIntermediate",
    "rightMiddleDistal",
    "rightRingProximal",
    "rightRingIntermediate",
    "rightRingDistal",
    "rightLittleProximal",
    "rightLittleIntermediate",
    "rightLittleDistal",
];
pub const HIPS_POSITION_PARAMS: [&str; 3] = ["ParamHipsPositionX", "ParamHipsPositionY", "ParamHipsPositionZ"];

pub const FIXED_PARAMS: [(&str, f32, f32, f32); 11] = [
    ("ParamAngleX", -30.0, 30.0, 0.0),
    ("ParamAngleY", -30.0, 30.0, 0.0),
    ("ParamAngleZ", -30.0, 30.0, 0.0),
    ("ParamBodyAngleX", -10.0, 10.0, 0.0),
    ("ParamEyeLOpen", 0.0, 1.0, 1.0),
    ("ParamEyeROpen", 0.0, 1.0, 1.0),
    ("ParamEyeBallX", -1.0, 1.0, 0.0),
    ("ParamEyeBallY", -1.0, 1.0, 0.0),
    ("ParamMouthOpenY", 0.0, 1.0, 0.0),
    ("ParamMouthForm", -1.0, 1.0, 0.0),
    ("ParamBreath", 0.0, 1.0, 0.0),
];

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TextureRef {
    pub texture: i32,
    pub tex_coord: i32,
    pub offset: [f32; 2],
    pub scale: [f32; 2],
    pub rotation: f32,
}

impl Default for TextureRef {
    fn default() -> Self {
        TextureRef { texture: -1, tex_coord: 0, offset: [0.0, 0.0], scale: [1.0, 1.0], rotation: 0.0 }
    }
}

#[repr(C)]
pub struct Texture {
    pub bytes: *const u8,
    pub length: usize,
    pub mime: i32,
    pub mag_filter: i32,
    pub min_filter: i32,
    pub wrap_s: i32,
    pub wrap_t: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Material {
    pub kind: i32,
    pub alpha_mode: i32,
    pub alpha_cutoff: f32,
    pub double_sided: bool,
    pub transparent_with_z_write: bool,
    pub render_queue_offset: i32,
    pub base_color: [f32; 4],
    pub base_color_texture: TextureRef,
    pub emissive: [f32; 3],
    pub emissive_texture: TextureRef,
    pub normal_texture: TextureRef,
    pub normal_scale: f32,
    pub metallic: f32,
    pub roughness: f32,
    pub metallic_roughness_texture: TextureRef,
    pub occlusion_texture: TextureRef,
    pub occlusion_strength: f32,
    pub shade_color: [f32; 3],
    pub shade_multiply_texture: TextureRef,
    pub shading_shift: f32,
    pub shading_shift_texture: TextureRef,
    pub shading_shift_texture_scale: f32,
    pub shading_toony: f32,
    pub gi_equalization: f32,
    pub matcap_color: [f32; 3],
    pub matcap_texture: TextureRef,
    pub rim_color: [f32; 3],
    pub rim_multiply_texture: TextureRef,
    pub rim_lighting_mix: f32,
    pub rim_fresnel_power: f32,
    pub rim_lift: f32,
    pub outline_width_mode: i32,
    pub outline_width: f32,
    pub outline_width_multiply_texture: TextureRef,
    pub outline_color: [f32; 3],
    pub outline_lighting_mix: f32,
    pub uv_animation_mask_texture: TextureRef,
    pub uv_animation_scroll_x: f32,
    pub uv_animation_scroll_y: f32,
    pub uv_animation_rotation: f32,
}

#[repr(C)]
pub struct Primitive {
    pub node: i32,
    pub skin: i32,
    pub material: i32,
    pub vertex_count: u32,
    pub index_count: u32,
    pub target_count: u32,
    pub positions: *const f32,
    pub normals: *const f32,
    pub uvs: *const f32,
    pub joints: *const u16,
    pub weights: *const f32,
    pub indices: *const u32,
    pub target_positions: *const f32,
    pub target_normals: *const f32,
    pub morph_weights: *const f32,
    pub world: *const f32,
}

const _: () = assert!(size_of::<TextureRef>() == 28 && size_of::<Texture>() == 40 && size_of::<Material>() == 472 && size_of::<Primitive>() == 104);
const _: () = assert!(std::mem::offset_of!(Material, outline_width_mode) == 380 && std::mem::offset_of!(Material, uv_animation_rotation) == 468 && std::mem::offset_of!(Primitive, world) == 96);

pub struct Image {
    pub bytes: Vec<u8>,
    pub mime: i32,
}

pub struct TextureSource {
    pub image: u32,
    pub mag_filter: i32,
    pub min_filter: i32,
    pub wrap_s: i32,
    pub wrap_t: i32,
}

pub struct PrimitiveData {
    pub node: u32,
    pub skin: i32,
    pub material: i32,
    pub vertex_count: u32,
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub uvs: Vec<f32>,
    pub joints: Vec<u16>,
    pub weights: Vec<f32>,
    pub indices: Vec<u32>,
    pub target_count: u32,
    pub target_positions: Vec<f32>,
    pub target_normals: Vec<f32>,
}

pub struct Node {
    pub parent: i32,
    pub children: Vec<u32>,
    pub mesh: i32,
    pub skin: i32,
    pub rest_translation: [f32; 3],
    pub rest_rotation: [f32; 4],
    pub rest_scale: [f32; 3],
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub world: [f32; 16],
    pub rest_weights: Vec<f32>,
    pub weights: Vec<f32>,
}

pub struct Skin {
    pub joints: Vec<u32>,
    pub inverse_bind_matrices: Vec<[f32; 16]>,
    pub matrices: Vec<f32>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Override {
    None,
    Block,
    Blend,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ColorTarget {
    Color,
    EmissionColor,
    ShadeColor,
    MatcapColor,
    RimColor,
    OutlineColor,
}

pub struct MorphBind {
    pub node: u32,
    pub index: u32,
    pub weight: f32,
}

pub struct ColorBind {
    pub material: u32,
    pub target: ColorTarget,
    pub value: [f32; 4],
}

pub struct TextureTransformBind {
    pub material: u32,
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

pub struct Expression {
    pub name: String,
    pub preset: bool,
    pub is_binary: bool,
    pub override_blink: Override,
    pub override_look_at: Override,
    pub override_mouth: Override,
    pub morph_binds: Vec<MorphBind>,
    pub color_binds: Vec<ColorBind>,
    pub texture_transform_binds: Vec<TextureTransformBind>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum LookAtKind {
    Bone,
    Expression,
}

#[derive(Clone, Copy)]
pub struct RangeMap {
    pub input_max_value: f32,
    pub output_scale: f32,
}

pub struct LookAt {
    pub kind: LookAtKind,
    pub horizontal_inner: RangeMap,
    pub horizontal_outer: RangeMap,
    pub vertical_down: RangeMap,
    pub vertical_up: RangeMap,
}

#[derive(Clone, Copy)]
pub enum ColliderShape {
    Sphere { offset: [f32; 3], radius: f32, inside: bool },
    Capsule { offset: [f32; 3], radius: f32, tail: [f32; 3], inside: bool },
    Plane { offset: [f32; 3], normal: [f32; 3] },
}

pub struct Collider {
    pub node: u32,
    pub shape: ColliderShape,
}

pub struct SpringJoint {
    pub node: u32,
    pub hit_radius: f32,
    pub stiffness: f32,
    pub gravity_power: f32,
    pub gravity_direction: [f32; 3],
    pub drag_force: f32,
}

pub struct Spring {
    pub joints: Vec<SpringJoint>,
    pub collider_groups: Vec<u32>,
    pub center: i32,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Axis {
    PositiveX,
    NegativeX,
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

#[derive(Clone, Copy)]
pub enum ConstraintKind {
    Roll { source: u32, axis: Axis },
    Aim { source: u32, aim_axis: Axis },
    Rotation { source: u32 },
}

pub struct Constraint {
    pub node: u32,
    pub kind: ConstraintKind,
    pub weight: f32,
}

pub struct Param {
    pub id: CString,
    pub minimum: f32,
    pub maximum: f32,
    pub default_value: f32,
}

pub struct Model {
    pub nodes: Vec<Node>,
    pub images: Vec<Image>,
    pub texture_sources: Vec<TextureSource>,
    pub textures: Vec<Texture>,
    pub material_rest: Vec<Material>,
    pub materials: Vec<Material>,
    pub primitive_data: Vec<PrimitiveData>,
    pub primitives: Vec<Primitive>,
    pub skins: Vec<Skin>,
    pub humanoid: Vec<(String, u32)>,
    pub expressions: Vec<Expression>,
    pub look_at: LookAt,
    pub colliders: Vec<Collider>,
    pub collider_groups: Vec<Vec<u32>>,
    pub springs: Vec<Spring>,
    pub constraints: Vec<Constraint>,
    pub params: Vec<Param>,
    pub param_values: Vec<f32>,
    pub bone_poses: Vec<f32>,
    pub physics_strength: f32,
    pub wind: f32,
    pub bounds: [[f32; 3]; 2],
    pub state: crate::update::State,
}

impl Model {
    pub fn bone(&self, name: &str) -> Option<u32> {
        for (bone, node) in &self.humanoid {
            if bone == name {
                return Some(*node);
            }
        }
        None
    }
}

pub fn bone_param_id(bone: &str, axis: &str) -> String {
    let mut letters = bone.chars();
    let first = letters.next().map(|letter| letter.to_ascii_uppercase()).into_iter();
    format!("Param{}{}", first.chain(letters).collect::<String>(), axis)
}

pub fn build_params(model: &mut Model) {
    model.params.clear();
    for (id, minimum, maximum, default_value) in FIXED_PARAMS {
        model.params.push(Param { id: CString::new(id).unwrap(), minimum, maximum, default_value });
    }
    for expression in &model.expressions {
        let id = CString::new(expression.name.as_str()).unwrap_or_default();
        model.params.push(Param { id, minimum: 0.0, maximum: 1.0, default_value: 0.0 });
    }
    for name in BONE_NAMES {
        if model.bone(name).is_none() {
            continue;
        }
        for axis in ["X", "Y", "Z"] {
            model.params.push(Param { id: CString::new(bone_param_id(name, axis)).unwrap(), minimum: -180.0, maximum: 180.0, default_value: 0.0 });
        }
    }
    if model.bone("hips").is_some() {
        for id in HIPS_POSITION_PARAMS {
            model.params.push(Param { id: CString::new(id).unwrap(), minimum: -10.0, maximum: 10.0, default_value: 0.0 });
        }
    }
    model.param_values.clear();
    for param in &model.params {
        model.param_values.push(param.default_value);
    }
}

pub fn link_views(model: &mut Model) {
    let mut textures = Vec::with_capacity(model.texture_sources.len());
    for source in &model.texture_sources {
        let image = &model.images[source.image as usize];
        textures.push(Texture {
            bytes: if image.bytes.is_empty() { null() } else { image.bytes.as_ptr() },
            length: image.bytes.len(),
            mime: image.mime,
            mag_filter: source.mag_filter,
            min_filter: source.min_filter,
            wrap_s: source.wrap_s,
            wrap_t: source.wrap_t,
        });
    }
    model.textures = textures;
    let mut primitives = Vec::with_capacity(model.primitive_data.len());
    for data in &model.primitive_data {
        let node = &model.nodes[data.node as usize];
        primitives.push(Primitive {
            node: data.node as i32,
            skin: data.skin,
            material: data.material,
            vertex_count: data.vertex_count,
            index_count: data.indices.len() as u32,
            target_count: data.target_count,
            positions: data.positions.as_ptr(),
            normals: if data.normals.is_empty() { null() } else { data.normals.as_ptr() },
            uvs: if data.uvs.is_empty() { null() } else { data.uvs.as_ptr() },
            joints: if data.joints.is_empty() { null() } else { data.joints.as_ptr() },
            weights: if data.weights.is_empty() { null() } else { data.weights.as_ptr() },
            indices: data.indices.as_ptr(),
            target_positions: if data.target_positions.is_empty() { null() } else { data.target_positions.as_ptr() },
            target_normals: if data.target_normals.is_empty() { null() } else { data.target_normals.as_ptr() },
            morph_weights: if node.weights.is_empty() { null() } else { node.weights.as_ptr() },
            world: node.world.as_ptr(),
        });
    }
    model.primitives = primitives;
}
