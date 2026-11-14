use crate::json::{self, NULL, Value};
use crate::math::{IDENTITY_MATRIX, matrix_rotation};
use crate::model::{
    ALPHA_BLEND, ALPHA_MASK, BONE_NAMES, ALPHA_OPAQUE, Axis, ColorBind, ColorTarget, Collider, ColliderShape, Constraint, ConstraintKind, Expression, Image, LookAt,
    LookAtKind, MATERIAL_MTOON, MATERIAL_PBR, MATERIAL_UNLIT, MIME_JPEG, MIME_PNG, Material, Model, MorphBind, Node, Param, OUTLINE_NONE, OUTLINE_SCREEN,
    OUTLINE_WORLD, Override, Primitive, PrimitiveData, RangeMap, Skin, Spring, SpringJoint, TextureRef, TextureSource, TextureTransformBind,
};
use crate::update::State;

pub const MAX_DECODED_BYTES: usize = 512 << 20;
const CHUNK_JSON: u32 = 0x4E4F534A;
const CHUNK_BIN: u32 = 0x004E4942;

struct View {
    start: usize,
    length: usize,
    stride: usize,
}

struct Sparse {
    count: usize,
    indices_start: usize,
    index_component: u32,
    values_start: usize,
}

struct Accessor {
    start: Option<usize>,
    stride: usize,
    component: u32,
    component_size: usize,
    components: usize,
    count: usize,
    normalized: bool,
    sparse: Option<Sparse>,
}

struct Gltf<'a> {
    bin: &'a [u8],
    accessors: Vec<Accessor>,
}

pub fn split_glb(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    if bytes.len() > MAX_DECODED_BYTES {
        return Err("glb file exceeds the 512 MiB size cap".to_string());
    }
    if bytes.len() < 12 || &bytes[0..4] != b"glTF" {
        return Err("not a glb file (bad magic or too short)".to_string());
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != 2 {
        return Err(format!("glb version {} is not supported, expected 2", version));
    }
    let total = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if total < 12 || total > bytes.len() {
        return Err(format!("glb header claims {} bytes but the file has {}", total, bytes.len()));
    }
    let mut json_chunk = None;
    let mut bin_chunk: Option<&[u8]> = None;
    let mut position = 12;
    while position < total {
        if total - position < 8 {
            return Err("glb chunk header is truncated".to_string());
        }
        let length = u32::from_le_bytes([bytes[position], bytes[position + 1], bytes[position + 2], bytes[position + 3]]) as usize;
        let kind = u32::from_le_bytes([bytes[position + 4], bytes[position + 5], bytes[position + 6], bytes[position + 7]]);
        let start = position + 8;
        let end = start.checked_add(length).filter(|end| *end <= total).ok_or_else(|| format!("glb chunk claims {} bytes past the end of the file", length))?;
        if json_chunk.is_none() && kind != CHUNK_JSON {
            return Err("first glb chunk is not json".to_string());
        }
        match kind {
            CHUNK_JSON if json_chunk.is_some() => return Err("glb has two json chunks".to_string()),
            CHUNK_JSON => json_chunk = Some(&bytes[start..end]),
            CHUNK_BIN if bin_chunk.is_some() => return Err("glb has two bin chunks".to_string()),
            CHUNK_BIN => bin_chunk = Some(&bytes[start..end]),
            _ => {}
        }
        position = end;
    }
    let json_chunk = json_chunk.ok_or("glb has no json chunk")?;
    Ok((json_chunk, bin_chunk.unwrap_or(&[])))
}

fn image_pixels(bytes: &[u8]) -> Option<usize> {
    let (width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16)? == b"IHDR" {
        let size = bytes.get(16..24)?;
        (u32::from_be_bytes([size[0], size[1], size[2], size[3]]) as usize, u32::from_be_bytes([size[4], size[5], size[6], size[7]]) as usize)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut position = 2;
        loop {
            let header = bytes.get(position..position + 4)?;
            let marker = header[1];
            if header[0] != 0xFF || marker == 0xD9 || marker == 0xDA {
                return None;
            }
            if marker == 0xFF {
                position += 1;
                continue;
            }
            if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                position += 2;
                continue;
            }
            if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                let size = bytes.get(position + 5..position + 9)?;
                break (u16::from_be_bytes([size[2], size[3]]) as usize, u16::from_be_bytes([size[0], size[1]]) as usize);
            }
            position += 2 + u16::from_be_bytes([header[2], header[3]]).max(2) as usize;
        }
    } else {
        return None;
    };
    width.checked_mul(height).filter(|pixels| *pixels > 0)
}

fn integer(value: &Value, what: &str) -> Result<usize, String> {
    match value.number() {
        Some(number) if number >= 0.0 && number.fract() == 0.0 && number <= 9007199254740992.0 => Ok(number as usize),
        _ => Err(format!("{} is missing or not a non-negative integer", what)),
    }
}

fn integer_or(value: &Value, default: usize, what: &str) -> Result<usize, String> {
    if value.is_null() {
        return Ok(default);
    }
    integer(value, what)
}

fn index(value: &Value, limit: usize, what: &str) -> Result<usize, String> {
    let number = integer(value, what)?;
    if number >= limit {
        return Err(format!("{} index {} is out of range (count {})", what, number, limit));
    }
    Ok(number)
}

fn optional_index(value: &Value, limit: usize, what: &str) -> Result<i32, String> {
    if value.is_null() {
        return Ok(-1);
    }
    Ok(index(value, limit, what)? as i32)
}

fn flag(value: &Value, what: &str) -> Result<bool, String> {
    if std::ptr::eq(value, &NULL) {
        return Ok(false);
    }
    value.boolean().ok_or_else(|| format!("{} is not true or false", what))
}

fn float(value: &Value, default: f32) -> f32 {
    match value.number() {
        Some(number) => number as f32,
        None => default,
    }
}

fn floats<const N: usize>(value: &Value, default: [f32; N]) -> [f32; N] {
    let items = value.array();
    let mut result = default;
    if items.len() != N {
        return result;
    }
    for (slot, item) in result.iter_mut().zip(items) {
        match item.number() {
            Some(number) => *slot = number as f32,
            None => return default,
        }
    }
    result
}

fn grow(total: usize, count: usize, unit: usize) -> Result<usize, String> {
    count
        .checked_mul(unit)
        .and_then(|bytes| total.checked_add(bytes))
        .filter(|sum| *sum <= MAX_DECODED_BYTES)
        .ok_or_else(|| "model exceeds the 512 MiB decoded size cap".to_string())
}

fn parse_views(document: &Value, bin_length: usize) -> Result<Vec<View>, String> {
    let buffers = document.get("buffers").array();
    for (number, buffer) in buffers.iter().enumerate() {
        match buffer.get("uri").string() {
            Some(uri) if uri.starts_with("data:") => return Err(format!("buffer {} uses a data uri, only the glb bin chunk is supported", number)),
            Some(_) => return Err(format!("buffer {} uses an external uri, only the glb bin chunk is supported", number)),
            None => {}
        }
    }
    if buffers.len() > 1 {
        return Err("only one buffer (the glb bin chunk) is supported".to_string());
    }
    let buffer_length = match buffers.first() {
        Some(buffer) => integer(buffer.get("byteLength"), "buffer byteLength")?,
        None => 0,
    };
    if buffer_length > bin_length {
        return Err(format!("buffer claims {} bytes but the bin chunk has {}", buffer_length, bin_length));
    }
    let mut views = Vec::new();
    for (number, view) in document.get("bufferViews").array().iter().enumerate() {
        index(view.get("buffer"), buffers.len(), "bufferView buffer")?;
        let offset = integer_or(view.get("byteOffset"), 0, "bufferView byteOffset")?;
        let length = integer(view.get("byteLength"), "bufferView byteLength")?;
        if offset.checked_add(length).is_none_or(|end| end > buffer_length) {
            return Err(format!("bufferView {} range {}+{} exceeds the {} byte buffer", number, offset, length, buffer_length));
        }
        let stride = integer_or(view.get("byteStride"), 0, "bufferView byteStride")?;
        if stride != 0 && (!(4..=252).contains(&stride) || stride % 4 != 0) {
            return Err(format!("bufferView {} has invalid byteStride {}", number, stride));
        }
        views.push(View { start: offset, length, stride });
    }
    Ok(views)
}

fn component_size(component: u32) -> Option<usize> {
    match component {
        5120 | 5121 => Some(1),
        5122 | 5123 => Some(2),
        5125 | 5126 => Some(4),
        _ => None,
    }
}

fn index_size(component: u32) -> Option<usize> {
    match component {
        5121 => Some(1),
        5123 => Some(2),
        5125 => Some(4),
        _ => None,
    }
}

fn read_integer(bin: &[u8], at: usize, component: u32) -> u32 {
    match component {
        5120 | 5121 => bin[at] as u32,
        5122 | 5123 => u16::from_le_bytes([bin[at], bin[at + 1]]) as u32,
        _ => u32::from_le_bytes([bin[at], bin[at + 1], bin[at + 2], bin[at + 3]]),
    }
}

fn read_float(bin: &[u8], at: usize, component: u32, normalized: bool) -> f32 {
    match component {
        5120 => {
            let value = bin[at] as i8 as f32;
            if normalized { (value / 127.0).max(-1.0) } else { value }
        }
        5121 => {
            let value = bin[at] as f32;
            if normalized { value / 255.0 } else { value }
        }
        5122 => {
            let value = i16::from_le_bytes([bin[at], bin[at + 1]]) as f32;
            if normalized { (value / 32767.0).max(-1.0) } else { value }
        }
        5123 => {
            let value = u16::from_le_bytes([bin[at], bin[at + 1]]) as f32;
            if normalized { value / 65535.0 } else { value }
        }
        5125 => u32::from_le_bytes([bin[at], bin[at + 1], bin[at + 2], bin[at + 3]]) as f32,
        _ => f32::from_bits(u32::from_le_bytes([bin[at], bin[at + 1], bin[at + 2], bin[at + 3]])),
    }
}

fn region(view: &View, offset: usize, length: Option<usize>, what: &str) -> Result<usize, String> {
    let end = length.and_then(|length| offset.checked_add(length));
    if end.is_none_or(|end| end > view.length) {
        return Err(format!("{} reads past the end of its bufferView ({} bytes)", what, view.length));
    }
    Ok(view.start + offset)
}

fn parse_accessors(document: &Value, views: &[View], bin: &[u8]) -> Result<Vec<Accessor>, String> {
    let mut accessors = Vec::new();
    for (number, accessor) in document.get("accessors").array().iter().enumerate() {
        let what = format!("accessor {}", number);
        let component = integer(accessor.get("componentType"), "accessor componentType")?;
        let component = u32::try_from(component).map_err(|_| format!("{} has unknown componentType {}", what, component))?;
        let size = component_size(component).ok_or_else(|| format!("{} has unknown componentType {}", what, component))?;
        let components = match accessor.get("type").string() {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            Some("MAT2") if size == 4 => 4,
            Some("MAT3") if size == 4 => 9,
            Some("MAT4") => 16,
            _ => return Err(format!("{} has an unsupported type", what)),
        };
        let count = integer(accessor.get("count"), "accessor count")?;
        if count == 0 || count > u32::MAX as usize {
            return Err(format!("{} count {} is out of range", what, count));
        }
        let element = components * size;
        let (start, stride) = if accessor.get("bufferView").is_null() {
            (None, element)
        } else {
            let view = &views[index(accessor.get("bufferView"), views.len(), "accessor bufferView")?];
            let offset = integer_or(accessor.get("byteOffset"), 0, "accessor byteOffset")?;
            let stride = if view.stride == 0 { element } else { view.stride };
            if stride < element {
                return Err(format!("{} elements are wider than the bufferView stride", what));
            }
            let span = (count - 1).checked_mul(stride).and_then(|span| span.checked_add(element));
            (Some(region(view, offset, span, &what)?), stride)
        };
        let sparse_json = accessor.get("sparse");
        let sparse = if sparse_json.is_null() {
            None
        } else {
            let sparse_count = integer(sparse_json.get("count"), "sparse count")?;
            if sparse_count == 0 || sparse_count > count {
                return Err(format!("{} sparse count {} is out of range", what, sparse_count));
            }
            let indices = sparse_json.get("indices");
            let index_component = integer(indices.get("componentType"), "sparse indices componentType")?;
            let index_component = u32::try_from(index_component).unwrap_or(0);
            let index_bytes = index_size(index_component).ok_or_else(|| format!("{} sparse indices have an invalid componentType", what))?;
            let indices_view = &views[index(indices.get("bufferView"), views.len(), "sparse indices bufferView")?];
            let indices_offset = integer_or(indices.get("byteOffset"), 0, "sparse indices byteOffset")?;
            let indices_start = region(indices_view, indices_offset, sparse_count.checked_mul(index_bytes), &what)?;
            let values = sparse_json.get("values");
            let values_view = &views[index(values.get("bufferView"), views.len(), "sparse values bufferView")?];
            let values_offset = integer_or(values.get("byteOffset"), 0, "sparse values byteOffset")?;
            let values_start = region(values_view, values_offset, sparse_count.checked_mul(element), &what)?;
            let mut previous = None;
            for entry in 0..sparse_count {
                let target = read_integer(bin, indices_start + entry * index_bytes, index_component) as usize;
                if target >= count || previous.is_some_and(|last| target <= last) {
                    return Err(format!("{} sparse index {} is out of range or not increasing (count {})", what, target, count));
                }
                previous = Some(target);
            }
            Some(Sparse { count: sparse_count, indices_start, index_component, values_start })
        };
        let normalized = flag(accessor.get("normalized"), "accessor normalized")?;
        accessors.push(Accessor { start, stride, component, component_size: size, components, count, normalized, sparse });
    }
    Ok(accessors)
}

impl Gltf<'_> {
    fn accessor(&self, value: &Value, components: usize, what: &str) -> Result<&Accessor, String> {
        let number = index(value, self.accessors.len(), what)?;
        let accessor = &self.accessors[number];
        if accessor.components != components {
            return Err(format!("{} accessor {} has {} components, expected {}", what, number, accessor.components, components));
        }
        Ok(accessor)
    }

    fn integer_accessor(&self, value: &Value, components: usize, what: &str) -> Result<&Accessor, String> {
        let accessor = self.accessor(value, components, what)?;
        if index_size(accessor.component).is_none() || (components == 4 && accessor.component == 5125) {
            return Err(format!("{} accessor has an invalid componentType {}", what, accessor.component));
        }
        Ok(accessor)
    }

    fn floats(&self, accessor: &Accessor) -> Result<Vec<f32>, String> {
        let components = accessor.components;
        let size = accessor.component_size;
        let mut output = vec![0.0f32; accessor.count * components];
        if let Some(start) = accessor.start {
            for (element, values) in output.chunks_exact_mut(components).enumerate() {
                let base = start + element * accessor.stride;
                for (component, value) in values.iter_mut().enumerate() {
                    *value = read_float(self.bin, base + component * size, accessor.component, accessor.normalized);
                }
            }
        }
        if let Some(sparse) = &accessor.sparse {
            let index_bytes = index_size(sparse.index_component).unwrap_or(4);
            for entry in 0..sparse.count {
                let target = read_integer(self.bin, sparse.indices_start + entry * index_bytes, sparse.index_component) as usize;
                let base = sparse.values_start + entry * components * size;
                for component in 0..components {
                    output[target * components + component] = read_float(self.bin, base + component * size, accessor.component, accessor.normalized);
                }
            }
        }
        if !output.iter().all(|value| value.is_finite()) {
            return Err("float accessor has a non-finite value".to_string());
        }
        Ok(output)
    }

    fn check_integers(&self, accessor: &Accessor, limit: usize, what: &str) -> Result<(), String> {
        let size = accessor.component_size;
        let mut largest = 0u32;
        let mut replaced = 0;
        if let Some(start) = accessor.start {
            for element in 0..accessor.count {
                if let Some(sparse) = &accessor.sparse {
                    let index_bytes = index_size(sparse.index_component).unwrap_or(4);
                    if replaced < sparse.count && read_integer(self.bin, sparse.indices_start + replaced * index_bytes, sparse.index_component) as usize == element {
                        replaced += 1;
                        continue;
                    }
                }
                for component in 0..accessor.components {
                    largest = largest.max(read_integer(self.bin, start + element * accessor.stride + component * size, accessor.component));
                }
            }
        }
        if let Some(sparse) = &accessor.sparse {
            for value in 0..sparse.count * accessor.components {
                largest = largest.max(read_integer(self.bin, sparse.values_start + value * size, accessor.component));
            }
        }
        if largest as usize >= limit {
            return Err(format!("{} value {} is out of range (limit {})", what, largest, limit));
        }
        Ok(())
    }

    fn integers(&self, accessor: &Accessor) -> Vec<u32> {
        let components = accessor.components;
        let size = accessor.component_size;
        let mut output = vec![0u32; accessor.count * components];
        if let Some(start) = accessor.start {
            for (element, values) in output.chunks_exact_mut(components).enumerate() {
                let base = start + element * accessor.stride;
                for (component, value) in values.iter_mut().enumerate() {
                    *value = read_integer(self.bin, base + component * size, accessor.component);
                }
            }
        }
        if let Some(sparse) = &accessor.sparse {
            let index_bytes = index_size(sparse.index_component).unwrap_or(4);
            for entry in 0..sparse.count {
                let target = read_integer(self.bin, sparse.indices_start + entry * index_bytes, sparse.index_component) as usize;
                let base = sparse.values_start + entry * components * size;
                for component in 0..components {
                    output[target * components + component] = read_integer(self.bin, base + component * size, accessor.component);
                }
            }
        }
        output
    }
}

fn decompose(matrix: [f32; 16]) -> ([f32; 3], [f32; 4], [f32; 3]) {
    let translation = [matrix[12], matrix[13], matrix[14]];
    let mut scale = [0.0f32; 3];
    for (column, slot) in scale.iter_mut().enumerate() {
        let base = column * 4;
        *slot = (matrix[base] * matrix[base] + matrix[base + 1] * matrix[base + 1] + matrix[base + 2] * matrix[base + 2]).sqrt();
    }
    let determinant = matrix[0] * (matrix[5] * matrix[10] - matrix[9] * matrix[6]) - matrix[4] * (matrix[1] * matrix[10] - matrix[9] * matrix[2])
        + matrix[8] * (matrix[1] * matrix[6] - matrix[5] * matrix[2]);
    if determinant < 0.0 {
        scale[0] = -scale[0];
    }
    if scale.contains(&0.0) {
        return (translation, [0.0, 0.0, 0.0, 1.0], scale);
    }
    let mut rotation = matrix;
    if scale[0] < 0.0 {
        for value in &mut rotation[0..3] {
            *value = -*value;
        }
    }
    (translation, matrix_rotation(&rotation), scale)
}

fn texture_ref(value: &Value, texture_count: usize) -> Result<TextureRef, String> {
    let mut reference = TextureRef::default();
    if value.is_null() {
        return Ok(reference);
    }
    reference.texture = index(value.get("index"), texture_count, "material texture")? as i32;
    reference.tex_coord = integer_or(value.get("texCoord"), 0, "texCoord")?.min(i32::MAX as usize) as i32;
    let transform = value.get("extensions").get("KHR_texture_transform");
    if !transform.is_null() {
        reference.offset = floats(transform.get("offset"), [0.0, 0.0]);
        reference.scale = floats(transform.get("scale"), [1.0, 1.0]);
        reference.rotation = float(transform.get("rotation"), 0.0);
        if !transform.get("texCoord").is_null() {
            reference.tex_coord = integer(transform.get("texCoord"), "KHR_texture_transform texCoord")?.min(i32::MAX as usize) as i32;
        }
    }
    Ok(reference)
}

fn parse_material(material: &Value, texture_count: usize) -> Result<Material, String> {
    let pbr = material.get("pbrMetallicRoughness");
    let extensions = material.get("extensions");
    let mtoon = extensions.get("VRMC_materials_mtoon");
    let emissive_strength = float(extensions.get("KHR_materials_emissive_strength").get("emissiveStrength"), 1.0);
    let emissive = floats(material.get("emissiveFactor"), [0.0, 0.0, 0.0]).map(|value| value * emissive_strength);
    let normal = material.get("normalTexture");
    let occlusion = material.get("occlusionTexture");
    let shading_shift = mtoon.get("shadingShiftTexture");
    Ok(Material {
        kind: if !mtoon.is_null() {
            MATERIAL_MTOON
        } else if !extensions.get("KHR_materials_unlit").is_null() {
            MATERIAL_UNLIT
        } else {
            MATERIAL_PBR
        },
        alpha_mode: match material.get("alphaMode").string() {
            None | Some("OPAQUE") => ALPHA_OPAQUE,
            Some("MASK") => ALPHA_MASK,
            Some("BLEND") => ALPHA_BLEND,
            Some(other) => return Err(format!("unknown alphaMode {}", other)),
        },
        alpha_cutoff: float(material.get("alphaCutoff"), 0.5),
        double_sided: flag(material.get("doubleSided"), "material doubleSided")?,
        transparent_with_z_write: flag(mtoon.get("transparentWithZWrite"), "mtoon transparentWithZWrite")?,
        render_queue_offset: float(mtoon.get("renderQueueOffsetNumber"), 0.0) as i32,
        base_color: floats(pbr.get("baseColorFactor"), [1.0, 1.0, 1.0, 1.0]),
        base_color_texture: texture_ref(pbr.get("baseColorTexture"), texture_count)?,
        emissive,
        emissive_texture: texture_ref(material.get("emissiveTexture"), texture_count)?,
        normal_texture: texture_ref(normal, texture_count)?,
        normal_scale: float(normal.get("scale"), 1.0),
        metallic: float(pbr.get("metallicFactor"), 1.0),
        roughness: float(pbr.get("roughnessFactor"), 1.0),
        metallic_roughness_texture: texture_ref(pbr.get("metallicRoughnessTexture"), texture_count)?,
        occlusion_texture: texture_ref(occlusion, texture_count)?,
        occlusion_strength: float(occlusion.get("strength"), 1.0),
        shade_color: floats(mtoon.get("shadeColorFactor"), [0.0, 0.0, 0.0]),
        shade_multiply_texture: texture_ref(mtoon.get("shadeMultiplyTexture"), texture_count)?,
        shading_shift: float(mtoon.get("shadingShiftFactor"), 0.0),
        shading_shift_texture: texture_ref(shading_shift, texture_count)?,
        shading_shift_texture_scale: float(shading_shift.get("scale"), 1.0),
        shading_toony: float(mtoon.get("shadingToonyFactor"), 0.9),
        gi_equalization: float(mtoon.get("giEqualizationFactor"), 0.9),
        matcap_color: floats(mtoon.get("matcapFactor"), [1.0, 1.0, 1.0]),
        matcap_texture: texture_ref(mtoon.get("matcapTexture"), texture_count)?,
        rim_color: floats(mtoon.get("parametricRimColorFactor"), [0.0, 0.0, 0.0]),
        rim_multiply_texture: texture_ref(mtoon.get("rimMultiplyTexture"), texture_count)?,
        rim_lighting_mix: float(mtoon.get("rimLightingMixFactor"), 1.0),
        rim_fresnel_power: float(mtoon.get("parametricRimFresnelPowerFactor"), 5.0),
        rim_lift: float(mtoon.get("parametricRimLiftFactor"), 0.0),
        outline_width_mode: match mtoon.get("outlineWidthMode").string() {
            None | Some("none") => OUTLINE_NONE,
            Some("worldCoordinates") => OUTLINE_WORLD,
            Some("screenCoordinates") => OUTLINE_SCREEN,
            Some(other) => return Err(format!("unknown outlineWidthMode {}", other)),
        },
        outline_width: float(mtoon.get("outlineWidthFactor"), 0.0),
        outline_width_multiply_texture: texture_ref(mtoon.get("outlineWidthMultiplyTexture"), texture_count)?,
        outline_color: floats(mtoon.get("outlineColorFactor"), [0.0, 0.0, 0.0]),
        outline_lighting_mix: float(mtoon.get("outlineLightingMixFactor"), 1.0),
        uv_animation_mask_texture: texture_ref(mtoon.get("uvAnimationMaskTexture"), texture_count)?,
        uv_animation_scroll_x: float(mtoon.get("uvAnimationScrollXSpeedFactor"), 0.0),
        uv_animation_scroll_y: float(mtoon.get("uvAnimationScrollYSpeedFactor"), 0.0),
        uv_animation_rotation: float(mtoon.get("uvAnimationRotationSpeedFactor"), 0.0),
    })
}

fn parse_override(value: &Value) -> Result<Override, String> {
    match value.string() {
        None | Some("none") => Ok(Override::None),
        Some("block") => Ok(Override::Block),
        Some("blend") => Ok(Override::Blend),
        Some(other) => Err(format!("unknown expression override {}", other)),
    }
}

fn parse_expression(name: &str, preset: bool, expression: &Value, node_targets: &[usize], material_count: usize) -> Result<Expression, String> {
    let mut morph_binds = Vec::new();
    for bind in expression.get("morphTargetBinds").array() {
        let node = index(bind.get("node"), node_targets.len(), "morphTargetBind node")?;
        let target = index(bind.get("index"), node_targets[node], "morphTargetBind morph target")?;
        morph_binds.push(MorphBind { node: node as u32, index: target as u32, weight: float(bind.get("weight"), 0.0) });
    }
    let mut color_binds = Vec::new();
    for bind in expression.get("materialColorBinds").array() {
        let target = match bind.get("type").string() {
            Some("color") => ColorTarget::Color,
            Some("emissionColor") => ColorTarget::EmissionColor,
            Some("shadeColor") => ColorTarget::ShadeColor,
            Some("matcapColor") => ColorTarget::MatcapColor,
            Some("rimColor") => ColorTarget::RimColor,
            Some("outlineColor") => ColorTarget::OutlineColor,
            _ => return Err(format!("expression {} has an unknown materialColorBind type", name)),
        };
        let material = index(bind.get("material"), material_count, "materialColorBind material")? as u32;
        color_binds.push(ColorBind { material, target, value: floats(bind.get("targetValue"), [0.0, 0.0, 0.0, 0.0]) });
    }
    let mut texture_transform_binds = Vec::new();
    for bind in expression.get("textureTransformBinds").array() {
        let material = index(bind.get("material"), material_count, "textureTransformBind material")? as u32;
        texture_transform_binds.push(TextureTransformBind { material, scale: floats(bind.get("scale"), [1.0, 1.0]), offset: floats(bind.get("offset"), [0.0, 0.0]) });
    }
    Ok(Expression {
        name: name.to_string(),
        preset,
        is_binary: flag(expression.get("isBinary"), "expression isBinary")?,
        override_blink: parse_override(expression.get("overrideBlink"))?,
        override_look_at: parse_override(expression.get("overrideLookAt"))?,
        override_mouth: parse_override(expression.get("overrideMouth"))?,
        morph_binds,
        color_binds,
        texture_transform_binds,
    })
}

fn parse_look_at(look_at: &Value) -> Result<LookAt, String> {
    let kind = match look_at.get("type").string() {
        None | Some("bone") => LookAtKind::Bone,
        Some("expression") => LookAtKind::Expression,
        Some(other) => return Err(format!("unknown lookAt type {}", other)),
    };
    let default_scale = if kind == LookAtKind::Bone { 10.0 } else { 1.0 };
    let mut maps = [RangeMap { input_max_value: 90.0, output_scale: default_scale }; 4];
    let names = ["rangeMapHorizontalInner", "rangeMapHorizontalOuter", "rangeMapVerticalDown", "rangeMapVerticalUp"];
    for (map, name) in maps.iter_mut().zip(names) {
        let value = look_at.get(name);
        map.input_max_value = float(value.get("inputMaxValue"), 90.0);
        map.output_scale = float(value.get("outputScale"), default_scale);
    }
    Ok(LookAt {
        kind,
        horizontal_inner: maps[0],
        horizontal_outer: maps[1],
        vertical_down: maps[2],
        vertical_up: maps[3],
    })
}

fn parse_shape(shape: &Value, extended: bool) -> Result<Option<ColliderShape>, String> {
    let sphere = shape.get("sphere");
    let capsule = shape.get("capsule");
    let plane = shape.get("plane");
    if !sphere.is_null() {
        return Ok(Some(ColliderShape::Sphere {
            offset: floats(sphere.get("offset"), [0.0, 0.0, 0.0]),
            radius: float(sphere.get("radius"), 0.0),
            inside: extended && flag(sphere.get("inside"), "collider inside")?,
        }));
    }
    if !capsule.is_null() {
        return Ok(Some(ColliderShape::Capsule {
            offset: floats(capsule.get("offset"), [0.0, 0.0, 0.0]),
            radius: float(capsule.get("radius"), 0.0),
            tail: floats(capsule.get("tail"), [0.0, 0.0, 0.0]),
            inside: extended && flag(capsule.get("inside"), "collider inside")?,
        }));
    }
    if extended && !plane.is_null() {
        return Ok(Some(ColliderShape::Plane { offset: floats(plane.get("offset"), [0.0, 0.0, 0.0]), normal: floats(plane.get("normal"), [0.0, 0.0, 1.0]) }));
    }
    Ok(None)
}

fn parse_axis(value: &Value) -> Result<Axis, String> {
    match value.string() {
        Some("X" | "PositiveX") => Ok(Axis::PositiveX),
        Some("NegativeX") => Ok(Axis::NegativeX),
        Some("Y" | "PositiveY") => Ok(Axis::PositiveY),
        Some("NegativeY") => Ok(Axis::NegativeY),
        Some("Z" | "PositiveZ") => Ok(Axis::PositiveZ),
        Some("NegativeZ") => Ok(Axis::NegativeZ),
        _ => Err("node constraint has a missing or unknown axis".to_string()),
    }
}

fn primitive_bytes(gltf: &Gltf, primitive: &Value, joint_count: usize, total: usize) -> Result<usize, String> {
    let skinned = joint_count > 0;
    let mode = integer_or(primitive.get("mode"), 4, "primitive mode")?;
    if mode != 4 {
        return Err(format!("primitive mode {} is not supported, only triangles", mode));
    }
    let attributes = primitive.get("attributes");
    let vertex_count = gltf.accessor(attributes.get("POSITION"), 3, "POSITION")?.count;
    let total = grow(total, 1, 2 * size_of::<(u32, &Value)>() + size_of::<PrimitiveData>() + size_of::<Primitive>())?;
    let mut total = grow(total, vertex_count, 12)?;
    let checks = [("NORMAL", 3, 12), ("TEXCOORD_0", 2, 8), ("JOINTS_0", 4, 8), ("WEIGHTS_0", 4, 16)];
    for (name, components, unit) in checks {
        let value = attributes.get(name);
        if value.is_null() || (!skinned && (name == "JOINTS_0" || name == "WEIGHTS_0")) {
            continue;
        }
        let accessor = if name == "JOINTS_0" { gltf.integer_accessor(value, components, name)? } else { gltf.accessor(value, components, name)? };
        if name == "JOINTS_0" {
            gltf.check_integers(accessor, joint_count, name)?;
        }
        if accessor.count != vertex_count {
            return Err(format!("{} has {} elements but POSITION has {}", name, accessor.count, vertex_count));
        }
        total = grow(total, vertex_count, unit)?;
    }
    if skinned && !attributes.get("JOINTS_0").is_null() && attributes.get("WEIGHTS_0").is_null() {
        return Err("primitive has JOINTS_0 without WEIGHTS_0".to_string());
    }
    let index_count = if primitive.get("indices").is_null() {
        vertex_count
    } else {
        let accessor = gltf.integer_accessor(primitive.get("indices"), 1, "indices")?;
        gltf.check_integers(accessor, vertex_count, "index")?;
        accessor.count
    };
    if index_count % 3 != 0 {
        return Err(format!("primitive index count {} is not a multiple of 3", index_count));
    }
    total = grow(total, index_count, 4)?;
    let targets = primitive.get("targets").array();
    let mut has_normals = false;
    for target in targets {
        for name in ["POSITION", "NORMAL"] {
            if target.get(name).is_null() {
                continue;
            }
            has_normals |= name == "NORMAL";
            let accessor = gltf.accessor(target.get(name), 3, "morph target")?;
            if accessor.count != vertex_count {
                return Err(format!("morph target {} has {} elements but POSITION has {}", name, accessor.count, vertex_count));
            }
        }
    }
    let target_floats = targets.len().checked_mul(vertex_count).ok_or("morph target size overflows")?;
    total = grow(total, target_floats, if has_normals { 24 } else { 12 })?;
    Ok(total)
}

fn decode_primitive(gltf: &Gltf, primitive: &Value, node: u32, skin: i32, joint_count: usize, material_count: usize) -> Result<PrimitiveData, String> {
    let attributes = primitive.get("attributes");
    let position = gltf.accessor(attributes.get("POSITION"), 3, "POSITION")?;
    let vertex_count = position.count;
    let positions = gltf.floats(position)?;
    let normals = if attributes.get("NORMAL").is_null() { Vec::new() } else { gltf.floats(gltf.accessor(attributes.get("NORMAL"), 3, "NORMAL")?)? };
    let uvs = if attributes.get("TEXCOORD_0").is_null() { Vec::new() } else { gltf.floats(gltf.accessor(attributes.get("TEXCOORD_0"), 2, "TEXCOORD_0")?)? };
    let skinned = skin >= 0 && !attributes.get("JOINTS_0").is_null();
    let (joints, weights) = if skinned {
        if joint_count > u16::MAX as usize + 1 {
            return Err("skin has more than 65536 joints".to_string());
        }
        let joints = gltf.integers(gltf.integer_accessor(attributes.get("JOINTS_0"), 4, "JOINTS_0")?);
        let weights = gltf.floats(gltf.accessor(attributes.get("WEIGHTS_0"), 4, "WEIGHTS_0")?)?;
        (joints.into_iter().map(|joint| joint as u16).collect(), weights)
    } else {
        (Vec::new(), Vec::new())
    };
    let indices = if primitive.get("indices").is_null() {
        (0..vertex_count as u32).collect()
    } else {
        gltf.integers(gltf.integer_accessor(primitive.get("indices"), 1, "indices")?)
    };
    let targets = primitive.get("targets").array();
    let has_normals = targets.iter().any(|target| !target.get("NORMAL").is_null());
    let block = vertex_count * 3;
    let mut target_positions = vec![0.0f32; targets.len() * block];
    let mut target_normals = if has_normals { vec![0.0f32; targets.len() * block] } else { Vec::new() };
    for (number, target) in targets.iter().enumerate() {
        if !target.get("POSITION").is_null() {
            let values = gltf.floats(gltf.accessor(target.get("POSITION"), 3, "morph target")?)?;
            target_positions[number * block..(number + 1) * block].copy_from_slice(&values);
        }
        if !target.get("NORMAL").is_null() {
            let values = gltf.floats(gltf.accessor(target.get("NORMAL"), 3, "morph target")?)?;
            target_normals[number * block..(number + 1) * block].copy_from_slice(&values);
        }
    }
    Ok(PrimitiveData {
        node,
        skin: if skinned { skin } else { -1 },
        material: optional_index(primitive.get("material"), material_count, "primitive material")?,
        vertex_count: vertex_count as u32,
        positions,
        normals,
        uvs,
        joints,
        weights,
        indices,
        target_count: targets.len() as u32,
        target_positions,
        target_normals,
    })
}

pub fn glb_floats(bytes: &[u8], wanted: &[(usize, usize)], limit: usize) -> Result<Vec<Vec<f32>>, String> {
    let (json_chunk, bin) = split_glb(bytes)?;
    grow(0, json_chunk.len(), size_of::<Value>())?;
    let document = json::parse(json_chunk)?;
    let views = parse_views(&document, bin.len())?;
    let gltf = Gltf { bin, accessors: parse_accessors(&document, &views, bin)? };
    let mut total = 0usize;
    let mut output = Vec::with_capacity(wanted.len());
    for &(number, components) in wanted {
        let accessor = gltf.accessors.get(number).ok_or_else(|| format!("accessor {} is out of range", number))?;
        if accessor.components != components {
            return Err(format!("accessor {} has {} components, expected {}", number, accessor.components, components));
        }
        total = accessor.count.checked_mul(components).and_then(|count| total.checked_add(count)).filter(|total| *total <= limit).ok_or_else(|| format!("accessors exceed {} values", limit))?;
        output.push(gltf.floats(accessor)?);
    }
    Ok(output)
}

pub fn load(bytes: &[u8]) -> Result<Model, String> {
    let (json_chunk, bin) = split_glb(bytes)?;
    let mut total = grow(0, json_chunk.len(), size_of::<Value>())?;
    let document = json::parse(json_chunk)?;
    let extensions = document.get("extensions");
    let vrm = extensions.get("VRMC_vrm");
    if vrm.is_null() {
        if !extensions.get("VRM").is_null() {
            return Err("vrm 0.x is not supported, re-export it as vrm 1.0".to_string());
        }
        return Err("not a vrm 1.0 file".to_string());
    }
    if vrm.get("specVersion").string() != Some("1.0") {
        return Err(format!("VRMC_vrm specVersion {} is not supported, expected 1.0", vrm.get("specVersion").string().unwrap_or("(missing)")));
    }
    let views = parse_views(&document, bin.len())?;
    let gltf = Gltf { bin, accessors: parse_accessors(&document, &views, bin)? };

    let images_json = document.get("images").array();
    let mut image_ranges = Vec::new();
    for (number, image) in images_json.iter().enumerate() {
        if !image.get("uri").is_null() {
            return Err(format!("image {} uses a uri, only images embedded in the glb are supported", number));
        }
        let view = &views[index(image.get("bufferView"), views.len(), "image bufferView")?];
        let mime = match image.get("mimeType").string() {
            Some("image/png") => MIME_PNG,
            Some("image/jpeg") => MIME_JPEG,
            _ => return Err(format!("image {} is not png or jpeg", number)),
        };
        let pixels = image_pixels(&bin[view.start..view.start + view.length]).ok_or_else(|| format!("image {} has no readable png or jpeg dimensions", number))?;
        total = grow(grow(total, view.length, 1)?, pixels, 4)?;
        image_ranges.push((view.start, view.length, mime));
    }

    let samplers = document.get("samplers").array();
    let mut texture_sources = Vec::new();
    for (number, texture) in document.get("textures").array().iter().enumerate() {
        if texture.get("source").is_null() {
            return Err(format!("texture {} has no png or jpeg source", number));
        }
        let image = index(texture.get("source"), images_json.len(), "texture source")? as u32;
        let sampler = optional_index(texture.get("sampler"), samplers.len(), "texture sampler")?;
        let sampler = if sampler < 0 { &Value::Null } else { &samplers[sampler as usize] };
        texture_sources.push(TextureSource {
            image,
            mag_filter: float(sampler.get("magFilter"), 9729.0) as i32,
            min_filter: float(sampler.get("minFilter"), 9987.0) as i32,
            wrap_s: float(sampler.get("wrapS"), 10497.0) as i32,
            wrap_t: float(sampler.get("wrapT"), 10497.0) as i32,
        });
    }

    let materials_json = document.get("materials").array();
    total = grow(total, materials_json.len(), 3 * size_of::<Material>())?;
    let mut materials = Vec::new();
    for material in materials_json {
        materials.push(parse_material(material, texture_sources.len())?);
    }

    let meshes = document.get("meshes").array();
    let skins_json = document.get("skins").array();
    let nodes_json = document.get("nodes").array();
    let node_count = nodes_json.len();
    let mut mesh_targets = Vec::with_capacity(meshes.len());
    for (number, mesh) in meshes.iter().enumerate() {
        let primitives = mesh.get("primitives").array();
        let first = primitives.first().ok_or_else(|| format!("mesh {} has no primitives", number))?;
        let count = first.get("targets").array().len();
        if primitives.iter().any(|primitive| primitive.get("targets").array().len() != count) {
            return Err(format!("mesh {} primitives disagree on morph target count", number));
        }
        for primitive in primitives {
            for (name, value) in primitive.get("attributes").entries().iter().chain(primitive.get("targets").array().iter().flat_map(|target| target.entries())) {
                index(value, gltf.accessors.len(), name)?;
            }
            optional_index(primitive.get("indices"), gltf.accessors.len(), "indices")?;
            optional_index(primitive.get("material"), materials.len(), "primitive material")?;
        }
        mesh_targets.push(count);
    }

    let mut skin_joints = Vec::with_capacity(skins_json.len());
    for (number, skin) in skins_json.iter().enumerate() {
        let joints = skin.get("joints").array();
        if joints.is_empty() {
            return Err(format!("skin {} has no joints", number));
        }
        let mut list = Vec::with_capacity(joints.len());
        for joint in joints {
            list.push(index(joint, node_count, "skin joint")? as u32);
        }
        if !skin.get("inverseBindMatrices").is_null() {
            let accessor = gltf.accessor(skin.get("inverseBindMatrices"), 16, "inverseBindMatrices")?;
            if accessor.count < list.len() {
                return Err(format!("skin {} has fewer inverse bind matrices than joints", number));
            }
            total = grow(total, accessor.count, 64)?;
        }
        total = grow(total, list.len(), 128)?;
        skin_joints.push(list);
    }

    let expressions_json = vrm.get("expressions");
    total = grow(total, expressions_json.get("preset").entries().len() + expressions_json.get("custom").entries().len(), size_of::<Expression>() + size_of::<Param>())?;
    total = grow(total, node_count, 2 * size_of::<Node>())?;
    let mut nodes = Vec::with_capacity(node_count);
    let mut node_targets = vec![0usize; node_count];
    for (number, node) in nodes_json.iter().enumerate() {
        let mesh = optional_index(node.get("mesh"), meshes.len(), "node mesh")?;
        let skin = optional_index(node.get("skin"), skins_json.len(), "node skin")?;
        let mut children = Vec::new();
        for child in node.get("children").array() {
            children.push(index(child, node_count, "node child")? as u32);
        }
        let (translation, rotation, scale) = if node.get("matrix").array().len() == 16 {
            decompose(floats(node.get("matrix"), IDENTITY_MATRIX))
        } else {
            (floats(node.get("translation"), [0.0; 3]), floats(node.get("rotation"), [0.0, 0.0, 0.0, 1.0]), floats(node.get("scale"), [1.0; 3]))
        };
        let mut rest_weights = Vec::new();
        if mesh >= 0 {
            let count = mesh_targets[mesh as usize];
            node_targets[number] = count;
            let authored = if node.get("weights").is_null() { meshes[mesh as usize].get("weights") } else { node.get("weights") };
            total = grow(total, count, 12)?;
            rest_weights = vec![0.0f32; count];
            let values = authored.array();
            if !values.is_empty() {
                if values.len() != count {
                    return Err(format!("node {} morph weights count {} does not match {} targets", number, values.len(), count));
                }
                for (slot, value) in rest_weights.iter_mut().zip(values) {
                    *slot = float(value, 0.0);
                }
            }
        }
        nodes.push(Node {
            parent: -1,
            children,
            mesh,
            skin,
            rest_translation: translation,
            rest_rotation: rotation,
            rest_scale: scale,
            translation,
            rotation,
            scale,
            world: IDENTITY_MATRIX,
            weights: rest_weights.clone(),
            rest_weights,
        });
    }
    for number in 0..node_count {
        for child_index in 0..nodes[number].children.len() {
            let child = nodes[number].children[child_index] as usize;
            if nodes[child].parent >= 0 || child == number {
                return Err(format!("node {} has more than one parent or is its own child", child));
            }
            nodes[child].parent = number as i32;
        }
    }
    let mut state = vec![0u8; node_count];
    for start in 0..node_count {
        let mut current = start;
        while state[current] == 0 {
            state[current] = 1;
            if nodes[current].parent < 0 {
                break;
            }
            current = nodes[current].parent as usize;
        }
        if state[current] == 1 && nodes[current].parent >= 0 {
            return Err("node hierarchy has a cycle".to_string());
        }
        let mut current = start;
        while state[current] == 1 {
            state[current] = 2;
            if nodes[current].parent < 0 {
                break;
            }
            current = nodes[current].parent as usize;
        }
    }

    let scenes = document.get("scenes").array();
    let mut visible = vec![scenes.is_empty(); node_count];
    if !scenes.is_empty() {
        let scene = if document.get("scene").is_null() { 0 } else { index(document.get("scene"), scenes.len(), "scene")? };
        let scene = &scenes[scene];
        let mut stack = Vec::new();
        for root in scene.get("nodes").array() {
            let root = index(root, node_count, "scene node")?;
            if nodes[root].parent >= 0 {
                return Err(format!("scene root node {} has a parent", root));
            }
            if !visible[root] {
                visible[root] = true;
                stack.push(root);
            }
        }
        while let Some(node) = stack.pop() {
            for child in &nodes[node].children {
                let child = *child as usize;
                if !visible[child] {
                    visible[child] = true;
                    stack.push(child);
                }
            }
        }
    }

    let mut plans = Vec::new();
    for (number, node) in nodes.iter().enumerate() {
        if node.mesh < 0 || !visible[number] {
            continue;
        }
        for primitive in meshes[node.mesh as usize].get("primitives").array() {
            let joint_count = if node.skin < 0 { 0 } else { skin_joints[node.skin as usize].len() };
            total = primitive_bytes(&gltf, primitive, joint_count, total)?;
            plans.push((number as u32, primitive));
        }
    }

    let mut images = Vec::with_capacity(image_ranges.len());
    for (start, length, mime) in image_ranges {
        images.push(Image { bytes: bin[start..start + length].to_vec(), mime });
    }

    let mut skins = Vec::with_capacity(skin_joints.len());
    for (skin, joints) in skins_json.iter().zip(skin_joints) {
        let inverse_bind_matrices = if skin.get("inverseBindMatrices").is_null() {
            vec![IDENTITY_MATRIX; joints.len()]
        } else {
            let values = gltf.floats(gltf.accessor(skin.get("inverseBindMatrices"), 16, "inverseBindMatrices")?)?;
            values.chunks_exact(16).take(joints.len()).map(|chunk| chunk.try_into().unwrap_or(IDENTITY_MATRIX)).collect()
        };
        let mut matrices = Vec::with_capacity(joints.len() * 16);
        for _ in 0..joints.len() {
            matrices.extend_from_slice(&IDENTITY_MATRIX);
        }
        optional_index(skin.get("skeleton"), node_count, "skin skeleton")?;
        skins.push(Skin { joints, inverse_bind_matrices, matrices });
    }

    let mut primitive_data = Vec::with_capacity(plans.len());
    for (node, primitive) in plans {
        let skin = nodes[node as usize].skin;
        let joint_count = if skin >= 0 { skins[skin as usize].joints.len() } else { 0 };
        primitive_data.push(decode_primitive(&gltf, primitive, node, skin, joint_count, materials.len())?);
    }

    let mut humanoid = Vec::new();
    for (bone, value) in vrm.get("humanoid").get("humanBones").entries() {
        humanoid.push((bone.clone(), index(value.get("node"), node_count, "humanoid bone node")? as u32));
    }

    let mut expressions = Vec::new();
    for (group, preset) in [("preset", true), ("custom", false)] {
        for (name, expression) in expressions_json.get(group).entries() {
            expressions.push(parse_expression(name, preset, expression, &node_targets, materials.len())?);
        }
    }

    let look_at = parse_look_at(vrm.get("lookAt"))?;

    let spring_bone = extensions.get("VRMC_springBone");
    let mut colliders = Vec::new();
    for (number, collider) in spring_bone.get("colliders").array().iter().enumerate() {
        let node = index(collider.get("node"), node_count, "collider node")? as u32;
        let extended = collider.get("extensions").get("VRMC_springBone_extended_collider").get("shape");
        let shape = if extended.is_null() { parse_shape(collider.get("shape"), false)? } else { parse_shape(extended, true)? };
        colliders.push(Collider { node, shape: shape.ok_or_else(|| format!("collider {} has no shape", number))? });
    }
    let mut collider_groups = Vec::new();
    for group in spring_bone.get("colliderGroups").array() {
        let mut members = Vec::new();
        for collider in group.get("colliders").array() {
            members.push(index(collider, colliders.len(), "colliderGroup collider")? as u32);
        }
        collider_groups.push(members);
    }
    let mut springs = Vec::new();
    for spring in spring_bone.get("springs").array() {
        let mut joints = Vec::new();
        for joint in spring.get("joints").array() {
            joints.push(SpringJoint {
                node: index(joint.get("node"), node_count, "spring joint node")? as u32,
                hit_radius: float(joint.get("hitRadius"), 0.0),
                stiffness: float(joint.get("stiffness"), 1.0),
                gravity_power: float(joint.get("gravityPower"), 0.0),
                gravity_direction: floats(joint.get("gravityDir"), [0.0, -1.0, 0.0]),
                drag_force: float(joint.get("dragForce"), 0.5),
            });
        }
        let mut groups = Vec::new();
        for group in spring.get("colliderGroups").array() {
            groups.push(index(group, collider_groups.len(), "spring colliderGroup")? as u32);
        }
        springs.push(Spring {
            joints,
            collider_groups: groups,
            center: optional_index(spring.get("center"), node_count, "spring center")?,
        });
    }

    let mut constraints = Vec::new();
    for (number, node) in nodes_json.iter().enumerate() {
        let constraint = node.get("extensions").get("VRMC_node_constraint").get("constraint");
        if constraint.is_null() {
            continue;
        }
        let roll = constraint.get("roll");
        let aim = constraint.get("aim");
        let rotation = constraint.get("rotation");
        let (body, kind) = if !roll.is_null() {
            (roll, ConstraintKind::Roll { source: index(roll.get("source"), node_count, "roll constraint source")? as u32, axis: parse_axis(roll.get("rollAxis"))? })
        } else if !aim.is_null() {
            (aim, ConstraintKind::Aim { source: index(aim.get("source"), node_count, "aim constraint source")? as u32, aim_axis: parse_axis(aim.get("aimAxis"))? })
        } else if !rotation.is_null() {
            (rotation, ConstraintKind::Rotation { source: index(rotation.get("source"), node_count, "rotation constraint source")? as u32 })
        } else {
            return Err(format!("node {} constraint has no roll, aim or rotation", number));
        };
        constraints.push(Constraint { node: number as u32, kind, weight: float(body.get("weight"), 1.0) });
    }

    Ok(Model {
        nodes,
        images,
        texture_sources,
        textures: Vec::new(),
        material_rest: materials.clone(),
        materials,
        primitive_data,
        primitives: Vec::new(),
        skins,
        humanoid,
        expressions,
        look_at,
        colliders,
        collider_groups,
        springs,
        constraints,
        params: Vec::new(),
        param_values: Vec::new(),
        bone_poses: vec![0.0; BONE_NAMES.len() * 5],
        physics_strength: 1.0,
        wind: 0.0,
        bounds: [[0.0; 3]; 2],
        state: State::default(),
    })
}
