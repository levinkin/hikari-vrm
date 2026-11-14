#version 450

const int slot_base_color = 0;
const int slot_shade_multiply = 1;
const int slot_shading_shift = 2;
const int slot_normal = 3;
const int slot_emissive = 4;
const int slot_matcap = 5;
const int slot_rim_multiply = 6;
const int slot_outline_width = 7;
const int slot_uv_animation_mask = 8;
const int slot_metallic_roughness = 9;
const int slot_occlusion = 10;

const uint draw_skinned = 1u;
const uint draw_normals = 2u;
const uint draw_uvs = 4u;
const uint draw_target_normals = 8u;

#if defined(SURFACE_VERTEX) || defined(SURFACE_FRAGMENT)
layout(constant_id = 0) const int material_kind = 0;
layout(constant_id = 1) const bool outline_pass = false;

struct texture_slot
{
    vec4 row_u;
    vec4 row_v;
};

layout(set = 0, binding = 0) uniform frame_uniforms
{
    mat4 view_projection;
    vec4 camera_position;
    vec4 light_direction;
    vec4 light_color;
    vec4 ambient;
    vec4 viewport;
} frame;

layout(set = 0, binding = 1) uniform material_uniforms
{
    vec4 base_color;
    vec4 emissive;
    vec4 shade_color;
    vec4 matcap_color;
    vec4 rim_color;
    vec4 outline_color;
    vec4 rim_shape;
    vec4 surface;
    vec4 alpha_outline;
    vec4 uv_animation;
    texture_slot slots[11];
} material;

layout(set = 1, binding = 0) uniform sampler2D textures[11];

vec2 slot_uv(int slot, vec2 uv)
{
    vec3 point = vec3(uv, 1.0);
    return vec2(dot(material.slots[slot].row_u.xyz, point), dot(material.slots[slot].row_v.xyz, point));
}

vec2 animated_uv(vec2 uv, float mask)
{
    float angle = material.uv_animation.z * mask;
    float cosine = cos(angle);
    float sine = sin(angle);
    vec2 centered = uv - 0.5;
    vec2 rotated = vec2(cosine * centered.x - sine * centered.y, sine * centered.x + cosine * centered.y) + 0.5;
    return rotated + material.uv_animation.xy * mask;
}
#endif

#ifdef SURFACE_VERTEX
struct active_target
{
    uint index;
    float weight;
};

layout(push_constant) uniform draw_uniforms
{
    mat4 world;
    uint vertex_count;
    uint active_target_count;
    uint flags;
    uint padding;
} draw;

layout(std430, set = 2, binding = 0) readonly buffer positions_buffer { float positions[]; };
layout(std430, set = 2, binding = 1) readonly buffer normals_buffer { float normals[]; };
layout(std430, set = 2, binding = 2) readonly buffer uvs_buffer { vec2 uvs[]; };
layout(std430, set = 2, binding = 3) readonly buffer joints_buffer { uint joints[]; };
layout(std430, set = 2, binding = 4) readonly buffer weights_buffer { vec4 weights[]; };
layout(std430, set = 2, binding = 5) readonly buffer joint_matrices_buffer { mat4 joint_matrices[]; };
layout(std430, set = 2, binding = 6) readonly buffer target_positions_buffer { float target_positions[]; };
layout(std430, set = 2, binding = 7) readonly buffer target_normals_buffer { float target_normals[]; };
layout(std430, set = 2, binding = 8) readonly buffer targets_buffer { active_target targets[]; };

layout(location = 0) out vec3 out_world_position;
layout(location = 1) out vec3 out_world_normal;
layout(location = 2) out vec2 out_uv;

vec3 packed3(uint index, uint which)
{
    if (which == 0u)
    {
        return vec3(positions[index * 3u], positions[index * 3u + 1u], positions[index * 3u + 2u]);
    }
    if (which == 1u)
    {
        return vec3(normals[index * 3u], normals[index * 3u + 1u], normals[index * 3u + 2u]);
    }
    if (which == 2u)
    {
        return vec3(target_positions[index * 3u], target_positions[index * 3u + 1u], target_positions[index * 3u + 2u]);
    }
    return vec3(target_normals[index * 3u], target_normals[index * 3u + 1u], target_normals[index * 3u + 2u]);
}

void main()
{
    uint vertex_index = uint(gl_VertexIndex);
    vec3 position = packed3(vertex_index, 0u);
    vec3 normal = (draw.flags & draw_normals) != 0u ? packed3(vertex_index, 1u) : vec3(0.0, 0.0, 1.0);
    for (uint target = 0u; target < draw.active_target_count; target++)
    {
        uint offset = targets[target].index * draw.vertex_count + vertex_index;
        position += packed3(offset, 2u) * targets[target].weight;
        if ((draw.flags & draw_target_normals) != 0u)
        {
            normal += packed3(offset, 3u) * targets[target].weight;
        }
    }
    mat4 world = draw.world;
    if ((draw.flags & draw_skinned) != 0u)
    {
        uint low = joints[vertex_index * 2u];
        uint high = joints[vertex_index * 2u + 1u];
        vec4 weight = weights[vertex_index];
        world = joint_matrices[low & 0xffffu] * weight.x + joint_matrices[low >> 16] * weight.y + joint_matrices[high & 0xffffu] * weight.z + joint_matrices[high >> 16] * weight.w;
    }
    mat3 linear_part = mat3(world[0].xyz, world[1].xyz, world[2].xyz);
    float handedness = determinant(linear_part) < 0.0 ? -1.0 : 1.0;
    mat3 normal_matrix = mat3(cross(linear_part[1], linear_part[2]), cross(linear_part[2], linear_part[0]), cross(linear_part[0], linear_part[1]));
    vec3 world_normal = normalize(normal_matrix * normal * handedness);
    vec4 world_position = world * vec4(position, 1.0);
    out_uv = (draw.flags & draw_uvs) != 0u ? uvs[vertex_index] : vec2(0.0);
    out_world_normal = world_normal;
    if (outline_pass)
    {
        float width = material.alpha_outline.y;
        if (material.slots[slot_outline_width].row_u.w > 0.5)
        {
            float mask = material.slots[slot_uv_animation_mask].row_u.w > 0.5 ? textureLod(textures[slot_uv_animation_mask], slot_uv(slot_uv_animation_mask, out_uv), 0.0).b : 1.0;
            vec2 uv = animated_uv(out_uv, mask);
            width *= textureLod(textures[slot_outline_width], slot_uv(slot_outline_width, uv), 0.0).g;
        }
        if (material.alpha_outline.w < 1.5)
        {
            world_position.xyz += world_normal * width;
            gl_Position = frame.view_projection * world_position;
        }
        else
        {
            gl_Position = frame.view_projection * world_position;
            vec2 projected = (frame.view_projection * vec4(world_normal, 0.0)).xy * frame.viewport.xy;
            float length_squared = dot(projected, projected);
            vec2 direction = length_squared > 0.0 ? projected * inversesqrt(length_squared) : vec2(0.0);
            gl_Position.xy += direction * vec2(2.0 * width * frame.viewport.y / frame.viewport.x, 2.0 * width) * gl_Position.w;
        }
    }
    else
    {
        gl_Position = frame.view_projection * world_position;
    }
    out_world_position = world_position.xyz;
}
#endif

#ifdef SURFACE_FRAGMENT
layout(location = 0) in vec3 in_world_position;
layout(location = 1) in vec3 in_world_normal;
layout(location = 2) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

const float pi = 3.14159265358979323846;

void main()
{
    float mask = material.slots[slot_uv_animation_mask].row_u.w > 0.5 ? texture(textures[slot_uv_animation_mask], slot_uv(slot_uv_animation_mask, in_uv)).b : 1.0;
    vec2 uv = material_kind == 2 ? animated_uv(in_uv, mask) : in_uv;
    vec4 base = material.base_color;
    if (material.slots[slot_base_color].row_u.w > 0.5)
    {
        base *= texture(textures[slot_base_color], slot_uv(slot_base_color, uv));
    }
    int alpha_mode = int(material.alpha_outline.z);
    if (alpha_mode == 1 && base.a < material.alpha_outline.x)
    {
        discard;
    }
    float alpha = alpha_mode == 2 ? base.a : 1.0;
    vec3 normal = normalize(in_world_normal);
    if (!outline_pass && !gl_FrontFacing)
    {
        normal = -normal;
    }
    if (material.slots[slot_normal].row_u.w > 0.5)
    {
        vec2 normal_uv = slot_uv(slot_normal, uv);
        vec3 position_dx = dFdx(in_world_position);
        vec3 position_dy = -dFdy(in_world_position);
        vec2 uv_dx = dFdx(normal_uv);
        vec2 uv_dy = -dFdy(normal_uv);
        vec3 across_dy = cross(position_dy, normal);
        vec3 across_dx = cross(normal, position_dx);
        vec3 tangent = across_dy * uv_dx.x + across_dx * uv_dy.x;
        vec3 bitangent = across_dy * uv_dx.y + across_dx * uv_dy.y;
        float scale_squared = max(dot(tangent, tangent), dot(bitangent, bitangent));
        if (scale_squared > 0.0)
        {
            float inverse_scale = inversesqrt(scale_squared);
            vec3 sampled = texture(textures[slot_normal], normal_uv).xyz * 2.0 - 1.0;
            sampled.xy *= material.surface.x;
            normal = normalize(tangent * inverse_scale * sampled.x - bitangent * inverse_scale * sampled.y + normal * sampled.z);
        }
    }
    vec3 view = normalize(frame.camera_position.xyz - in_world_position);
    vec3 light = frame.light_direction.xyz;
    vec3 color;
    if (material_kind == 1)
    {
        color = base.rgb;
    }
    else if (material_kind == 2)
    {
        float shading = dot(normal, light) + material.shade_color.w;
        if (material.slots[slot_shading_shift].row_u.w > 0.5)
        {
            shading += texture(textures[slot_shading_shift], slot_uv(slot_shading_shift, uv)).r * material.rim_shape.w;
        }
        shading = clamp((shading + 1.0 - material.matcap_color.w) / max(2.0 - 2.0 * material.matcap_color.w, 1e-6), 0.0, 1.0);
        vec3 shade = material.shade_color.rgb;
        if (material.slots[slot_shade_multiply].row_u.w > 0.5)
        {
            shade *= texture(textures[slot_shade_multiply], slot_uv(slot_shade_multiply, uv)).rgb;
        }
        color = mix(shade, base.rgb, shading) * frame.light_color.rgb;
        vec3 illumination = frame.ambient.rgb;
        color += illumination * base.rgb;
        vec3 emissive = material.emissive.rgb;
        if (material.slots[slot_emissive].row_u.w > 0.5)
        {
            emissive *= texture(textures[slot_emissive], slot_uv(slot_emissive, uv)).rgb;
        }
        color += emissive;
        vec3 view_x = normalize(vec3(view.z, 0.0, -view.x));
        vec3 view_y = cross(view, view_x);
        vec2 matcap_uv = vec2(dot(view_x, normal), -dot(view_y, normal)) * 0.495 + 0.5;
        vec3 rim = material.slots[slot_matcap].row_u.w > 0.5 ? material.matcap_color.rgb * texture(textures[slot_matcap], matcap_uv).rgb : vec3(0.0);
        float parametric = material.rim_shape.x > 0.0 ? pow(clamp(1.0 - dot(normal, view) + material.rim_shape.y, 0.0, 1.0), material.rim_shape.x) : 1.0;
        rim += parametric * material.rim_color.rgb;
        if (material.slots[slot_rim_multiply].row_u.w > 0.5)
        {
            rim *= texture(textures[slot_rim_multiply], slot_uv(slot_rim_multiply, uv)).rgb;
        }
        rim *= mix(vec3(1.0), frame.light_color.rgb + illumination, material.rim_color.w);
        color += rim;
        if (outline_pass)
        {
            color = material.outline_color.rgb * mix(vec3(1.0), color, material.outline_color.w);
        }
    }
    else
    {
        float metallic = material.surface.y;
        float roughness = material.surface.z;
        if (material.slots[slot_metallic_roughness].row_u.w > 0.5)
        {
            vec4 sampled = texture(textures[slot_metallic_roughness], slot_uv(slot_metallic_roughness, uv));
            roughness *= sampled.g;
            metallic *= sampled.b;
        }
        float occlusion = 1.0;
        if (material.slots[slot_occlusion].row_u.w > 0.5)
        {
            occlusion = 1.0 + material.surface.w * (texture(textures[slot_occlusion], slot_uv(slot_occlusion, uv)).r - 1.0);
        }
        float alpha_roughness = max(roughness * roughness, 1e-4);
        vec3 reflectance = mix(vec3(0.04), base.rgb, metallic);
        vec3 diffuse_color = base.rgb * (1.0 - metallic);
        vec3 halfway = normalize(light + view);
        float normal_light = clamp(dot(normal, light), 0.0, 1.0);
        float normal_view = max(abs(dot(normal, view)), 1e-4);
        float normal_half = clamp(dot(normal, halfway), 0.0, 1.0);
        vec3 fresnel = reflectance + (1.0 - reflectance) * pow(1.0 - clamp(dot(view, halfway), 0.0, 1.0), 5.0);
        float roughness_squared = alpha_roughness * alpha_roughness;
        float denominator = normal_half * normal_half * (roughness_squared - 1.0) + 1.0;
        float distribution = roughness_squared / (pi * denominator * denominator);
        float visibility = 0.5 / max(normal_light * sqrt(normal_view * normal_view * (1.0 - roughness_squared) + roughness_squared) + normal_view * sqrt(normal_light * normal_light * (1.0 - roughness_squared) + roughness_squared), 1e-6);
        vec3 direct = ((1.0 - fresnel) * diffuse_color + pi * fresnel * distribution * visibility) * normal_light * frame.light_color.rgb;
        vec3 indirect = frame.ambient.rgb * (diffuse_color + reflectance) * occlusion;
        vec3 emissive = material.emissive.rgb;
        if (material.slots[slot_emissive].row_u.w > 0.5)
        {
            emissive *= texture(textures[slot_emissive], slot_uv(slot_emissive, uv)).rgb;
        }
        color = direct + indirect + emissive;
    }
    out_color = vec4(color * alpha, alpha);
}
#endif

#ifdef SCREEN_VERTEX
void main()
{
    int index = gl_VertexIndex;
    gl_Position = vec4(vec2((index << 1) & 2, index & 2) * 2.0 - 1.0, 0.0, 1.0);
}
#endif

#if defined(UNDER_FRAGMENT) || defined(SHARE_FRAGMENT)
layout(push_constant) uniform screen_uniforms
{
    vec4 size;
} screen;

layout(set = 0, binding = 0) uniform sampler2D model;
layout(set = 0, binding = 1) uniform sampler2D back;

vec3 cover(vec2 position)
{
    if (screen.size.z <= 0.0)
    {
        return vec3(0.0);
    }
    vec2 back_size = vec2(textureSize(back, 0));
    float scale = max(screen.size.x / back_size.x, screen.size.y / back_size.y);
    return texture(back, ((position - screen.size.xy * 0.5) / scale + back_size * 0.5) / back_size).rgb;
}
#endif

#ifdef UNDER_FRAGMENT
layout(location = 0) out vec4 out_color;

void main()
{
    out_color = vec4(cover(gl_FragCoord.xy), 1.0);
}
#endif

#ifdef SHARE_FRAGMENT
layout(location = 0) out vec4 out_window;
layout(location = 1) out vec4 out_share;

void main()
{
    vec4 color = texelFetch(model, ivec2(gl_FragCoord.xy), 0);
    out_window = vec4(color.rgb + cover(gl_FragCoord.xy) * (1.0 - color.a), 1.0);
    out_share = color.a > 0.0 ? vec4(color.rgb / color.a, color.a) : vec4(0.0);
}
#endif
