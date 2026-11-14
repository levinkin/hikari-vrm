#![allow(dead_code)]

#[path = "../src/math.rs"]
mod math;
#[path = "../src/json.rs"]
mod json;
#[path = "../src/load.rs"]
mod load;
#[path = "../src/model.rs"]
mod model;
#[path = "../src/update.rs"]
mod update;

use std::io::Read;
use std::panic::catch_unwind;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

const BASE_JSON: &str = r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRMC_vrm"],"extensions":{"VRMC_vrm":{"specVersion":"1.0","humanoid":{"humanBones":{"hips":{"node":0}}},"expressions":{"preset":{"aa":{"morphTargetBinds":[{"node":0,"index":0,"weight":1}]}}}}},"buffers":[{"byteLength":60}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":6},{"buffer":0,"byteOffset":44,"byteLength":2},{"buffer":0,"byteOffset":48,"byteLength":12}],"accessors":[{"bufferView":0,"byteOffset":0,"componentType":5126,"count":3,"type":"VEC3"},{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"},{"componentType":5126,"count":3,"type":"VEC3","sparse":{"count":1,"indices":{"bufferView":2,"componentType":5123},"values":{"bufferView":3}}}],"meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1,"material":0,"targets":[{"POSITION":2}]}]}],"materials":[{"extensions":{"VRMC_materials_mtoon":{"specVersion":"1.0"}}}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}],"scene":0}"#;

fn base_bin() -> Vec<u8> {
    let mut bin = Vec::new();
    for value in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&value.to_le_bytes());
    }
    for index in [0u16, 1, 2, 0, 1, 0] {
        bin.extend_from_slice(&index.to_le_bytes());
    }
    for value in [0.0f32, 0.0, 1.0] {
        bin.extend_from_slice(&value.to_le_bytes());
    }
    bin
}

fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
    let mut json_bytes = json.as_bytes().to_vec();
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }
    let mut bin_bytes = bin.to_vec();
    while bin_bytes.len() % 4 != 0 {
        bin_bytes.push(0);
    }
    let total = 12 + 8 + json_bytes.len() + 8 + bin_bytes.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_bytes);
    out.extend_from_slice(&(bin_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin_bytes);
    out
}

fn summary(model: &model::Model) -> String {
    let triangles: usize = model.primitive_data.iter().map(|primitive| primitive.indices.len() / 3).sum();
    let joints: Vec<String> = model.skins.iter().map(|skin| skin.joints.len().to_string()).collect();
    let spring_joints: usize = model.springs.iter().map(|spring| spring.joints.len()).sum();
    let mut kinds = [0usize; 3];
    for material in &model.material_rest {
        kinds[material.kind as usize] += 1;
    }
    format!(
        "nodes {} primitives {} triangles {} skins {} joints [{}] expressions {} springs {} spring_joints {} colliders {} constraints {} materials pbr {} unlit {} mtoon {} textures {}",
        model.nodes.len(),
        model.primitive_data.len(),
        triangles,
        model.skins.len(),
        joints.join(","),
        model.expressions.len(),
        model.springs.len(),
        spring_joints,
        model.colliders.len(),
        model.constraints.len(),
        kinds[0],
        kinds[1],
        kinds[2],
        model.textures.len()
    )
}

fn check(label: &str, bytes: &[u8]) -> Option<bool> {
    let started = Instant::now();
    let result = catch_unwind(|| {
        let mut model = load::load(bytes)?;
        model::build_params(&mut model);
        update::prepare(&mut model)?;
        model::link_views(&mut model);
        Ok::<model::Model, String>(model)
    });
    let milliseconds = started.elapsed().as_secs_f64() * 1000.0;
    match result {
        Ok(Ok(model)) => {
            println!("{}: ok {:.2} ms {}", label, milliseconds, summary(&model));
            Some(true)
        }
        Ok(Err(message)) => {
            println!("{}: error: {}", label, message);
            Some(false)
        }
        Err(_) => {
            println!("{}: PANIC", label);
            None
        }
    }
}

fn hostile_cases() -> Vec<(String, Vec<u8>)> {
    let bin = base_bin();
    let valid = glb(BASE_JSON, &bin);
    let mut cases = Vec::new();
    for sixteenth in 0..16 {
        let truncated = valid[..valid.len() * sixteenth / 16].to_vec();
        cases.push((format!("truncated_{:02}_of_16", sixteenth), truncated.clone()));
        if truncated.len() >= 12 {
            let mut relabeled = truncated;
            let length = relabeled.len() as u32;
            relabeled[8..12].copy_from_slice(&length.to_le_bytes());
            cases.push((format!("truncated_{:02}_of_16_header_matches", sixteenth), relabeled));
        }
    }
    let replacements = [
        ("accessor_offset_out_of_range", r#""bufferView":0,"byteOffset":0,"#, r#""bufferView":0,"byteOffset":4000,"#),
        ("accessor_count_huge", r#""byteOffset":0,"componentType":5126,"count":3"#, r#""byteOffset":0,"componentType":5126,"count":4000000000"#),
        ("accessor_count_unbacked_over_cap", r#"{"bufferView":0,"byteOffset":0,"componentType":5126,"count":3"#, r#"{"componentType":5126,"count":100000000"#),
        ("buffer_view_length_huge", r#""byteOffset":0,"byteLength":36"#, r#""byteOffset":0,"byteLength":4294967295"#),
        ("external_buffer_uri", r#""buffers":[{"byteLength":60}]"#, r#""buffers":[{"byteLength":60,"uri":"model.bin"}]"#),
        ("data_buffer_uri", r#""buffers":[{"byteLength":60}]"#, r#""buffers":[{"byteLength":60,"uri":"data:application/octet-stream;base64,AAAA"}]"#),
        ("morph_bind_index_out_of_range", r#""index":0,"weight":1"#, r#""index":5,"weight":1"#),
        ("vrm_0x", r#""VRMC_vrm":"#, r#""VRM":"#),
        ("no_vrm", r#""VRMC_vrm":"#, r#""OTHER":"#),
        ("translation_overflows_f32", r#""nodes":[{"mesh":0}]"#, r#""nodes":[{"mesh":0,"translation":[1e39,0,0]}]"#),
        ("double_sided_not_bool", r#""materials":[{"extensions""#, r#""materials":[{"doubleSided":1,"extensions""#),
        ("is_binary_null", r#""aa":{"morphTargetBinds""#, r#""aa":{"isBinary":null,"morphTargetBinds""#),
    ];
    for (name, from, to) in replacements {
        cases.push((name.to_string(), glb(&BASE_JSON.replace(from, to), &bin)));
    }
    let mut nan = bin.clone();
    nan[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
    cases.push(("float_accessor_nan".to_string(), glb(BASE_JSON, &nan)));
    let mut sparse = bin.clone();
    sparse[44..46].copy_from_slice(&7u16.to_le_bytes());
    cases.push(("sparse_index_out_of_range".to_string(), glb(BASE_JSON, &sparse)));
    let depth = 100000;
    let deep = format!("{},\"extras\":{}{}}}", &BASE_JSON[..BASE_JSON.len() - 1], "[".repeat(depth), "]".repeat(depth));
    cases.push(("deep_json_nesting".to_string(), glb(&deep, &bin)));
    let mut json_chunk = valid.clone();
    json_chunk[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(("json_chunk_length_4gib".to_string(), json_chunk));
    let mut bin_chunk = valid.clone();
    let bin_header = 20 + u32::from_le_bytes([valid[12], valid[13], valid[14], valid[15]]) as usize;
    bin_chunk[bin_header..bin_header + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(("bin_chunk_length_4gib".to_string(), bin_chunk));
    let mut header = valid.clone();
    header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(("glb_length_4gib".to_string(), header));
    cases
}

fn run_hostile(directory: &Path) -> bool {
    if let Err(error) = std::fs::create_dir_all(directory) {
        println!("{}: error: {}", directory.display(), error);
        return false;
    }
    let mut passed = true;
    let valid_path = directory.join("valid_base.glb");
    let written = std::fs::write(&valid_path, glb(BASE_JSON, &base_bin())).and_then(|_| std::fs::read(&valid_path));
    match written {
        Ok(bytes) => passed &= check("valid_base", &bytes) == Some(true),
        Err(error) => {
            println!("valid_base: error: {}", error);
            passed = false;
        }
    }
    for (name, bytes) in hostile_cases() {
        let path = directory.join(format!("{}.glb", name));
        let loaded = std::fs::write(&path, &bytes).and_then(|_| std::fs::read(&path));
        match loaded {
            Ok(bytes) => passed &= check(&name, &bytes) == Some(false),
            Err(error) => {
                println!("{}: error: {}", name, error);
                passed = false;
            }
        }
    }
    passed
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut passed = true;
    let mut position = 0;
    while position < arguments.len() {
        if arguments[position] == "--hostile" && position + 1 < arguments.len() {
            passed &= run_hostile(Path::new(&arguments[position + 1]));
            position += 2;
            continue;
        }
        let path = &arguments[position];
        let mut bytes = Vec::new();
        let read = std::fs::File::open(path).and_then(|file| file.take(load::MAX_DECODED_BYTES as u64 + 1).read_to_end(&mut bytes));
        match read.map(|_| bytes) {
            Ok(bytes) => passed &= check(path, &bytes) == Some(true),
            Err(error) => {
                println!("{}: error: {}", path, error);
                passed = false;
            }
        }
        position += 1;
    }
    if passed { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
