#import <Metal/Metal.h>
#include <simd/simd.h>
#include <algorithm>
#include <cmath>
#include <cstring>
#include <string>
#include <vector>
#include "hv.h"
#include "renderer_common.inc"

static const int kSamples = 4;

struct material_gpu : material_state
{
    id<MTLTexture> textures[kSlotCount];
    id<MTLSamplerState> samplers[kSlotCount];
};

struct primitive_gpu : primitive_state
{
    id<MTLBuffer> positions = nil;
    id<MTLBuffer> normals = nil;
    id<MTLBuffer> uvs = nil;
    id<MTLBuffer> joints = nil;
    id<MTLBuffer> weights = nil;
    id<MTLBuffer> indices = nil;
    id<MTLBuffer> target_positions = nil;
    id<MTLBuffer> target_normals = nil;
    id<MTLBuffer> targets = nil;
};

struct metal_renderer
{
    hv_model* model = NULL;
    std::vector<id<MTLTexture>> srgb_textures;
    std::vector<id<MTLTexture>> linear_textures;
    std::vector<id<MTLSamplerState>> samplers;
    std::vector<material_gpu> materials;
    std::vector<primitive_gpu> primitives;
    std::vector<id<MTLBuffer>> skins;
    std::vector<void*> skin_memory;
    std::vector<int> solid_order;
    std::vector<blend_entry> blend_order;
    id<MTLTexture> msaa = nil;
    id<MTLTexture> depth = nil;
    MTLRenderPassDescriptor* pass = nil;
};

static id<MTLDevice> renderer_device = nil;
static id<MTLLibrary> renderer_library = nil;
static id<MTLRenderPipelineState> surface_pipelines[3][2][2];
static id<MTLRenderPipelineState> under_pipeline = nil;
static id<MTLRenderPipelineState> share_pipelines[2];
static id<MTLDepthStencilState> depth_states[2];
static id<MTLDepthStencilState> depth_off = nil;
static id<MTLTexture> blank_texture = nil;
static id<MTLSamplerState> default_sampler = nil;
static id<MTLBuffer> blank_buffer = nil;
static bool memoryless_supported = false;

static id<MTLRenderPipelineState> make_pipeline(NSString* vertex, NSString* fragment, MTLFunctionConstantValues* constants, bool blend, bool under)
{
    NSError* error = nil;
    MTLRenderPipelineDescriptor* descriptor = [MTLRenderPipelineDescriptor new];
    descriptor.vertexFunction = constants != nil ? [renderer_library newFunctionWithName:vertex constantValues:constants error:&error] : [renderer_library newFunctionWithName:vertex];
    descriptor.fragmentFunction = constants != nil ? [renderer_library newFunctionWithName:fragment constantValues:constants error:&error] : [renderer_library newFunctionWithName:fragment];
    descriptor.rasterSampleCount = kSamples;
    descriptor.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm_sRGB;
    descriptor.depthAttachmentPixelFormat = MTLPixelFormatDepth32Float;
    if (blend || under)
    {
        MTLRenderPipelineColorAttachmentDescriptor* color = descriptor.colorAttachments[0];
        color.blendingEnabled = YES;
        color.sourceRGBBlendFactor = under ? MTLBlendFactorOneMinusDestinationAlpha : MTLBlendFactorOne;
        color.destinationRGBBlendFactor = under ? MTLBlendFactorOne : MTLBlendFactorOneMinusSourceAlpha;
        color.sourceAlphaBlendFactor = under ? MTLBlendFactorOneMinusDestinationAlpha : MTLBlendFactorOne;
        color.destinationAlphaBlendFactor = under ? MTLBlendFactorOne : MTLBlendFactorOneMinusSourceAlpha;
    }
    id<MTLRenderPipelineState> state = [renderer_device newRenderPipelineStateWithDescriptor:descriptor error:&error];
    if (state == nil)
    {
        NSLog(@"engine: pipeline %@ failed %@", fragment, error);
    }
    return state;
}

bool metal_renderer_start(id<MTLDevice> device, const char* shader_source)
{
    if (renderer_device != nil)
    {
        return true;
    }
    NSError* error = nil;
    id<MTLLibrary> library = [device newLibraryWithSource:[NSString stringWithUTF8String:shader_source] options:nil error:&error];
    if (library == nil)
    {
        NSLog(@"engine: renderer shader compile failed %@", error);
        return false;
    }
    renderer_device = device;
    renderer_library = library;
    memoryless_supported = [device supportsFamily:MTLGPUFamilyApple1];
    for (int kind = 0; kind < 3; kind++)
    {
        for (int outline = 0; outline < 2; outline++)
        {
            for (int blend = 0; blend < 2; blend++)
            {
                MTLFunctionConstantValues* constants = [MTLFunctionConstantValues new];
                int kind_value = kind;
                bool outline_value = outline == 1;
                [constants setConstantValue:&kind_value type:MTLDataTypeInt atIndex:0];
                [constants setConstantValue:&outline_value type:MTLDataTypeBool atIndex:1];
                surface_pipelines[kind][outline][blend] = outline == 1 && kind != HV_MATERIAL_MTOON ? nil : make_pipeline(@"surface_vertex", @"surface_fragment", constants, blend == 1, false);
                if (surface_pipelines[kind][outline][blend] == nil && !(outline == 1 && kind != HV_MATERIAL_MTOON))
                {
                    return false;
                }
            }
        }
    }
    under_pipeline = make_pipeline(@"screen_vertex", @"under_fragment", nil, false, true);
    MTLDepthStencilDescriptor* depth = [MTLDepthStencilDescriptor new];
    depth.depthCompareFunction = MTLCompareFunctionGreaterEqual;
    depth.depthWriteEnabled = NO;
    depth_states[0] = [device newDepthStencilStateWithDescriptor:depth];
    depth.depthWriteEnabled = YES;
    depth_states[1] = [device newDepthStencilStateWithDescriptor:depth];
    depth.depthCompareFunction = MTLCompareFunctionAlways;
    depth.depthWriteEnabled = NO;
    depth_off = [device newDepthStencilStateWithDescriptor:depth];
    MTLTextureDescriptor* blank = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:1 height:1 mipmapped:NO];
    blank_texture = [device newTextureWithDescriptor:blank];
    const uint8_t white[4] = {255, 255, 255, 255};
    [blank_texture replaceRegion:MTLRegionMake2D(0, 0, 1, 1) mipmapLevel:0 withBytes:white bytesPerRow:4];
    default_sampler = [device newSamplerStateWithDescriptor:[MTLSamplerDescriptor new]];
    blank_buffer = [device newBufferWithLength:64 options:MTLResourceStorageModeShared];
    return under_pipeline != nil && blank_texture != nil && blank_buffer != nil;
}

static id<MTLBuffer> make_buffer(const void* bytes, size_t length)
{
    if (bytes == NULL || length == 0)
    {
        return nil;
    }
    return [renderer_device newBufferWithBytes:bytes length:length options:MTLResourceStorageModeShared];
}

static id<MTLBuffer> shared_buffer(std::vector<id<MTLBuffer>>& statics, size_t& next, const void* bytes, size_t length)
{
    if (next == statics.size())
    {
        statics.push_back(make_buffer(bytes, length));
    }
    return statics[next++];
}

void metal_renderer_destroy(metal_renderer* r, hv_model* model)
{
    delete r;
    hv_free(model);
}

metal_renderer* metal_renderer_create(hv_model* model, const std::vector<id<MTLTexture>>& srgb_textures, const std::vector<id<MTLTexture>>& linear_textures, const std::vector<id<MTLSamplerState>>& samplers, std::vector<id<MTLBuffer>>& statics)
{
    metal_renderer* r = new metal_renderer();
    size_t next = 0;
    r->model = model;
    r->srgb_textures = srgb_textures;
    r->linear_textures = linear_textures;
    r->samplers = samplers;
    int32_t material_count = hv_material_count(model);
    r->materials.resize((size_t)material_count + 1);
    for (int32_t index = 0; index < material_count; index++)
    {
        bind_material(r, r->materials[index], hv_material_at(model, index), blank_texture, default_sampler);
    }
    hv_material fallback = fallback_material();
    bind_material(r, r->materials[material_count], &fallback, blank_texture, default_sampler);
    for (int32_t skin = 0; skin < hv_skin_count(model); skin++)
    {
        id<MTLBuffer> buffer = [renderer_device newBufferWithLength:std::max((size_t)hv_skin_joint_count(model, skin), (size_t)1) * 64 options:MTLResourceStorageModeShared];
        if (buffer == nil)
        {
            delete r;
            return NULL;
        }
        r->skins.push_back(buffer);
        r->skin_memory.push_back(buffer.contents);
    }
    int32_t primitive_count = hv_primitive_count(model);
    r->primitives.resize((size_t)primitive_count);
    for (int32_t index = 0; index < primitive_count; index++)
    {
        const hv_primitive* source = hv_primitive_at(model, index);
        primitive_gpu& gpu = r->primitives[index];
        size_t vertices = source->vertex_count;
        gpu.source = source;
        gpu.material = source->material >= 0 && source->material < material_count ? source->material : material_count;
        gpu.positions = shared_buffer(statics, next, source->positions, vertices * 12);
        gpu.normals = shared_buffer(statics, next, source->normals, vertices * 12);
        gpu.uvs = shared_buffer(statics, next, source->uvs, vertices * 8);
        gpu.indices = shared_buffer(statics, next, source->indices, (size_t)source->index_count * 4);
        bool skinned = source->skin >= 0 && source->skin < hv_skin_count(model) && source->joints != NULL && source->weights != NULL;
        if (skinned)
        {
            gpu.joints = shared_buffer(statics, next, source->joints, vertices * 8);
            gpu.weights = shared_buffer(statics, next, source->weights, vertices * 16);
        }
        if (source->target_count > 0 && source->target_positions != NULL)
        {
            gpu.target_positions = shared_buffer(statics, next, source->target_positions, (size_t)source->target_count * vertices * 12);
            gpu.target_normals = shared_buffer(statics, next, source->target_normals, (size_t)source->target_count * vertices * 12);
            gpu.targets = [renderer_device newBufferWithLength:source->target_count * sizeof(active_target) options:MTLResourceStorageModeShared];
            gpu.mapped_targets = (active_target*)gpu.targets.contents;
        }
        gpu.flags = (skinned ? draw_skinned : 0) | (gpu.normals != nil ? draw_normals : 0) | (gpu.uvs != nil ? draw_uvs : 0) | (gpu.target_normals != nil ? draw_target_normals : 0);
        if (gpu.positions == nil || gpu.indices == nil || (skinned && (gpu.joints == nil || gpu.weights == nil)))
        {
            gpu.source = NULL;
            continue;
        }
        place_primitive(model, gpu);
    }
    draw_orders(r->materials, r->primitives, r->solid_order, r->blend_order);
    r->pass = [MTLRenderPassDescriptor renderPassDescriptor];
    r->pass.colorAttachments[0].loadAction = MTLLoadActionClear;
    r->pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0);
    r->pass.colorAttachments[0].storeAction = kSamples > 1 ? MTLStoreActionMultisampleResolve : MTLStoreActionStore;
    r->pass.depthAttachment.loadAction = MTLLoadActionClear;
    r->pass.depthAttachment.clearDepth = 0.0;
    r->pass.depthAttachment.storeAction = MTLStoreActionDontCare;
    return r;
}

static void encode_primitive(metal_renderer* r, id<MTLRenderCommandEncoder> encoder, int index, bool outline)
{
    const primitive_gpu& gpu = r->primitives[index];
    const material_gpu& material = r->materials[gpu.material];
    const hv_primitive* source = gpu.source;
    [encoder setRenderPipelineState:surface_pipelines[material.kind][outline ? 1 : 0][material.alpha_mode == HV_ALPHA_BLEND ? 1 : 0]];
    [encoder setDepthStencilState:depth_states[material.depth_write ? 1 : 0]];
    [encoder setCullMode:outline ? MTLCullModeFront : (material.double_sided ? MTLCullModeNone : MTLCullModeBack)];
    [encoder setFrontFacingWinding:draw_mirrored(gpu) ? MTLWindingClockwise : MTLWindingCounterClockwise];
    id<MTLBuffer> buffers[9] = {gpu.positions, gpu.normals ?: blank_buffer, gpu.uvs ?: blank_buffer, gpu.joints ?: blank_buffer, gpu.weights ?: blank_buffer, (gpu.flags & draw_skinned) != 0 ? r->skins[source->skin] : blank_buffer, gpu.target_positions ?: blank_buffer, gpu.target_normals ?: blank_buffer, gpu.targets ?: blank_buffer};
    const NSUInteger offsets[9] = {0, 0, 0, 0, 0, 0, 0, 0, 0};
    [encoder setVertexBuffers:buffers offsets:offsets withRange:NSMakeRange(3, 9)];
    [encoder setVertexTexture:material.textures[7] atIndex:0];
    [encoder setVertexTexture:material.textures[8] atIndex:1];
    [encoder setVertexSamplerState:material.samplers[7] atIndex:0];
    [encoder setVertexSamplerState:material.samplers[8] atIndex:1];
    [encoder setFragmentTextures:material.textures withRange:NSMakeRange(0, kSlotCount)];
    [encoder setFragmentSamplerStates:material.samplers withRange:NSMakeRange(0, kSlotCount)];
    [encoder drawIndexedPrimitives:MTLPrimitiveTypeTriangle indexCount:source->index_count indexType:MTLIndexTypeUInt32 indexBuffer:gpu.indices indexBufferOffset:0];
}

static void draw_primitive(metal_renderer* r, id<MTLRenderCommandEncoder> encoder, int index, float seconds)
{
    const material_gpu& material = r->materials[r->primitives[index].material];
    draw_uniforms draw = draw_for(r->primitives[index]);
    material_uniforms uniforms;
    animated_uniforms(uniforms, material, seconds);
    [encoder setVertexBytes:&draw length:sizeof(draw) atIndex:0];
    [encoder setVertexBytes:&uniforms length:sizeof(uniforms) atIndex:2];
    [encoder setFragmentBytes:&uniforms length:sizeof(uniforms) atIndex:2];
    encode_primitive(r, encoder, index, false);
    if (material.outline)
    {
        encode_primitive(r, encoder, index, true);
    }
}

static void size_targets(metal_renderer* r, int width, int height)
{
    if (r->depth != nil && (int)r->depth.width == width && (int)r->depth.height == height)
    {
        return;
    }
    MTLTextureDescriptor* descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatDepth32Float width:width height:height mipmapped:NO];
    descriptor.textureType = kSamples > 1 ? MTLTextureType2DMultisample : MTLTextureType2D;
    descriptor.sampleCount = kSamples;
    descriptor.storageMode = memoryless_supported ? MTLStorageModeMemoryless : MTLStorageModePrivate;
    descriptor.usage = MTLTextureUsageRenderTarget;
    r->depth = [renderer_device newTextureWithDescriptor:descriptor];
    descriptor.pixelFormat = MTLPixelFormatBGRA8Unorm_sRGB;
    r->msaa = kSamples > 1 ? [renderer_device newTextureWithDescriptor:descriptor] : nil;
}

void metal_renderer_draw(metal_renderer* r, id<MTLCommandBuffer> command_buffer, id<MTLTexture> target, const float* view_projection, const float* eye, const float* light_direction, const float* light_color, const float* ambient, double seconds, uint32_t changed, id<MTLTexture> back, bool opaque)
{
    if (upload_changes(r, changed))
    {
        for (int32_t index = 0; index < hv_material_count(r->model); index++)
        {
            bind_material(r, r->materials[index], hv_material_at(r->model, index), blank_texture, default_sampler);
        }
    }
    sort_blend(r->model, r->primitives, r->blend_order, eye);
    size_targets(r, (int)target.width, (int)target.height);
    if (kSamples > 1)
    {
        r->pass.colorAttachments[0].texture = r->msaa;
        r->pass.colorAttachments[0].resolveTexture = target;
    }
    else
    {
        r->pass.colorAttachments[0].texture = target;
    }
    r->pass.depthAttachment.texture = r->depth;
    frame_uniforms frame = frame_for(view_projection, eye, light_direction, light_color, ambient, (int)target.width, (int)target.height);
    id<MTLRenderCommandEncoder> encoder = [command_buffer renderCommandEncoderWithDescriptor:r->pass];
    [encoder setVertexBytes:&frame length:sizeof(frame) atIndex:1];
    [encoder setFragmentBytes:&frame length:sizeof(frame) atIndex:1];
    float clock = (float)seconds;
    for (int index : r->solid_order)
    {
        draw_primitive(r, encoder, index, clock);
    }
    for (const blend_entry& entry : r->blend_order)
    {
        draw_primitive(r, encoder, entry.primitive, clock);
    }
    if (opaque)
    {
        simd::float4 size = {(float)target.width, (float)target.height, back != nil ? 1.0f : 0.0f, 0.0f};
        [encoder setRenderPipelineState:under_pipeline];
        [encoder setDepthStencilState:depth_off];
        [encoder setCullMode:MTLCullModeNone];
        [encoder setFragmentTexture:back != nil ? back : blank_texture atIndex:0];
        [encoder setFragmentBytes:&size length:sizeof(size) atIndex:0];
        [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
    }
    [encoder endEncoding];
}

bool metal_renderer_share(id<MTLCommandBuffer> command_buffer, id<MTLTexture> model, id<MTLTexture> back, id<MTLTexture> window_target, id<MTLTexture> share_target)
{
    int shown = window_target != nil ? 0 : 1;
    if (share_pipelines[shown] == nil)
    {
        MTLRenderPipelineDescriptor* descriptor = [MTLRenderPipelineDescriptor new];
        descriptor.vertexFunction = [renderer_library newFunctionWithName:@"screen_vertex"];
        descriptor.fragmentFunction = [renderer_library newFunctionWithName:@"share_fragment"];
        descriptor.colorAttachments[0].pixelFormat = window_target != nil ? MTLPixelFormatBGRA8Unorm_sRGB : MTLPixelFormatInvalid;
        descriptor.colorAttachments[1].pixelFormat = MTLPixelFormatBGRA8Unorm_sRGB;
        NSError* error = nil;
        share_pipelines[shown] = [renderer_device newRenderPipelineStateWithDescriptor:descriptor error:&error];
        if (share_pipelines[shown] == nil)
        {
            NSLog(@"engine: share pipeline failed, drawing the window only %@", error);
            return false;
        }
    }
    MTLRenderPassDescriptor* pass = [MTLRenderPassDescriptor renderPassDescriptor];
    pass.colorAttachments[0].texture = window_target;
    pass.colorAttachments[0].loadAction = MTLLoadActionDontCare;
    pass.colorAttachments[0].storeAction = MTLStoreActionStore;
    pass.colorAttachments[1].texture = share_target;
    pass.colorAttachments[1].loadAction = MTLLoadActionDontCare;
    pass.colorAttachments[1].storeAction = MTLStoreActionStore;
    simd::float4 size = {(float)share_target.width, (float)share_target.height, back != nil ? 1.0f : 0.0f, 0.0f};
    id<MTLRenderCommandEncoder> encoder = [command_buffer renderCommandEncoderWithDescriptor:pass];
    [encoder setRenderPipelineState:share_pipelines[shown]];
    [encoder setFragmentTexture:model atIndex:0];
    [encoder setFragmentTexture:back != nil ? back : model atIndex:1];
    [encoder setFragmentBytes:&size length:sizeof(size) atIndex:0];
    [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
    [encoder endEncoding];
    return true;
}
