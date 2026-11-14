use crate::motion::{read_capped, BoneTrack, Curve, Motion, MAX_FILE_BYTES};
use serde_json::Value;
use std::path::Path;

const MAX_VALUES: usize = 1 << 22;
const IDENTITY: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const EYE_RANGE_DEGREES: f64 = 90.0;

fn node_rotation(node: &Value) -> [f32; 4] {
    match node["rotation"].as_array() {
        Some(row) if row.len() == 4 => [0, 1, 2, 3].map(|axis| row[axis].as_f64().map_or(IDENTITY[axis], |value| value as f32)),
        _ => IDENTITY,
    }
}

fn node_translation(node: &Value) -> [f32; 3] {
    match node["translation"].as_array() {
        Some(row) if row.len() == 3 => [0, 1, 2].map(|axis| row[axis].as_f64().unwrap_or(0.0) as f32),
        _ => [0.0; 3],
    }
}

fn world_rotation(nodes: &[Value], parents: &[usize], mut node: usize) -> [f32; 4] {
    let mut rotation = IDENTITY;
    let mut depth = 0;
    while node < nodes.len() && depth <= nodes.len() {
        rotation = hv::quaternion_multiply(node_rotation(&nodes[node]), rotation);
        node = parents[node];
        depth += 1;
    }
    rotation
}

fn parse(path: &Path, name: String) -> Result<Motion, String> {
    let bytes = read_capped(path, MAX_FILE_BYTES)?;
    let (json_chunk, _) = hv::split_glb(&bytes)?;
    let document: Value = serde_json::from_slice(json_chunk).map_err(|error| format!("glb json: {}", error))?;
    let extension = &document["extensions"]["VRMC_vrm_animation"];
    if !extension.is_object() {
        return Err("no VRMC_vrm_animation extension".to_string());
    }
    let nodes = document["nodes"].as_array().map_or(&[][..], Vec::as_slice);
    let mut parents = vec![usize::MAX; nodes.len()];
    for (parent, node) in nodes.iter().enumerate() {
        let rotation = node_rotation(node);
        if rotation.iter().chain(&node_translation(node)).any(|value| !value.is_finite()) || rotation.iter().map(|value| value * value).sum::<f32>() <= 0.0 {
            return Err(format!("node {} rest transform is not finite or has a zero rotation", parent));
        }
        for child in node["children"].as_array().into_iter().flatten() {
            let child = child.as_u64().and_then(|number| usize::try_from(number).ok()).ok_or("node child must be an index")?;
            *parents.get_mut(child).ok_or_else(|| format!("node {} child {} is out of range", parent, child))? = parent;
        }
    }
    let mut targets: Vec<(usize, Option<usize>, String)> = Vec::new();
    for (bone, entry) in extension["humanoid"]["humanBones"].as_object().into_iter().flatten() {
        if let Some(slot) = hv::BONE_NAMES.iter().position(|known| known == bone) {
            let node = entry["node"].as_u64().and_then(|number| usize::try_from(number).ok()).ok_or_else(|| format!("{} must be an index", bone))?;
            targets.push((node, Some(slot), bone.clone()));
        }
    }
    for kind in ["preset", "custom"] {
        for (expression, entry) in extension["expressions"][kind].as_object().into_iter().flatten() {
            let node = entry["node"].as_u64().and_then(|number| usize::try_from(number).ok()).ok_or_else(|| format!("{} must be an index", expression))?;
            targets.push((node, None, expression.clone()));
        }
    }
    if !extension["lookAt"].is_null() {
        let node = extension["lookAt"]["node"].as_u64().and_then(|number| usize::try_from(number).ok()).ok_or("lookAt node must be an index")?;
        targets.push((node, None, "lookAt".to_string()));
    }
    if let Some((node, _, target)) = targets.iter().find(|target| target.0 >= nodes.len()) {
        return Err(format!("{} node {} is out of range ({} nodes)", target, node, nodes.len()));
    }
    let animation = &document["animations"][0];
    let samplers = animation["samplers"].as_array().ok_or("no animation")?;
    let mut wanted = Vec::new();
    let mut channels = Vec::new();
    for channel in animation["channels"].as_array().into_iter().flatten() {
        let node = channel["target"]["node"].as_u64();
        let path = channel["target"]["path"].as_str().unwrap_or("");
        let Some(target) = targets.iter().position(|target| Some(target.0 as u64) == node) else { continue };
        let width = match (targets[target].1, path) {
            (Some(_), "rotation") => 4,
            (Some(_), "translation") if targets[target].2 == "hips" => 3,
            (None, "rotation") if targets[target].2 == "lookAt" => 4,
            (None, "translation") => 3,
            _ => continue,
        };
        let sampler = samplers.get(channel["sampler"].as_u64().and_then(|number| usize::try_from(number).ok()).ok_or("channel sampler must be an index")?).ok_or("channel sampler is out of range")?;
        let mode = match sampler["interpolation"].as_str().unwrap_or("LINEAR") {
            "LINEAR" => 0,
            "STEP" => 1,
            "CUBICSPLINE" => 2,
            other => return Err(format!("unknown interpolation {}", other)),
        };
        wanted.push((sampler["input"].as_u64().and_then(|number| usize::try_from(number).ok()).ok_or("sampler input must be an index")?, 1));
        wanted.push((sampler["output"].as_u64().and_then(|number| usize::try_from(number).ok()).ok_or("sampler output must be an index")?, width));
        channels.push((target, width, mode));
    }
    if channels.is_empty() {
        return Err("no humanoid, expression or lookAt tracks".to_string());
    }
    let mut floats = hv::glb_floats(&bytes, &wanted, MAX_VALUES)?.into_iter();
    let mut duration: f32 = 0.0;
    let mut curves = Vec::new();
    let mut bones = Vec::new();
    let mut look_at = None;
    for (target, width, mode) in channels {
        let (Some(times), Some(values)) = (floats.next(), floats.next()) else { break };
        let (node, slot, name) = &targets[target];
        let stride = if mode == 2 { 3 } else { 1 };
        if values.len() != times.len() * width * stride {
            return Err(format!("{} keyframe counts do not match", name));
        }
        if values.iter().chain(&times).any(|value| !value.is_finite()) || times.windows(2).any(|pair| pair[1] <= pair[0]) || times[0] < 0.0 {
            return Err(format!("{} keyframes are not finite and increasing", name));
        }
        duration = duration.max(times[times.len() - 1]);
        let parent = world_rotation(nodes, &parents, parents[*node]);
        match (slot, width) {
            (Some(slot), 4) => {
                let rest = world_rotation(nodes, &parents, *node);
                let rest_inverse = [-rest[0], -rest[1], -rest[2], rest[3]];
                let keys: Vec<[f32; 4]> = values
                    .chunks_exact(4)
                    .enumerate()
                    .map(|(key, local)| {
                        let normalized = hv::quaternion_multiply(hv::quaternion_multiply(parent, [local[0], local[1], local[2], local[3]]), rest_inverse);
                        if key % stride == stride / 2 { hv::quaternion_normalize(normalized) } else { normalized }
                    })
                    .collect();
                if keys.iter().flatten().any(|value| !value.is_finite()) {
                    return Err(format!("{} retargeted keyframes are not finite", name));
                }
                bones.push((*slot, BoneTrack { times, keys, mode }));
            }
            (Some(_), _) => {
                let rest = node_translation(&nodes[*node]);
                let offsets: Vec<[f32; 3]> = values
                    .chunks_exact(3)
                    .enumerate()
                    .map(|(key, local)| {
                        let shift = if key % stride == stride / 2 { rest } else { [0.0; 3] };
                        hv::quaternion_rotate(parent, [local[0] - shift[0], local[1] - shift[1], local[2] - shift[2]])
                    })
                    .collect();
                if offsets.iter().flatten().any(|value| !value.is_finite()) {
                    return Err(format!("{} retargeted keyframes are not finite", name));
                }
                for (axis, id) in hv::HIPS_POSITION_PARAMS.iter().enumerate() {
                    let keys: Vec<f64> = offsets.iter().map(|offset| offset[axis] as f64).collect();
                    curves.push(Curve::keyed(id.to_string(), &times, &keys, mode));
                }
            }
            (None, 4) => {
                let keys: Vec<[f32; 4]> = values
                    .chunks_exact(4)
                    .enumerate()
                    .map(|(key, local)| if key % stride == stride / 2 { hv::quaternion_normalize([local[0], local[1], local[2], local[3]]) } else { [local[0], local[1], local[2], local[3]] })
                    .collect();
                if keys.iter().flatten().any(|value| !value.is_finite()) {
                    return Err(format!("{} keyframes are not finite", name));
                }
                look_at = Some(BoneTrack { times, keys, mode });
            }
            _ => {
                let keys: Vec<f64> = values.chunks_exact(3).enumerate().map(|(key, local)| if key % stride == stride / 2 { local[0].clamp(0.0, 1.0) as f64 } else { local[0] as f64 }).collect();
                curves.push(Curve::keyed(name.clone(), &times, &keys, mode));
            }
        }
    }
    Ok(Motion { name, duration: duration as f64, fade_in: 0.3, fade_out: 0.3, curves, bones, look_at })
}

pub fn gaze(rotation: [f32; 4]) -> [f64; 2] {
    let forward = hv::quaternion_rotate(rotation, [0.0, 0.0, 1.0]);
    [forward[0].atan2(forward[2]).to_degrees() as f64 / EYE_RANGE_DEGREES, forward[1].clamp(-1.0, 1.0).asin().to_degrees() as f64 / EYE_RANGE_DEGREES]
}

pub fn load(path: &Path) -> Result<Motion, String> {
    let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let name = file_name.strip_suffix(".vrma").unwrap_or(&file_name).to_string();
    parse(path, name).map_err(|line| format!("{}: {}", file_name, line))
}
