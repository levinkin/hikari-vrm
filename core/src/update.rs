use crate::math::{
    IDENTITY_MATRIX, IDENTITY_QUATERNION, Matrix, Quaternion, Vector, matrix_compose, matrix_inverse, matrix_multiply, matrix_rotation, normalize, quaternion_from_euler,
    quaternion_from_to, quaternion_multiply, quaternion_power, quaternion_rotate, quaternion_slerp, transform_point,
};
use crate::model::{
    Axis, BONE_NAMES, CHANGED_JOINTS, CHANGED_MATERIALS, CHANGED_MORPHS, CHANGED_NODES, ColliderShape, ColorTarget, ConstraintKind, FIXED_PARAMS, LookAtKind, Material, Model, Override,
    RangeMap, TextureRef,
};

const STEP: f32 = 1.0 / 60.0;
const MAX_STEPS: f32 = 4.0;
const MINIMUM_BONE_LENGTH: f32 = 1.0e-4;
const MAX_CONSTRAINT_DEPTH: usize = 64;

const PRESET_NAMES: [&str; 12] = ["blink", "blinkLeft", "blinkRight", "lookUp", "lookDown", "lookLeft", "lookRight", "aa", "ih", "ou", "ee", "oh"];
const BLINK: usize = 0;
const BLINK_LEFT: usize = 1;
const BLINK_RIGHT: usize = 2;
const LOOK_UP: usize = 3;
const LOOK_DOWN: usize = 4;
const LOOK_LEFT: usize = 5;
const LOOK_RIGHT: usize = 6;
const MOUTH_AA: usize = 7;
const MOUTH_OU: usize = 9;
const MOUTH_EE: usize = 10;

const CATEGORY_NONE: u8 = 0;
const CATEGORY_BLINK: u8 = 1;
const CATEGORY_LOOK_AT: u8 = 2;
const CATEGORY_MOUTH: u8 = 3;

const HEAD: usize = 0;
const NECK: usize = 1;
const SPINE: usize = 2;
const LEFT_EYE: usize = 5;
const RIGHT_EYE: usize = 6;
const LEFT_SHOULDER: usize = 7;
const RIGHT_SHOULDER: usize = 8;
const HIPS: usize = 15;
pub const REST_POSE: [(usize, [f32; 3]); 6] = [(9, [0.0, 0.0, -70.0]), (10, [0.0, 0.0, 70.0]), (11, [-12.0, 0.0, 0.0]), (12, [12.0, 0.0, 0.0]), (13, [0.0, 0.0, -5.0]), (14, [0.0, 0.0, 5.0])];
const BREATH_CHEST_DEGREES: f32 = 0.8;
const BREATH_SHOULDER_DEGREES: f32 = 1.5;

const ANGLE_X: usize = 0;
const ANGLE_Y: usize = 1;
const ANGLE_Z: usize = 2;
const BODY_ANGLE_X: usize = 3;
const EYE_LEFT_OPEN: usize = 4;
const EYE_RIGHT_OPEN: usize = 5;
const EYE_BALL_X: usize = 6;
const EYE_BALL_Y: usize = 7;
const MOUTH_OPEN: usize = 8;
const MOUTH_FORM: usize = 9;
const BREATH: usize = 10;

pub struct SpringPair {
    head: u32,
    spring: u32,
    center: i32,
    hit_radius: f32,
    stiffness: f32,
    gravity_power: f32,
    gravity_direction: Vector,
    drag_force: f32,
    bone_axis: Vector,
    bone_length: f32,
    initial_local_matrix: Matrix,
    initial_local_rotation: Quaternion,
    previous_tail: Vector,
    current_tail: Vector,
}

#[derive(Default)]
pub struct State {
    order: Vec<u32>,
    position: Vec<u32>,
    subtree_end: Vec<u32>,
    local_dirty: Vec<bool>,
    moved: Vec<bool>,
    moved_any: bool,
    rest_world_rotation: Vec<Quaternion>,
    last_params: Vec<f32>,
    expression_weights: Vec<f32>,
    expression_category: Vec<u8>,
    presets: [i32; PRESET_NAMES.len()],
    bones: Vec<i32>,
    bone_params: Vec<(u32, u32)>,
    hips_params: i32,
    last_bone_poses: Vec<f32>,
    morph_slot: Vec<i32>,
    morph_nodes: Vec<u32>,
    morph_scratch: Vec<Vec<f32>>,
    material_targets: Vec<u32>,
    material_scratch: Vec<Material>,
    constraint_order: Vec<u32>,
    pairs: Vec<SpringPair>,
    time: f32,
}

fn uv_textures(material: &mut Material) -> [&mut TextureRef; 10] {
    [
        &mut material.base_color_texture,
        &mut material.emissive_texture,
        &mut material.normal_texture,
        &mut material.metallic_roughness_texture,
        &mut material.occlusion_texture,
        &mut material.shade_multiply_texture,
        &mut material.shading_shift_texture,
        &mut material.rim_multiply_texture,
        &mut material.outline_width_multiply_texture,
        &mut material.uv_animation_mask_texture,
    ]
}

fn color_field(material: &mut Material, target: ColorTarget) -> &mut [f32] {
    match target {
        ColorTarget::Color => &mut material.base_color,
        ColorTarget::EmissionColor => &mut material.emissive,
        ColorTarget::ShadeColor => &mut material.shade_color,
        ColorTarget::MatcapColor => &mut material.matcap_color,
        ColorTarget::RimColor => &mut material.rim_color,
        ColorTarget::OutlineColor => &mut material.outline_color,
    }
}

fn material_differs(a: &mut Material, b: &mut Material) -> bool {
    let targets = [ColorTarget::Color, ColorTarget::EmissionColor, ColorTarget::ShadeColor, ColorTarget::MatcapColor, ColorTarget::RimColor, ColorTarget::OutlineColor];
    for target in targets {
        if color_field(a, target) != color_field(b, target) {
            return true;
        }
    }
    let textures_a = uv_textures(a);
    let textures_b = uv_textures(b);
    for (texture_a, texture_b) in textures_a.iter().zip(textures_b.iter()) {
        if texture_a.offset != texture_b.offset || texture_a.scale != texture_b.scale {
            return true;
        }
    }
    false
}

fn axis_vector(axis: Axis) -> Vector {
    match axis {
        Axis::PositiveX => [1.0, 0.0, 0.0],
        Axis::NegativeX => [-1.0, 0.0, 0.0],
        Axis::PositiveY => [0.0, 1.0, 0.0],
        Axis::NegativeY => [0.0, -1.0, 0.0],
        Axis::PositiveZ => [0.0, 0.0, 1.0],
        Axis::NegativeZ => [0.0, 0.0, -1.0],
    }
}

fn range_map(value: f32, map: RangeMap) -> f32 {
    let magnitude = value.abs();
    if map.input_max_value <= 0.0 {
        return if magnitude == 0.0 { 0.0 } else { map.output_scale };
    }
    magnitude.min(map.input_max_value) / map.input_max_value * map.output_scale
}

fn local_world(model: &Model, node: usize) -> Matrix {
    let current = &model.nodes[node];
    let local = matrix_compose(current.translation, current.rotation, current.scale);
    if current.parent < 0 { local } else { matrix_multiply(&model.nodes[current.parent as usize].world, &local) }
}

fn refresh_world(model: &mut Model, state: &mut State, node: usize) {
    let world = local_world(model, node);
    if world != model.nodes[node].world {
        model.nodes[node].world = world;
        state.moved[node] = true;
        state.moved_any = true;
    }
}

fn refresh_subtree(model: &mut Model, state: &mut State, node: usize) {
    let start = state.position[node] as usize;
    for position in start..state.subtree_end[start] as usize {
        let current = state.order[position] as usize;
        refresh_world(model, state, current);
    }
}

fn set_rotation(model: &mut Model, state: &mut State, node: usize, rotation: Quaternion) -> bool {
    if model.nodes[node].rotation == rotation {
        return false;
    }
    model.nodes[node].rotation = rotation;
    state.local_dirty[node] = true;
    true
}

fn rotate_bone(model: &mut Model, state: &mut State, slot: usize, rotation: Quaternion) {
    let node = state.bones[slot];
    if node < 0 {
        return;
    }
    let node = node as usize;
    let local = if rotation == IDENTITY_QUATERNION {
        model.nodes[node].rest_rotation
    } else {
        let parent = model.nodes[node].parent;
        let parent_rest = if parent < 0 { IDENTITY_QUATERNION } else { state.rest_world_rotation[parent as usize] };
        let parent_inverse = [-parent_rest[0], -parent_rest[1], -parent_rest[2], parent_rest[3]];
        quaternion_multiply(quaternion_multiply(parent_inverse, rotation), state.rest_world_rotation[node])
    };
    set_rotation(model, state, node, local);
}

fn raise_preset(state: &mut State, preset: usize, value: f32) {
    let expression = state.presets[preset];
    if expression >= 0 {
        let weight = &mut state.expression_weights[expression as usize];
        *weight = weight.max(value);
    }
}

fn evaluate_expressions(model: &Model, state: &mut State, yaw: f32, pitch: f32) -> f32 {
    let params = &model.param_values;
    for index in 0..model.expressions.len() {
        state.expression_weights[index] = params[FIXED_PARAMS.len() + index].clamp(0.0, 1.0);
    }
    let open = params[MOUTH_OPEN].clamp(0.0, 1.0);
    let form = params[MOUTH_FORM].clamp(-1.0, 1.0);
    let left = 1.0 - params[EYE_LEFT_OPEN].clamp(0.0, 1.0);
    let right = 1.0 - params[EYE_RIGHT_OPEN].clamp(0.0, 1.0);
    if state.presets[BLINK_LEFT] < 0 && state.presets[BLINK_RIGHT] < 0 {
        raise_preset(state, BLINK, left.min(right));
    }
    raise_preset(state, BLINK_LEFT, left);
    raise_preset(state, BLINK_RIGHT, right);
    raise_preset(state, MOUTH_AA, open * (1.0 - form.abs() * 0.5));
    raise_preset(state, MOUTH_EE, form.max(0.0) * open);
    raise_preset(state, MOUTH_OU, (-form).max(0.0) * open);
    if model.look_at.kind == LookAtKind::Expression {
        let horizontal = range_map(yaw, model.look_at.horizontal_outer);
        raise_preset(state, if yaw > 0.0 { LOOK_LEFT } else { LOOK_RIGHT }, horizontal);
        if pitch > 0.0 {
            raise_preset(state, LOOK_DOWN, range_map(pitch, model.look_at.vertical_down));
        } else {
            raise_preset(state, LOOK_UP, range_map(pitch, model.look_at.vertical_up));
        }
    }
    for (index, expression) in model.expressions.iter().enumerate() {
        if expression.is_binary {
            let weight = &mut state.expression_weights[index];
            *weight = if *weight > 0.5 { 1.0 } else { 0.0 };
        }
    }
    let mut look_at_factor = 1.0;
    for category in [CATEGORY_BLINK, CATEGORY_LOOK_AT, CATEGORY_MOUTH] {
        let mut blocked = false;
        let mut blend = 0.0;
        for (index, expression) in model.expressions.iter().enumerate() {
            let weight = state.expression_weights[index];
            if weight <= 0.0 || state.expression_category[index] == category {
                continue;
            }
            let rule = match category {
                CATEGORY_BLINK => expression.override_blink,
                CATEGORY_LOOK_AT => expression.override_look_at,
                _ => expression.override_mouth,
            };
            match rule {
                Override::Block => blocked = true,
                Override::Blend => blend += weight,
                Override::None => {}
            }
        }
        let factor = if blocked { 0.0 } else { 1.0 - blend.min(1.0) };
        if factor >= 1.0 {
            continue;
        }
        if category == CATEGORY_LOOK_AT {
            look_at_factor = factor;
        }
        for (index, expression) in model.expressions.iter().enumerate() {
            if state.expression_category[index] == category {
                let weight = &mut state.expression_weights[index];
                *weight = if expression.is_binary { 0.0 } else { *weight * factor };
            }
        }
    }
    look_at_factor
}

fn apply_morphs(model: &mut Model, state: &mut State) -> u32 {
    for weights in &mut state.morph_scratch {
        weights.fill(0.0);
    }
    for (index, expression) in model.expressions.iter().enumerate() {
        let weight = state.expression_weights[index];
        if weight == 0.0 {
            continue;
        }
        for bind in &expression.morph_binds {
            let slot = state.morph_slot[bind.node as usize];
            if let Some(target) = state.morph_scratch[slot as usize].get_mut(bind.index as usize) {
                *target += bind.weight * weight;
            }
        }
    }
    let mut changed = 0;
    for (slot, node) in state.morph_nodes.iter().enumerate() {
        let weights = &mut model.nodes[*node as usize].weights;
        if *weights != state.morph_scratch[slot] {
            weights.copy_from_slice(&state.morph_scratch[slot]);
            changed = CHANGED_MORPHS;
        }
    }
    changed
}

fn apply_materials(model: &mut Model, state: &mut State) -> u32 {
    for material in &state.material_targets {
        state.material_scratch[*material as usize] = model.material_rest[*material as usize];
    }
    for (index, expression) in model.expressions.iter().enumerate() {
        let weight = state.expression_weights[index];
        if weight == 0.0 {
            continue;
        }
        for bind in &expression.color_binds {
            let base = color_field(&mut model.material_rest[bind.material as usize], bind.target);
            let current = color_field(&mut state.material_scratch[bind.material as usize], bind.target);
            for component in 0..current.len() {
                current[component] += (bind.value[component] - base[component]) * weight;
            }
        }
        for bind in &expression.texture_transform_binds {
            let rest_textures = uv_textures(&mut model.material_rest[bind.material as usize]);
            let textures = uv_textures(&mut state.material_scratch[bind.material as usize]);
            for (texture, base) in textures.into_iter().zip(rest_textures.iter()) {
                for component in 0..2 {
                    texture.offset[component] += (bind.offset[component] - base.offset[component]) * weight;
                    texture.scale[component] += (bind.scale[component] - base.scale[component]) * weight;
                }
            }
        }
    }
    let mut changed = 0;
    for material in &state.material_targets {
        let index = *material as usize;
        if material_differs(&mut model.materials[index], &mut state.material_scratch[index]) {
            model.materials[index] = state.material_scratch[index];
            changed = CHANGED_MATERIALS;
        }
    }
    changed
}

fn apply_pose(model: &mut Model, state: &mut State, yaw: f32, pitch: f32, look_at_factor: f32) {
    let params = &model.param_values;
    let (angle_x, angle_y, angle_z, body) = (params[ANGLE_X], params[ANGLE_Y], params[ANGLE_Z], params[BODY_ANGLE_X]);
    let breath = params[BREATH].clamp(0.0, 1.0);
    let mut angles = [[0.0f32; 3]; 55];
    let head_share = if state.bones[NECK] >= 0 && state.bones[HEAD] >= 0 { 0.6 } else { 1.0 };
    for (slot, share) in [(NECK, 1.0 - head_share), (HEAD, head_share)] {
        angles[slot] = [angle_x * share, -angle_y * share, -angle_z * share];
    }
    let body_bones = state.bones[SPINE..SPINE + 3].iter().filter(|node| **node >= 0).count();
    if body_bones > 0 {
        for slot in SPINE..SPINE + 3 {
            let lift = if slot == SPINE { 0.0 } else { -BREATH_CHEST_DEGREES * breath };
            angles[slot] = [body / body_bones as f32, lift, 0.0];
        }
    }
    for (slot, side) in [(LEFT_SHOULDER, 1.0), (RIGHT_SHOULDER, -1.0)] {
        angles[slot] = [0.0, 0.0, side * BREATH_SHOULDER_DEGREES * breath];
    }
    let mut rest = [[0.0f32; 3]; 55];
    for (slot, pose) in REST_POSE {
        rest[slot] = pose;
    }
    if model.look_at.kind == LookAtKind::Bone {
        let look_at = &model.look_at;
        let vertical = if pitch > 0.0 { range_map(pitch, look_at.vertical_down) } else { -range_map(pitch, look_at.vertical_up) };
        let left_yaw = if yaw > 0.0 { range_map(yaw, look_at.horizontal_outer) } else { -range_map(yaw, look_at.horizontal_inner) };
        let right_yaw = if yaw > 0.0 { range_map(yaw, look_at.horizontal_inner) } else { -range_map(yaw, look_at.horizontal_outer) };
        for (slot, horizontal) in [(LEFT_EYE, left_yaw), (RIGHT_EYE, right_yaw)] {
            angles[slot] = [horizontal * look_at_factor, vertical * look_at_factor, 0.0];
        }
    }
    for &(slot, first) in &state.bone_params {
        let [x, y, z] = [0, 1, 2].map(|axis| model.param_values[first as usize + axis]);
        angles[slot as usize][0] += y;
        angles[slot as usize][1] += x;
        angles[slot as usize][2] += z;
    }
    for (slot, [yaw, pitch, roll]) in angles.into_iter().enumerate() {
        let [rest_yaw, rest_pitch, rest_roll] = rest[slot];
        let mut rotation = quaternion_from_euler((yaw + rest_yaw).to_radians(), (pitch + rest_pitch).to_radians(), (roll + rest_roll).to_radians());
        let pose: [f32; 5] = model.bone_poses[slot * 5..slot * 5 + 5].try_into().unwrap_or_default();
        if pose[4] > 0.0 {
            let clip = quaternion_multiply([pose[0], pose[1], pose[2], pose[3]], quaternion_from_euler(yaw.to_radians(), pitch.to_radians(), roll.to_radians()));
            rotation = quaternion_slerp(rotation, clip, pose[4]);
        }
        rotate_bone(model, state, slot, rotation);
    }
    let hips = state.bones[HIPS];
    if state.hips_params >= 0 && hips >= 0 {
        let first = state.hips_params as usize;
        let offset = [0, 1, 2].map(|axis| model.param_values[first + axis]);
        let node = hips as usize;
        let parent = model.nodes[node].parent;
        let parent_rest = if parent < 0 { IDENTITY_QUATERNION } else { state.rest_world_rotation[parent as usize] };
        let local = quaternion_rotate([-parent_rest[0], -parent_rest[1], -parent_rest[2], parent_rest[3]], offset);
        let rest = model.nodes[node].rest_translation;
        let translation = [rest[0] + local[0], rest[1] + local[1], rest[2] + local[2]];
        if model.nodes[node].translation != translation {
            model.nodes[node].translation = translation;
            state.local_dirty[node] = true;
        }
    }
}

fn apply_constraint(model: &mut Model, state: &mut State, index: usize) {
    let constraint = &model.constraints[index];
    let destination = constraint.node as usize;
    let destination_rest = model.nodes[destination].rest_rotation;
    let destination_inverse = [-destination_rest[0], -destination_rest[1], -destination_rest[2], destination_rest[3]];
    let target = match constraint.kind {
        ConstraintKind::Roll { source, axis } => {
            let source_node = &model.nodes[source as usize];
            let source_rest = source_node.rest_rotation;
            let delta_in_parent = quaternion_multiply(source_node.rotation, [-source_rest[0], -source_rest[1], -source_rest[2], source_rest[3]]);
            let delta_in_destination = quaternion_multiply(quaternion_multiply(destination_inverse, delta_in_parent), destination_rest);
            let roll_axis = axis_vector(axis);
            let swing = quaternion_from_to(roll_axis, quaternion_rotate(delta_in_destination, roll_axis));
            let twist = quaternion_multiply([-swing[0], -swing[1], -swing[2], swing[3]], delta_in_destination);
            quaternion_slerp(destination_rest, quaternion_multiply(destination_rest, twist), constraint.weight)
        }
        ConstraintKind::Aim { source, aim_axis } => {
            let parent = model.nodes[destination].parent;
            let parent_world = if parent < 0 { IDENTITY_QUATERNION } else { matrix_rotation(&model.nodes[parent as usize].world) };
            let from = quaternion_rotate(quaternion_multiply(parent_world, destination_rest), axis_vector(aim_axis));
            let source_world = &model.nodes[source as usize].world;
            let destination_world = &model.nodes[destination].world;
            let to = [source_world[12] - destination_world[12], source_world[13] - destination_world[13], source_world[14] - destination_world[14]];
            let aim = quaternion_from_to(from, to);
            let parent_inverse = [-parent_world[0], -parent_world[1], -parent_world[2], parent_world[3]];
            let local = quaternion_multiply(quaternion_multiply(quaternion_multiply(parent_inverse, aim), parent_world), destination_rest);
            quaternion_slerp(destination_rest, local, constraint.weight)
        }
        ConstraintKind::Rotation { source } => {
            let source_node = &model.nodes[source as usize];
            let source_rest = source_node.rest_rotation;
            let delta = quaternion_multiply([-source_rest[0], -source_rest[1], -source_rest[2], source_rest[3]], source_node.rotation);
            quaternion_slerp(destination_rest, quaternion_multiply(destination_rest, delta), constraint.weight)
        }
    };
    if set_rotation(model, state, destination, target) {
        state.local_dirty[destination] = false;
        refresh_subtree(model, state, destination);
    }
}

fn collide(shape: ColliderShape, world: &Matrix, tail: Vector, joint_radius: f32) -> (Vector, f32) {
    let (offset, radius, segment_end, inside) = match shape {
        ColliderShape::Sphere { offset, radius, inside } => (offset, radius, None, inside),
        ColliderShape::Capsule { offset, radius, tail, inside } => (offset, radius, Some(tail), inside),
        ColliderShape::Plane { offset, normal } => {
            let point = transform_point(world, offset);
            let inverse = matrix_inverse(world);
            let mut transformed = [0.0; 3];
            for row in 0..3 {
                transformed[row] = inverse[row * 4] * normal[0] + inverse[row * 4 + 1] * normal[1] + inverse[row * 4 + 2] * normal[2];
            }
            let direction = normalize(transformed);
            let distance = (tail[0] - point[0]) * direction[0] + (tail[1] - point[1]) * direction[1] + (tail[2] - point[2]) * direction[2] - joint_radius;
            return (direction, distance);
        }
    };
    let start = transform_point(world, offset);
    let mut delta = [tail[0] - start[0], tail[1] - start[1], tail[2] - start[2]];
    if let Some(segment_end) = segment_end {
        let end = transform_point(world, segment_end);
        let segment = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
        let projection = segment[0] * delta[0] + segment[1] * delta[1] + segment[2] * delta[2];
        let squared = segment[0] * segment[0] + segment[1] * segment[1] + segment[2] * segment[2];
        let along = if projection <= 0.0 { 0.0 } else if projection > squared { 1.0 } else { projection / squared };
        for component in 0..3 {
            delta[component] -= segment[component] * along;
        }
    }
    let length = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
    let direction = normalize(delta);
    if inside {
        ([-direction[0], -direction[1], -direction[2]], radius - joint_radius - length)
    } else {
        (direction, length - radius - joint_radius)
    }
}

fn constrain_length(origin: Vector, tail: Vector, length: f32) -> Vector {
    let direction = normalize([tail[0] - origin[0], tail[1] - origin[1], tail[2] - origin[2]]);
    [origin[0] + direction[0] * length, origin[1] + direction[1] * length, origin[2] + direction[2] * length]
}

fn step_pair(model: &mut Model, state: &mut State, index: usize) {
    let pair = &state.pairs[index];
    let head = pair.head as usize;
    let node = &model.nodes[head];
    let origin = [node.world[12], node.world[13], node.world[14]];
    let parent_world = if node.parent < 0 { IDENTITY_MATRIX } else { model.nodes[node.parent as usize].world };
    let center_world = if pair.center < 0 { IDENTITY_MATRIX } else { model.nodes[pair.center as usize].world };
    let current = transform_point(&center_world, pair.current_tail);
    let previous = transform_point(&center_world, pair.previous_tail);
    let rest_direction = quaternion_rotate(quaternion_multiply(matrix_rotation(&parent_world), pair.initial_local_rotation), pair.bone_axis);
    let mut next = [0.0; 3];
    for component in 0..3 {
        let wind = if component == 0 { model.wind } else { 0.0 };
        let inertia = (current[component] - previous[component]) * (1.0 - pair.drag_force);
        let stiffness = STEP * rest_direction[component] * pair.stiffness;
        let external = STEP * (pair.gravity_direction[component] * pair.gravity_power + wind);
        next[component] = current[component] + inertia + stiffness + external;
    }
    next = constrain_length(origin, next, pair.bone_length);
    for group in &model.springs[pair.spring as usize].collider_groups {
        let Some(colliders) = model.collider_groups.get(*group as usize) else { continue };
        for collider_index in colliders {
            let collider = &model.colliders[*collider_index as usize];
            let (direction, distance) = collide(collider.shape, &model.nodes[collider.node as usize].world, next, pair.hit_radius);
            if distance < 0.0 {
                for component in 0..3 {
                    next[component] -= direction[component] * distance;
                }
                next = constrain_length(origin, next, pair.bone_length);
            }
        }
    }
    let rest_world = matrix_multiply(&parent_world, &pair.initial_local_matrix);
    let to = transform_point(&matrix_inverse(&rest_world), next);
    let mut swing = quaternion_from_to(pair.bone_axis, to);
    if model.physics_strength != 1.0 {
        swing = quaternion_power(swing, model.physics_strength.clamp(0.0, 4.0));
    }
    let rotation = quaternion_multiply(pair.initial_local_rotation, swing);
    let center_inverse = if pair.center < 0 { IDENTITY_MATRIX } else { matrix_inverse(&center_world) };
    let pair = &mut state.pairs[index];
    pair.previous_tail = pair.current_tail;
    pair.current_tail = transform_point(&center_inverse, next);
    if set_rotation(model, state, head, rotation) {
        state.local_dirty[head] = false;
        refresh_subtree(model, state, head);
    }
}

fn update_skins(model: &mut Model, state: &State) -> u32 {
    let mut changed = 0;
    for skin in &mut model.skins {
        for (index, joint) in skin.joints.iter().enumerate() {
            if state.moved[*joint as usize] {
                let matrix = matrix_multiply(&model.nodes[*joint as usize].world, &skin.inverse_bind_matrices[index]);
                skin.matrices[index * 16..index * 16 + 16].copy_from_slice(&matrix);
                changed = CHANGED_JOINTS;
            }
        }
    }
    changed
}

fn build_order(model: &Model, state: &mut State) {
    let count = model.nodes.len();
    state.order = Vec::with_capacity(count);
    state.position = vec![0; count];
    state.subtree_end = vec![0; count];
    let mut stack = Vec::new();
    for root in (0..count).rev().filter(|node| model.nodes[*node].parent < 0) {
        stack.push((root as u32, false));
    }
    while let Some((node, finished)) = stack.pop() {
        if finished {
            let start = state.position[node as usize] as usize;
            state.subtree_end[start] = state.order.len() as u32;
            continue;
        }
        state.position[node as usize] = state.order.len() as u32;
        state.order.push(node);
        stack.push((node, true));
        for child in model.nodes[node as usize].children.iter().rev() {
            stack.push((*child, false));
        }
    }
}

fn constraint_depends(model: &Model, constraint: usize, other: usize) -> bool {
    let target = model.constraints[other].node;
    let current = &model.constraints[constraint];
    let source = match current.kind {
        ConstraintKind::Roll { source, .. } | ConstraintKind::Aim { source, .. } | ConstraintKind::Rotation { source } => source,
    };
    if source == target {
        return true;
    }
    if !matches!(current.kind, ConstraintKind::Aim { .. }) {
        return false;
    }
    for start in [source as i32, model.nodes[current.node as usize].parent] {
        let mut node = start;
        while node >= 0 {
            if node as u32 == target {
                return true;
            }
            node = model.nodes[node as usize].parent;
        }
    }
    false
}

fn visit_constraint(model: &Model, state: &mut State, visiting: &mut [u8], constraint: usize, depth: usize) -> Result<(), String> {
    if visiting[constraint] == 2 {
        return Ok(());
    }
    if visiting[constraint] == 1 {
        return Err("node constraints form a cycle".to_string());
    }
    if depth >= MAX_CONSTRAINT_DEPTH {
        return Err(format!("node constraint chain is deeper than {}", MAX_CONSTRAINT_DEPTH));
    }
    visiting[constraint] = 1;
    for other in 0..model.constraints.len() {
        if other != constraint && constraint_depends(model, constraint, other) {
            visit_constraint(model, state, visiting, other, depth + 1)?;
        }
    }
    visiting[constraint] = 2;
    state.constraint_order.push(constraint as u32);
    Ok(())
}

pub fn prepare(model: &mut Model) -> Result<(), String> {
    let mut state = State::default();
    let count = model.nodes.len();
    for node in &mut model.nodes {
        node.translation = node.rest_translation;
        node.rotation = node.rest_rotation;
        node.scale = node.rest_scale;
        node.weights = node.rest_weights.clone();
    }
    model.materials = model.material_rest.clone();
    build_order(model, &mut state);
    state.local_dirty = vec![false; count];
    state.moved = vec![false; count];
    state.rest_world_rotation = vec![IDENTITY_QUATERNION; count];
    for position in 0..state.order.len() {
        let node = state.order[position] as usize;
        model.nodes[node].world = local_world(model, node);
        let parent = model.nodes[node].parent;
        let parent_rotation = if parent < 0 { IDENTITY_QUATERNION } else { state.rest_world_rotation[parent as usize] };
        state.rest_world_rotation[node] = quaternion_multiply(parent_rotation, model.nodes[node].rest_rotation);
    }
    state.last_params = vec![f32::NAN; model.param_values.len()];
    state.expression_weights = vec![0.0; model.expressions.len()];
    state.presets = [-1; PRESET_NAMES.len()];
    state.expression_category = vec![CATEGORY_NONE; model.expressions.len()];
    for (index, expression) in model.expressions.iter().enumerate() {
        let Some(preset) = PRESET_NAMES.iter().position(|name| expression.preset && *name == expression.name) else { continue };
        state.presets[preset] = index as i32;
        state.expression_category[index] = match preset {
            BLINK..=BLINK_RIGHT => CATEGORY_BLINK,
            LOOK_UP..=LOOK_RIGHT => CATEGORY_LOOK_AT,
            _ => CATEGORY_MOUTH,
        };
    }
    state.bones = vec![-1; BONE_NAMES.len()];
    let mut first = FIXED_PARAMS.len() + model.expressions.len();
    for (slot, name) in BONE_NAMES.iter().enumerate() {
        state.bones[slot] = model.bone(name).map_or(-1, |node| node as i32);
        if state.bones[slot] >= 0 {
            state.bone_params.push((slot as u32, first as u32));
            first += 3;
        }
    }
    state.hips_params = if state.bones[HIPS] >= 0 { first as i32 } else { -1 };
    state.morph_slot = vec![-1; count];
    let mut material_used = vec![false; model.material_rest.len()];
    for expression in &model.expressions {
        for bind in &expression.morph_binds {
            let node = bind.node as usize;
            if node < count && state.morph_slot[node] < 0 {
                state.morph_slot[node] = state.morph_nodes.len() as i32;
                state.morph_nodes.push(bind.node);
                state.morph_scratch.push(model.nodes[node].rest_weights.clone());
            }
        }
        for material in expression.color_binds.iter().map(|bind| bind.material).chain(expression.texture_transform_binds.iter().map(|bind| bind.material)) {
            if let Some(used) = material_used.get_mut(material as usize) {
                if !*used {
                    *used = true;
                    state.material_targets.push(material);
                }
            }
        }
    }
    state.material_scratch = model.material_rest.clone();
    let mut visiting = vec![0u8; model.constraints.len()];
    for constraint in 0..model.constraints.len() {
        visit_constraint(model, &mut state, &mut visiting, constraint, 0)?;
    }
    for (spring_index, spring) in model.springs.iter().enumerate() {
        for pair in spring.joints.windows(2) {
            let head = &model.nodes[pair[0].node as usize];
            let tail_world = model.nodes[pair[1].node as usize].world;
            let tail = [tail_world[12], tail_world[13], tail_world[14]];
            let center_inverse = if spring.center < 0 { IDENTITY_MATRIX } else { matrix_inverse(&model.nodes[spring.center as usize].world) };
            let offset = [tail[0] - head.world[12], tail[1] - head.world[13], tail[2] - head.world[14]];
            let initial_tail = transform_point(&center_inverse, tail);
            let bone_length = (offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2]).sqrt();
            if bone_length < MINIMUM_BONE_LENGTH {
                continue;
            }
            state.pairs.push(SpringPair {
                head: pair[0].node,
                spring: spring_index as u32,
                center: spring.center,
                hit_radius: pair[0].hit_radius,
                stiffness: pair[0].stiffness,
                gravity_power: pair[0].gravity_power,
                gravity_direction: pair[0].gravity_direction,
                drag_force: pair[0].drag_force,
                bone_axis: normalize(transform_point(&matrix_inverse(&head.world), tail)),
                bone_length,
                initial_local_matrix: matrix_compose(head.rest_translation, head.rest_rotation, head.rest_scale),
                initial_local_rotation: head.rest_rotation,
                previous_tail: initial_tail,
                current_tail: initial_tail,
            });
        }
    }
    state.pairs.sort_by_key(|pair| state.position[pair.head as usize]);
    for skin in &mut model.skins {
        skin.matrices = vec![0.0; skin.joints.len() * 16];
        for (index, joint) in skin.joints.iter().enumerate() {
            let matrix = matrix_multiply(&model.nodes[*joint as usize].world, &skin.inverse_bind_matrices[index]);
            skin.matrices[index * 16..index * 16 + 16].copy_from_slice(&matrix);
        }
    }
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    for data in &model.primitive_data {
        let rest_weights = &model.nodes[data.node as usize].rest_weights;
        let vertex_count = data.vertex_count as usize;
        for vertex in 0..vertex_count {
            let mut point = [data.positions[vertex * 3], data.positions[vertex * 3 + 1], data.positions[vertex * 3 + 2]];
            for target in 0..(data.target_count as usize).min(rest_weights.len()) {
                for component in 0..3 {
                    point[component] += rest_weights[target] * data.target_positions[(target * vertex_count + vertex) * 3 + component];
                }
            }
            let mut placed = [0.0; 3];
            if data.skin >= 0 && !data.joints.is_empty() {
                let matrices = &model.skins[data.skin as usize].matrices;
                for influence in 0..4 {
                    let weight = data.weights[vertex * 4 + influence];
                    if weight == 0.0 {
                        continue;
                    }
                    let joint = data.joints[vertex * 4 + influence] as usize;
                    let matrix: &Matrix = matrices[joint * 16..joint * 16 + 16].try_into().unwrap();
                    let skinned = transform_point(matrix, point);
                    for component in 0..3 {
                        placed[component] += skinned[component] * weight;
                    }
                }
            } else {
                placed = transform_point(&model.nodes[data.node as usize].world, point);
            }
            for component in 0..3 {
                minimum[component] = minimum[component].min(placed[component]);
                maximum[component] = maximum[component].max(placed[component]);
            }
        }
    }
    model.bounds = if minimum[0] <= maximum[0] { [minimum, maximum] } else { [[0.0; 3]; 2] };
    model.state = state;
    Ok(())
}

pub fn update(model: &mut Model, seconds: f32) -> u32 {
    let mut state = std::mem::take(&mut model.state);
    state.moved.fill(false);
    state.moved_any = false;
    let mut changed = 0;
    if model.param_values != state.last_params || model.bone_poses != state.last_bone_poses {
        state.last_params.copy_from_slice(&model.param_values);
        state.last_bone_poses.clone_from(&model.bone_poses);
        let look_at = &model.look_at;
        let ball_y = model.param_values[EYE_BALL_Y];
        let yaw = model.param_values[EYE_BALL_X] * look_at.horizontal_outer.input_max_value;
        let pitch = if ball_y > 0.0 { -ball_y * look_at.vertical_up.input_max_value } else { -ball_y * look_at.vertical_down.input_max_value };
        let look_at_factor = evaluate_expressions(model, &mut state, yaw, pitch);
        changed |= apply_morphs(model, &mut state);
        changed |= apply_materials(model, &mut state);
        apply_pose(model, &mut state, yaw, pitch, look_at_factor);
    }
    for position in 0..state.order.len() {
        let node = state.order[position] as usize;
        let parent = model.nodes[node].parent;
        if state.local_dirty[node] || (parent >= 0 && state.moved[parent as usize]) {
            state.local_dirty[node] = false;
            refresh_world(model, &mut state, node);
        }
    }
    for position in 0..state.constraint_order.len() {
        let constraint = state.constraint_order[position] as usize;
        apply_constraint(model, &mut state, constraint);
    }
    if seconds > 0.0 {
        state.time = (state.time + seconds).min(STEP * MAX_STEPS);
    }
    while state.time >= STEP {
        state.time -= STEP;
        for index in 0..state.pairs.len() {
            step_pair(model, &mut state, index);
        }
    }
    changed |= update_skins(model, &state);
    if state.moved_any {
        changed |= CHANGED_NODES;
    }
    model.state = state;
    changed
}
