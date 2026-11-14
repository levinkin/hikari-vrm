#include <algorithm>
#include <cmath>
#include <cstring>
#include <vector>
#include "hv.h"

static const int kSlotCount = 11;
static const int kTextureSrgb = 1;
static const int kTextureLinear = 2;
static const int kUvAnimationMaskSlot = 8;
static const int kSrgbSlots = 1 << 0 | 1 << 1 | 1 << 4 | 1 << 5 | 1 << 6;

enum
{
    draw_skinned = 1,
    draw_normals = 2,
    draw_uvs = 4,
    draw_target_normals = 8,
};

struct texture_slot
{
    float row_u[4];
    float row_v[4];
};

struct material_uniforms
{
    float base_color[4];
    float emissive[4];
    float shade_color[4];
    float matcap_color[4];
    float rim_color[4];
    float outline_color[4];
    float rim_shape[4];
    float surface[4];
    float alpha_outline[4];
    float uv_animation[4];
    texture_slot slots[kSlotCount];
};

struct frame_uniforms
{
    float view_projection[16];
    float camera_position[4];
    float light_direction[4];
    float light_color[4];
    float ambient[4];
    float viewport[4];
};

struct draw_uniforms
{
    float world[16];
    uint32_t vertex_count;
    uint32_t active_target_count;
    uint32_t flags;
    uint32_t padding;
};

struct active_target
{
    uint32_t index;
    float weight;
};

struct material_state
{
    material_uniforms uniforms = {};
    float uv_speed[3] = {0, 0, 0};
    int kind = HV_MATERIAL_PBR;
    int alpha_mode = HV_ALPHA_OPAQUE;
    bool double_sided = false;
    bool depth_write = true;
    int queue_offset = 0;
    bool outline = false;
};

struct primitive_state
{
    const hv_primitive* source = NULL;
    int material = 0;
    uint32_t flags = 0;
    uint32_t active_count = 0;
    active_target* mapped_targets = NULL;
    float centroid[3] = {0, 0, 0};
    int dominant_joint = 0;
};

struct blend_entry
{
    int primitive;
    int group;
    int queue_offset;
    float distance;
};

static const hv_texture_ref* material_ref(const hv_material* material, int slot)
{
    switch (slot)
    {
    case 0:
        return &material->base_color_texture;
    case 1:
        return &material->shade_multiply_texture;
    case 2:
        return &material->shading_shift_texture;
    case 3:
        return &material->normal_texture;
    case 4:
        return &material->emissive_texture;
    case 5:
        return &material->matcap_texture;
    case 6:
        return &material->rim_multiply_texture;
    case 7:
        return &material->outline_width_multiply_texture;
    case 8:
        return &material->uv_animation_mask_texture;
    case 9:
        return &material->metallic_roughness_texture;
    default:
        return &material->occlusion_texture;
    }
}

std::vector<uint8_t> renderer_texture_usage(hv_model* model)
{
    std::vector<uint8_t> usage((size_t)hv_texture_count(model), 0);
    for (int32_t index = 0; index < hv_material_count(model); index++)
    {
        const hv_material* material = hv_material_at(model, index);
        for (int slot = 0; slot < kSlotCount; slot++)
        {
            int32_t texture = material_ref(material, slot)->texture;
            if (texture >= 0 && texture < (int32_t)usage.size())
            {
                usage[texture] |= (kSrgbSlots >> slot & 1) != 0 ? kTextureSrgb : kTextureLinear;
            }
        }
    }
    return usage;
}

static void set4(float* out, float x, float y, float z, float w)
{
    out[0] = x;
    out[1] = y;
    out[2] = z;
    out[3] = w;
}

static void fill_material(material_state& out, const hv_material* material, const bool* bound)
{
    material_uniforms& u = out.uniforms;
    set4(u.base_color, material->base_color[0], material->base_color[1], material->base_color[2], material->base_color[3]);
    set4(u.emissive, material->emissive[0], material->emissive[1], material->emissive[2], 0.0f);
    set4(u.shade_color, material->shade_color[0], material->shade_color[1], material->shade_color[2], material->shading_shift);
    set4(u.matcap_color, material->matcap_color[0], material->matcap_color[1], material->matcap_color[2], material->shading_toony);
    set4(u.rim_color, material->rim_color[0], material->rim_color[1], material->rim_color[2], material->rim_lighting_mix);
    set4(u.outline_color, material->outline_color[0], material->outline_color[1], material->outline_color[2], material->outline_lighting_mix);
    set4(u.rim_shape, material->rim_fresnel_power, material->rim_lift, 0.0f, material->shading_shift_texture_scale);
    set4(u.surface, material->normal_scale, material->metallic, material->roughness, material->occlusion_strength);
    set4(u.alpha_outline, material->alpha_cutoff, material->outline_width, (float)material->alpha_mode, (float)material->outline_width_mode);
    for (int slot = 0; slot < kSlotCount; slot++)
    {
        const hv_texture_ref& ref = *material_ref(material, slot);
        float cosine = cosf(ref.rotation);
        float sine = sinf(ref.rotation);
        set4(u.slots[slot].row_u, cosine * ref.scale[0], -sine * ref.scale[1], ref.offset[0], bound[slot] ? 1.0f : 0.0f);
        set4(u.slots[slot].row_v, sine * ref.scale[0], cosine * ref.scale[1], ref.offset[1], 0.0f);
    }
    out.uv_speed[0] = material->uv_animation_scroll_x;
    out.uv_speed[1] = material->uv_animation_scroll_y;
    out.uv_speed[2] = material->uv_animation_rotation;
    out.kind = std::clamp((int)material->kind, 0, 2);
    out.alpha_mode = material->alpha_mode;
    out.double_sided = material->double_sided;
    out.depth_write = material->alpha_mode != HV_ALPHA_BLEND || material->transparent_with_z_write;
    out.queue_offset = material->alpha_mode == HV_ALPHA_BLEND ? material->render_queue_offset : 0;
    out.outline = out.kind == HV_MATERIAL_MTOON && material->outline_width_mode != HV_OUTLINE_NONE && material->outline_width > 0.0f;
}

static hv_material fallback_material()
{
    hv_material fallback = {};
    fallback.base_color[0] = fallback.base_color[1] = fallback.base_color[2] = fallback.base_color[3] = 1.0f;
    fallback.roughness = 1.0f;
    fallback.normal_scale = 1.0f;
    fallback.occlusion_strength = 1.0f;
    for (int slot = 0; slot < kSlotCount; slot++)
    {
        hv_texture_ref* ref = (hv_texture_ref*)material_ref(&fallback, slot);
        ref->texture = -1;
        ref->scale[0] = ref->scale[1] = 1.0f;
    }
    return fallback;
}

static void place_primitive(hv_model* model, primitive_state& out)
{
    const hv_primitive* source = out.source;
    size_t vertices = source->vertex_count;
    double sum[3] = {0, 0, 0};
    for (size_t vertex = 0; vertex < vertices; vertex++)
    {
        for (int axis = 0; axis < 3; axis++)
        {
            sum[axis] += source->positions[vertex * 3 + axis];
        }
    }
    for (int axis = 0; axis < 3; axis++)
    {
        out.centroid[axis] = vertices > 0 ? (float)(sum[axis] / vertices) : 0.0f;
    }
    if ((out.flags & draw_skinned) != 0)
    {
        std::vector<float> totals((size_t)hv_skin_joint_count(model, source->skin), 0.0f);
        for (size_t vertex = 0; vertex < vertices; vertex++)
        {
            for (int influence = 0; influence < 4; influence++)
            {
                uint16_t joint = source->joints[vertex * 4 + influence];
                if (joint < totals.size())
                {
                    totals[joint] += source->weights[vertex * 4 + influence];
                }
            }
        }
        out.dominant_joint = totals.empty() ? 0 : (int)(std::max_element(totals.begin(), totals.end()) - totals.begin());
    }
}

template <class Material, class Primitive>
static void draw_orders(const std::vector<Material>& materials, const std::vector<Primitive>& primitives, std::vector<int>& solid, std::vector<blend_entry>& blend)
{
    for (int alpha_mode = HV_ALPHA_OPAQUE; alpha_mode <= HV_ALPHA_BLEND; alpha_mode++)
    {
        for (int index = 0; index < (int)primitives.size(); index++)
        {
            const Material& material = materials[primitives[index].material];
            if (primitives[index].source == NULL || material.alpha_mode != alpha_mode)
            {
                continue;
            }
            if (alpha_mode == HV_ALPHA_BLEND)
            {
                blend.push_back({index, material.depth_write ? 0 : 1, material.queue_offset, 0.0f});
            }
            else
            {
                solid.push_back(index);
            }
        }
    }
}

static uint32_t compact_targets(const hv_primitive* source, active_target* targets)
{
    uint32_t count = 0;
    const float* weights = source->morph_weights;
    for (uint32_t target = 0; weights != NULL && target < source->target_count; target++)
    {
        if (weights[target] != 0.0f)
        {
            targets[count++] = {target, weights[target]};
        }
    }
    return count;
}

template <class Renderer, class Material, class Texture, class Sampler>
static bool bind_material(const Renderer* r, Material& out, const hv_material* material, Texture blank, Sampler fallback)
{
    bool bound[kSlotCount];
    bool changed = false;
    for (int slot = 0; slot < kSlotCount; slot++)
    {
        const hv_texture_ref& ref = *material_ref(material, slot);
        bool valid = ref.texture >= 0 && ref.texture < (int32_t)r->samplers.size();
        Texture texture = valid ? ((kSrgbSlots >> slot & 1) != 0 ? r->srgb_textures[ref.texture] : r->linear_textures[ref.texture]) : Texture();
        bound[slot] = texture != Texture();
        Texture view = bound[slot] ? texture : blank;
        Sampler sampler = bound[slot] ? r->samplers[ref.texture] : fallback;
        changed = changed || out.textures[slot] != view || out.samplers[slot] != sampler;
        out.textures[slot] = view;
        out.samplers[slot] = sampler;
    }
    fill_material(out, material, bound);
    return changed;
}

template <class Renderer>
static bool upload_changes(Renderer* r, uint32_t changed)
{
    if ((changed & HV_CHANGED_JOINTS) != 0)
    {
        for (size_t skin = 0; skin < r->skin_memory.size(); skin++)
        {
            const float* matrices = hv_skin_joint_matrices(r->model, (int32_t)skin);
            if (matrices != NULL)
            {
                memcpy(r->skin_memory[skin], matrices, (size_t)hv_skin_joint_count(r->model, (int32_t)skin) * 64);
            }
        }
    }
    if ((changed & HV_CHANGED_MORPHS) != 0)
    {
        for (primitive_state& gpu : r->primitives)
        {
            if (gpu.mapped_targets != NULL && gpu.source != NULL)
            {
                gpu.active_count = compact_targets(gpu.source, gpu.mapped_targets);
            }
        }
    }
    return (changed & HV_CHANGED_MATERIALS) != 0;
}

static bool blend_before(const blend_entry& left, const blend_entry& right)
{
    if (left.group != right.group)
    {
        return left.group < right.group;
    }
    if (left.queue_offset != right.queue_offset)
    {
        return left.queue_offset < right.queue_offset;
    }
    return left.distance > right.distance;
}

template <class Primitive>
static void sort_blend(hv_model* model, const std::vector<Primitive>& primitives, std::vector<blend_entry>& blend, const float* eye)
{
    for (blend_entry& entry : blend)
    {
        const Primitive& gpu = primitives[entry.primitive];
        const float* matrix = (gpu.flags & draw_skinned) != 0 ? hv_skin_joint_matrices(model, gpu.source->skin) + gpu.dominant_joint * 16 : gpu.source->world;
        float delta[3];
        for (int row = 0; row < 3; row++)
        {
            delta[row] = matrix[row] * gpu.centroid[0] + matrix[4 + row] * gpu.centroid[1] + matrix[8 + row] * gpu.centroid[2] + matrix[12 + row] - eye[row];
        }
        entry.distance = delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2];
    }
    std::sort(blend.begin(), blend.end(), blend_before);
}

static draw_uniforms draw_for(const primitive_state& gpu)
{
    draw_uniforms draw = {};
    memcpy(draw.world, gpu.source->world, 64);
    draw.vertex_count = gpu.source->vertex_count;
    draw.active_target_count = gpu.active_count;
    draw.flags = gpu.flags;
    return draw;
}

template <class Primitive>
static bool draw_mirrored(const Primitive& gpu)
{
    const float* m = gpu.source->world;
    float determinant = m[0] * (m[5] * m[10] - m[6] * m[9]) + m[1] * (m[6] * m[8] - m[4] * m[10]) + m[2] * (m[4] * m[9] - m[5] * m[8]);
    return determinant < 0.0f;
}

static void animated_uniforms(material_uniforms& out, const material_state& material, float seconds)
{
    out = material.uniforms;
    double period = material.uniforms.slots[kUvAnimationMaskSlot].row_u[3] > 0.5f ? 1024.0 : 1.0;
    set4(out.uv_animation, (float)fmod((double)material.uv_speed[0] * seconds, period), (float)fmod((double)material.uv_speed[1] * seconds, period), material.uv_speed[2] * seconds, 0.0f);
}

static frame_uniforms frame_for(const float* view_projection, const float* eye, const float* light_direction, const float* light_color, const float* ambient, int width, int height)
{
    frame_uniforms frame = {};
    memcpy(frame.view_projection, view_projection, 64);
    set4(frame.camera_position, eye[0], eye[1], eye[2], 1.0f);
    set4(frame.light_direction, light_direction[0], light_direction[1], light_direction[2], 0.0f);
    set4(frame.light_color, light_color[0], light_color[1], light_color[2], 1.0f);
    set4(frame.ambient, ambient[0], ambient[1], ambient[2], 1.0f);
    set4(frame.viewport, (float)width, (float)height, 0.0f, 0.0f);
    return frame;
}
