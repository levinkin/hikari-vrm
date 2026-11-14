#include <metal_stdlib>
using namespace metal;

constant int material_kind [[function_constant(0)]];
constant bool outline_pass [[function_constant(1)]];

enum
{
    slot_base_color = 0,
    slot_shade_multiply = 1,
    slot_shading_shift = 2,
    slot_normal = 3,
    slot_emissive = 4,
    slot_matcap = 5,
    slot_rim_multiply = 6,
    slot_outline_width = 7,
    slot_uv_animation_mask = 8,
    slot_metallic_roughness = 9,
    slot_occlusion = 10,
    slot_count = 11,
};

enum
{
    draw_skinned = 1,
    draw_normals = 2,
    draw_uvs = 4,
    draw_target_normals = 8,
};

struct texture_slot
{
    float4 row_u;
    float4 row_v;
};

struct material_uniforms
{
    float4 base_color;
    float4 emissive;
    float4 shade_color;
    float4 matcap_color;
    float4 rim_color;
    float4 outline_color;
    float4 rim_shape;
    float4 surface;
    float4 alpha_outline;
    float4 uv_animation;
    texture_slot slots[slot_count];
};

struct frame_uniforms
{
    float4x4 view_projection;
    float4 camera_position;
    float4 light_direction;
    float4 light_color;
    float4 ambient;
    float4 viewport;
};

struct draw_uniforms
{
    float4x4 world;
    uint vertex_count;
    uint active_target_count;
    uint flags;
    uint padding;
};

struct active_target
{
    uint index;
    float weight;
};

struct surface_varyings
{
    float4 position [[position]];
    float3 world_position;
    float3 world_normal;
    float2 uv;
};

static float2 slot_uv(constant material_uniforms& material, int slot, float2 uv)
{
    float3 point = float3(uv, 1.0);
    return float2(dot(material.slots[slot].row_u.xyz, point), dot(material.slots[slot].row_v.xyz, point));
}

static float2 animated_uv(constant material_uniforms& material, float2 uv, float mask)
{
    float angle = material.uv_animation.z * mask;
    float cosine = cos(angle);
    float sine = sin(angle);
    float2 centered = uv - 0.5;
    float2 rotated = float2(cosine * centered.x - sine * centered.y, sine * centered.x + cosine * centered.y) + 0.5;
    return rotated + material.uv_animation.xy * mask;
}

vertex surface_varyings surface_vertex(uint vertex_index [[vertex_id]],
                                       constant draw_uniforms& draw [[buffer(0)]],
                                       constant frame_uniforms& frame [[buffer(1)]],
                                       constant material_uniforms& material [[buffer(2)]],
                                       device const packed_float3* positions [[buffer(3)]],
                                       device const packed_float3* normals [[buffer(4)]],
                                       device const float2* uvs [[buffer(5)]],
                                       device const ushort4* joints [[buffer(6)]],
                                       device const float4* weights [[buffer(7)]],
                                       device const float4x4* joint_matrices [[buffer(8)]],
                                       device const packed_float3* target_positions [[buffer(9)]],
                                       device const packed_float3* target_normals [[buffer(10)]],
                                       device const active_target* targets [[buffer(11)]],
                                       texture2d<float> outline_width_texture [[texture(0)]],
                                       texture2d<float> mask_texture [[texture(1)]],
                                       sampler outline_width_sampler [[sampler(0)]],
                                       sampler mask_sampler [[sampler(1)]])
{
    float3 position = float3(positions[vertex_index]);
    float3 normal = (draw.flags & draw_normals) != 0 ? float3(normals[vertex_index]) : float3(0.0, 0.0, 1.0);
    for (uint target = 0; target < draw.active_target_count; target++)
    {
        uint offset = targets[target].index * draw.vertex_count + vertex_index;
        position += float3(target_positions[offset]) * targets[target].weight;
        if ((draw.flags & draw_target_normals) != 0)
        {
            normal += float3(target_normals[offset]) * targets[target].weight;
        }
    }
    float4x4 world = draw.world;
    if ((draw.flags & draw_skinned) != 0)
    {
        ushort4 joint = joints[vertex_index];
        float4 weight = weights[vertex_index];
        world = joint_matrices[joint.x] * weight.x + joint_matrices[joint.y] * weight.y + joint_matrices[joint.z] * weight.z + joint_matrices[joint.w] * weight.w;
    }
    float3x3 linear_part = float3x3(world[0].xyz, world[1].xyz, world[2].xyz);
    float handedness = determinant(linear_part) < 0.0 ? -1.0 : 1.0;
    float3x3 normal_matrix = float3x3(cross(linear_part[1], linear_part[2]), cross(linear_part[2], linear_part[0]), cross(linear_part[0], linear_part[1]));
    float3 world_normal = normalize(normal_matrix * normal * handedness);
    float4 world_position = world * float4(position, 1.0);
    surface_varyings out;
    out.uv = (draw.flags & draw_uvs) != 0 ? uvs[vertex_index] : float2(0.0);
    out.world_normal = world_normal;
    if (outline_pass)
    {
        float width = material.alpha_outline.y;
        if (material.slots[slot_outline_width].row_u.w > 0.5)
        {
            float mask = material.slots[slot_uv_animation_mask].row_u.w > 0.5 ? mask_texture.sample(mask_sampler, slot_uv(material, slot_uv_animation_mask, out.uv), level(0.0)).b : 1.0;
            float2 uv = animated_uv(material, out.uv, mask);
            width *= outline_width_texture.sample(outline_width_sampler, slot_uv(material, slot_outline_width, uv), level(0.0)).g;
        }
        if (material.alpha_outline.w < 1.5)
        {
            world_position.xyz += world_normal * width;
            out.position = frame.view_projection * world_position;
        }
        else
        {
            out.position = frame.view_projection * world_position;
            float2 projected = (frame.view_projection * float4(world_normal, 0.0)).xy * frame.viewport.xy;
            float length_squared = dot(projected, projected);
            float2 direction = length_squared > 0.0 ? projected * rsqrt(length_squared) : float2(0.0);
            out.position.xy += direction * float2(2.0 * width * frame.viewport.y / frame.viewport.x, 2.0 * width) * out.position.w;
        }
    }
    else
    {
        out.position = frame.view_projection * world_position;
    }
    out.world_position = world_position.xyz;
    return out;
}

fragment float4 surface_fragment(surface_varyings in [[stage_in]],
                                 bool front_facing [[front_facing]],
                                 constant frame_uniforms& frame [[buffer(1)]],
                                 constant material_uniforms& material [[buffer(2)]],
                                 array<texture2d<float>, 11> textures [[texture(0)]],
                                 array<sampler, 11> samplers [[sampler(0)]])
{
    float mask = material.slots[slot_uv_animation_mask].row_u.w > 0.5 ? textures[slot_uv_animation_mask].sample(samplers[slot_uv_animation_mask], slot_uv(material, slot_uv_animation_mask, in.uv)).b : 1.0;
    float2 uv = material_kind == 2 ? animated_uv(material, in.uv, mask) : in.uv;
    float4 base = material.base_color;
    if (material.slots[slot_base_color].row_u.w > 0.5)
    {
        base *= textures[slot_base_color].sample(samplers[slot_base_color], slot_uv(material, slot_base_color, uv));
    }
    int alpha_mode = int(material.alpha_outline.z);
    if (alpha_mode == 1 && base.a < material.alpha_outline.x)
    {
        discard_fragment();
    }
    float alpha = alpha_mode == 2 ? base.a : 1.0;
    float3 normal = normalize(in.world_normal);
    if (!outline_pass && !front_facing)
    {
        normal = -normal;
    }
    if (material.slots[slot_normal].row_u.w > 0.5)
    {
        float2 normal_uv = slot_uv(material, slot_normal, uv);
        float3 position_dx = dfdx(in.world_position);
        float3 position_dy = -dfdy(in.world_position);
        float2 uv_dx = dfdx(normal_uv);
        float2 uv_dy = -dfdy(normal_uv);
        float3 across_dy = cross(position_dy, normal);
        float3 across_dx = cross(normal, position_dx);
        float3 tangent = across_dy * uv_dx.x + across_dx * uv_dy.x;
        float3 bitangent = across_dy * uv_dx.y + across_dx * uv_dy.y;
        float scale_squared = max(dot(tangent, tangent), dot(bitangent, bitangent));
        if (scale_squared > 0.0)
        {
            float inverse_scale = rsqrt(scale_squared);
            float3 sampled = textures[slot_normal].sample(samplers[slot_normal], normal_uv).xyz * 2.0 - 1.0;
            sampled.xy *= material.surface.x;
            normal = normalize(tangent * inverse_scale * sampled.x - bitangent * inverse_scale * sampled.y + normal * sampled.z);
        }
    }
    float3 view = normalize(frame.camera_position.xyz - in.world_position);
    float3 light = frame.light_direction.xyz;
    float3 color;
    if (material_kind == 1)
    {
        color = base.rgb;
    }
    else if (material_kind == 2)
    {
        float shading = dot(normal, light) + material.shade_color.w;
        if (material.slots[slot_shading_shift].row_u.w > 0.5)
        {
            shading += textures[slot_shading_shift].sample(samplers[slot_shading_shift], slot_uv(material, slot_shading_shift, uv)).r * material.rim_shape.w;
        }
        shading = saturate((shading + 1.0 - material.matcap_color.w) / max(2.0 - 2.0 * material.matcap_color.w, 1e-6));
        float3 shade = material.shade_color.rgb;
        if (material.slots[slot_shade_multiply].row_u.w > 0.5)
        {
            shade *= textures[slot_shade_multiply].sample(samplers[slot_shade_multiply], slot_uv(material, slot_shade_multiply, uv)).rgb;
        }
        color = mix(shade, base.rgb, shading) * frame.light_color.rgb;
        float3 illumination = frame.ambient.rgb;
        color += illumination * base.rgb;
        float3 emissive = material.emissive.rgb;
        if (material.slots[slot_emissive].row_u.w > 0.5)
        {
            emissive *= textures[slot_emissive].sample(samplers[slot_emissive], slot_uv(material, slot_emissive, uv)).rgb;
        }
        color += emissive;
        float3 view_x = normalize(float3(view.z, 0.0, -view.x));
        float3 view_y = cross(view, view_x);
        float2 matcap_uv = float2(dot(view_x, normal), -dot(view_y, normal)) * 0.495 + 0.5;
        float3 rim = material.slots[slot_matcap].row_u.w > 0.5 ? material.matcap_color.rgb * textures[slot_matcap].sample(samplers[slot_matcap], matcap_uv).rgb : float3(0.0);
        float parametric = material.rim_shape.x > 0.0 ? pow(saturate(1.0 - dot(normal, view) + material.rim_shape.y), material.rim_shape.x) : 1.0;
        rim += parametric * material.rim_color.rgb;
        if (material.slots[slot_rim_multiply].row_u.w > 0.5)
        {
            rim *= textures[slot_rim_multiply].sample(samplers[slot_rim_multiply], slot_uv(material, slot_rim_multiply, uv)).rgb;
        }
        rim *= mix(float3(1.0), frame.light_color.rgb + illumination, material.rim_color.w);
        color += rim;
        if (outline_pass)
        {
            color = material.outline_color.rgb * mix(float3(1.0), color, material.outline_color.w);
        }
    }
    else
    {
        float metallic = material.surface.y;
        float roughness = material.surface.z;
        if (material.slots[slot_metallic_roughness].row_u.w > 0.5)
        {
            float4 sampled = textures[slot_metallic_roughness].sample(samplers[slot_metallic_roughness], slot_uv(material, slot_metallic_roughness, uv));
            roughness *= sampled.g;
            metallic *= sampled.b;
        }
        float occlusion = 1.0;
        if (material.slots[slot_occlusion].row_u.w > 0.5)
        {
            occlusion = 1.0 + material.surface.w * (textures[slot_occlusion].sample(samplers[slot_occlusion], slot_uv(material, slot_occlusion, uv)).r - 1.0);
        }
        float alpha_roughness = max(roughness * roughness, 1e-4);
        float3 reflectance = mix(float3(0.04), base.rgb, metallic);
        float3 diffuse_color = base.rgb * (1.0 - metallic);
        float3 halfway = normalize(light + view);
        float normal_light = saturate(dot(normal, light));
        float normal_view = max(abs(dot(normal, view)), 1e-4);
        float normal_half = saturate(dot(normal, halfway));
        float3 fresnel = reflectance + (1.0 - reflectance) * pow(1.0 - saturate(dot(view, halfway)), 5.0);
        float roughness_squared = alpha_roughness * alpha_roughness;
        float denominator = normal_half * normal_half * (roughness_squared - 1.0) + 1.0;
        float distribution = roughness_squared / (M_PI_F * denominator * denominator);
        float visibility = 0.5 / max(normal_light * sqrt(normal_view * normal_view * (1.0 - roughness_squared) + roughness_squared) + normal_view * sqrt(normal_light * normal_light * (1.0 - roughness_squared) + roughness_squared), 1e-6);
        float3 direct = ((1.0 - fresnel) * diffuse_color + M_PI_F * fresnel * distribution * visibility) * normal_light * frame.light_color.rgb;
        float3 indirect = frame.ambient.rgb * (diffuse_color + reflectance) * occlusion;
        float3 emissive = material.emissive.rgb;
        if (material.slots[slot_emissive].row_u.w > 0.5)
        {
            emissive *= textures[slot_emissive].sample(samplers[slot_emissive], slot_uv(material, slot_emissive, uv)).rgb;
        }
        color = direct + indirect + emissive;
    }
    return float4(color * alpha, alpha);
}

struct screen_varyings
{
    float4 position [[position]];
};

vertex screen_varyings screen_vertex(uint vertex_index [[vertex_id]])
{
    screen_varyings out;
    out.position = float4(float2((vertex_index << 1) & 2, vertex_index & 2) * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

static float3 cover(texture2d<float> back, float2 position, float4 size)
{
    constexpr sampler linear_sampler(filter::linear, address::clamp_to_edge);
    if (size.z <= 0.0)
    {
        return float3(0.0);
    }
    float2 back_size = float2(back.get_width(), back.get_height());
    float scale = max(size.x / back_size.x, size.y / back_size.y);
    return back.sample(linear_sampler, ((position - size.xy * 0.5) / scale + back_size * 0.5) / back_size).rgb;
}

fragment float4 under_fragment(screen_varyings in [[stage_in]], texture2d<float> back [[texture(0)]], constant float4& size [[buffer(0)]])
{
    return float4(cover(back, in.position.xy, size), 1.0);
}

struct share_targets
{
    float4 window [[color(0)]];
    float4 share [[color(1)]];
};

fragment share_targets share_fragment(screen_varyings in [[stage_in]], texture2d<float> model [[texture(0)]], texture2d<float> back [[texture(1)]], constant float4& size [[buffer(0)]])
{
    float4 color = model.read(uint2(in.position.xy));
    share_targets out;
    out.window = float4(color.rgb + cover(back, in.position.xy, size) * (1.0 - color.a), 1.0);
    out.share = color.a > 0.0 ? float4(color.rgb / color.a, color.a) : float4(0.0);
    return out;
}
