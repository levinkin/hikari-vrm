#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct hv_model hv_model;

enum {
    HV_CHANGED_JOINTS = 1,
    HV_CHANGED_MORPHS = 2,
    HV_CHANGED_MATERIALS = 4,
    HV_CHANGED_NODES = 8,
};

enum { HV_MIME_PNG = 0, HV_MIME_JPEG = 1 };
enum { HV_MATERIAL_PBR = 0, HV_MATERIAL_UNLIT = 1, HV_MATERIAL_MTOON = 2 };
enum { HV_ALPHA_OPAQUE = 0, HV_ALPHA_MASK = 1, HV_ALPHA_BLEND = 2 };
enum { HV_OUTLINE_NONE = 0, HV_OUTLINE_WORLD = 1, HV_OUTLINE_SCREEN = 2 };

typedef struct hv_texture_ref {
    int32_t texture;
    int32_t tex_coord;
    float offset[2];
    float scale[2];
    float rotation;
} hv_texture_ref;

typedef struct hv_texture {
    const uint8_t* bytes;
    size_t length;
    int32_t mime;
    int32_t mag_filter;
    int32_t min_filter;
    int32_t wrap_s;
    int32_t wrap_t;
} hv_texture;

typedef struct hv_material {
    int32_t kind;
    int32_t alpha_mode;
    float alpha_cutoff;
    bool double_sided;
    bool transparent_with_z_write;
    int32_t render_queue_offset;
    float base_color[4];
    hv_texture_ref base_color_texture;
    float emissive[3];
    hv_texture_ref emissive_texture;
    hv_texture_ref normal_texture;
    float normal_scale;
    float metallic;
    float roughness;
    hv_texture_ref metallic_roughness_texture;
    hv_texture_ref occlusion_texture;
    float occlusion_strength;
    float shade_color[3];
    hv_texture_ref shade_multiply_texture;
    float shading_shift;
    hv_texture_ref shading_shift_texture;
    float shading_shift_texture_scale;
    float shading_toony;
    float gi_equalization;
    float matcap_color[3];
    hv_texture_ref matcap_texture;
    float rim_color[3];
    hv_texture_ref rim_multiply_texture;
    float rim_lighting_mix;
    float rim_fresnel_power;
    float rim_lift;
    int32_t outline_width_mode;
    float outline_width;
    hv_texture_ref outline_width_multiply_texture;
    float outline_color[3];
    float outline_lighting_mix;
    hv_texture_ref uv_animation_mask_texture;
    float uv_animation_scroll_x;
    float uv_animation_scroll_y;
    float uv_animation_rotation;
} hv_material;

typedef struct hv_primitive {
    int32_t node;
    int32_t skin;
    int32_t material;
    uint32_t vertex_count;
    uint32_t index_count;
    uint32_t target_count;
    const float* positions;
    const float* normals;
    const float* uvs;
    const uint16_t* joints;
    const float* weights;
    const uint32_t* indices;
    const float* target_positions;
    const float* target_normals;
    const float* morph_weights;
    const float* world;
} hv_primitive;

hv_model* hv_load(const uint8_t* bytes, size_t length, char* error, size_t error_capacity);
void hv_free(hv_model* model);

int32_t hv_param_count(const hv_model* model);
const char* hv_param_id(const hv_model* model, int32_t index);
void hv_param_range(const hv_model* model, int32_t index, float* minimum, float* maximum, float* default_value);
float* hv_param_values(hv_model* model);
float* hv_bone_poses(hv_model* model);
void hv_physics_set(hv_model* model, float strength, float wind);
uint32_t hv_update(hv_model* model, float seconds);

int32_t hv_texture_count(const hv_model* model);
const hv_texture* hv_texture_at(const hv_model* model, int32_t index);
void hv_textures_release(hv_model* model);
void hv_geometry_release(hv_model* model);
int32_t hv_material_count(const hv_model* model);
const hv_material* hv_material_at(const hv_model* model, int32_t index);
int32_t hv_primitive_count(const hv_model* model);
const hv_primitive* hv_primitive_at(const hv_model* model, int32_t index);
int32_t hv_skin_count(const hv_model* model);
int32_t hv_skin_joint_count(const hv_model* model, int32_t skin);
const float* hv_skin_joint_matrices(const hv_model* model, int32_t skin);
void hv_bounds(const hv_model* model, float* minimum, float* maximum);
bool hv_bone_position(const hv_model* model, const char* humanoid_bone, float* position);

#ifdef __cplusplus
}
#endif
