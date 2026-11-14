#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <vector>
#include "renderer_vk.h"
#include "renderer_common.inc"
#include "mtoon_spv.inc"

static const VkSampleCountFlagBits kSamples = VK_SAMPLE_COUNT_4_BIT;
static const VkFormat kDepthFormat = VK_FORMAT_D32_SFLOAT;
static const int kBufferSlots = 9;

enum
{
    blend_none = 0,
    blend_over = 1,
    blend_under = 2,
};

struct material_gpu : material_state
{
    VkDescriptorSet set = VK_NULL_HANDLE;
    VkImageView textures[kSlotCount] = {};
    VkSampler samplers[kSlotCount] = {};
};

struct primitive_gpu : primitive_state
{
    vk_buffer buffers[kBufferSlots];
    VkDescriptorSet set = VK_NULL_HANDLE;
};

struct vk_renderer
{
    hv_model* model = NULL;
    std::vector<VkImageView> srgb_textures;
    std::vector<VkImageView> linear_textures;
    std::vector<VkSampler> samplers;
    std::vector<material_gpu> materials;
    std::vector<primitive_gpu> primitives;
    std::vector<vk_buffer> skins;
    std::vector<void*> skin_memory;
    std::vector<int> solid_order;
    std::vector<blend_entry> blend_order;
    VkDescriptorPool pool = VK_NULL_HANDLE;
    VkDescriptorSet uniform_set = VK_NULL_HANDLE;
    vk_buffer ring;
    VkDeviceSize material_stride = 0;
    vk_image msaa;
    vk_image depth;
};

static VkDescriptorSetLayout set_layouts[3];
static VkPipelineLayout pipeline_layout = VK_NULL_HANDLE;
static VkPipeline surface_pipelines[3][2][2];
static VkDescriptorSetLayout screen_set_layout = VK_NULL_HANDLE;
static VkPipelineLayout screen_layout = VK_NULL_HANDLE;
static VkPipeline under_pipeline = VK_NULL_HANDLE;
static VkPipeline share_pipeline = VK_NULL_HANDLE;
static VkDescriptorPool screen_pool = VK_NULL_HANDLE;
static VkDescriptorSet screen_sets[2];
static VkImageView screen_bound[2][2];
static vk_image blank_texture;
static VkSampler default_sampler = VK_NULL_HANDLE;
static vk_buffer blank_buffer;
static VkDeviceSize uniform_alignment = 256;

static uint32_t memory_type(uint32_t bits, VkMemoryPropertyFlags wanted)
{
    VkPhysicalDeviceMemoryProperties memory;
    vkGetPhysicalDeviceMemoryProperties(physical_device, &memory);
    for (uint32_t index = 0; index < memory.memoryTypeCount; index++)
    {
        if ((bits & (1u << index)) != 0 && (memory.memoryTypes[index].propertyFlags & wanted) == wanted)
        {
            return index;
        }
    }
    return UINT32_MAX;
}

bool vk_buffer_create(vk_buffer& out, VkDeviceSize size, VkBufferUsageFlags usage, bool host)
{
    VkBufferCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO;
    info.size = size;
    info.usage = usage;
    info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    if (vkCreateBuffer(device, &info, NULL, &out.buffer) != VK_SUCCESS)
    {
        out.buffer = VK_NULL_HANDLE;
        return false;
    }
    VkMemoryRequirements requirements;
    vkGetBufferMemoryRequirements(device, out.buffer, &requirements);
    VkMemoryAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate.allocationSize = requirements.size;
    VkMemoryPropertyFlags wanted = host ? VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT : VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT;
    allocate.memoryTypeIndex = host && (usage & VK_BUFFER_USAGE_TRANSFER_DST_BIT) != 0 ? memory_type(requirements.memoryTypeBits, wanted | VK_MEMORY_PROPERTY_HOST_CACHED_BIT) : UINT32_MAX;
    if (allocate.memoryTypeIndex == UINT32_MAX)
    {
        allocate.memoryTypeIndex = memory_type(requirements.memoryTypeBits, wanted);
    }
    if (allocate.memoryTypeIndex == UINT32_MAX || vkAllocateMemory(device, &allocate, NULL, &out.memory) != VK_SUCCESS)
    {
        out.memory = VK_NULL_HANDLE;
        vk_buffer_destroy(out);
        return false;
    }
    vkBindBufferMemory(device, out.buffer, out.memory, 0);
    if (host && vkMapMemory(device, out.memory, 0, VK_WHOLE_SIZE, 0, &out.mapped) != VK_SUCCESS)
    {
        vk_buffer_destroy(out);
        return false;
    }
    return true;
}

void vk_buffer_destroy(vk_buffer& buffer)
{
    vkDestroyBuffer(device, buffer.buffer, NULL);
    vkFreeMemory(device, buffer.memory, NULL);
    buffer = vk_buffer();
}

bool vk_image_create(vk_image& out, uint32_t width, uint32_t height, uint32_t levels, VkFormat format, VkSampleCountFlagBits samples, VkImageUsageFlags usage, VkImageAspectFlags aspect)
{
    VkImageCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    info.imageType = VK_IMAGE_TYPE_2D;
    info.format = format;
    info.extent = {width, height, 1};
    info.mipLevels = levels;
    info.arrayLayers = 1;
    info.samples = samples;
    info.tiling = VK_IMAGE_TILING_OPTIMAL;
    info.usage = usage;
    info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    if (vkCreateImage(device, &info, NULL, &out.image) != VK_SUCCESS)
    {
        out.image = VK_NULL_HANDLE;
        return false;
    }
    VkMemoryRequirements requirements;
    vkGetImageMemoryRequirements(device, out.image, &requirements);
    VkMemoryAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate.allocationSize = requirements.size;
    allocate.memoryTypeIndex = (usage & VK_IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT) != 0 ? memory_type(requirements.memoryTypeBits, VK_MEMORY_PROPERTY_LAZILY_ALLOCATED_BIT) : UINT32_MAX;
    if (allocate.memoryTypeIndex == UINT32_MAX)
    {
        allocate.memoryTypeIndex = memory_type(requirements.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    }
    if (allocate.memoryTypeIndex == UINT32_MAX || vkAllocateMemory(device, &allocate, NULL, &out.memory) != VK_SUCCESS)
    {
        out.memory = VK_NULL_HANDLE;
        vk_image_destroy(out);
        return false;
    }
    vkBindImageMemory(device, out.image, out.memory, 0);
    VkImageViewCreateInfo view{};
    view.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO;
    view.image = out.image;
    view.viewType = VK_IMAGE_VIEW_TYPE_2D;
    view.format = format;
    view.subresourceRange = {aspect, 0, levels, 0, 1};
    if (vkCreateImageView(device, &view, NULL, &out.view) != VK_SUCCESS)
    {
        out.view = VK_NULL_HANDLE;
        vk_image_destroy(out);
        return false;
    }
    out.width = width;
    out.height = height;
    return true;
}

void vk_image_destroy(vk_image& image)
{
    vkDestroyImageView(device, image.view, NULL);
    vkDestroyImage(device, image.image, NULL);
    vkFreeMemory(device, image.memory, NULL);
    image = vk_image();
}

bool vk_host_image_create(vk_image& out, uint32_t width, uint32_t height, VkFormat format)
{
    VkFormatProperties properties;
    vkGetPhysicalDeviceFormatProperties(physical_device, format, &properties);
    if ((properties.linearTilingFeatures & VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT) == 0)
    {
        return false;
    }
    VkImageCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    info.imageType = VK_IMAGE_TYPE_2D;
    info.format = format;
    info.extent = {width, height, 1};
    info.mipLevels = 1;
    info.arrayLayers = 1;
    info.samples = VK_SAMPLE_COUNT_1_BIT;
    info.tiling = VK_IMAGE_TILING_LINEAR;
    info.usage = VK_IMAGE_USAGE_SAMPLED_BIT;
    info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    info.initialLayout = VK_IMAGE_LAYOUT_PREINITIALIZED;
    if (vkCreateImage(device, &info, NULL, &out.image) != VK_SUCCESS)
    {
        out.image = VK_NULL_HANDLE;
        return false;
    }
    VkMemoryRequirements requirements;
    vkGetImageMemoryRequirements(device, out.image, &requirements);
    VkMemoryAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate.allocationSize = requirements.size;
    allocate.memoryTypeIndex = memory_type(requirements.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    VkImageViewCreateInfo view{};
    view.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO;
    view.image = out.image;
    view.viewType = VK_IMAGE_VIEW_TYPE_2D;
    view.format = format;
    view.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    void* mapped = NULL;
    if (allocate.memoryTypeIndex == UINT32_MAX || vkAllocateMemory(device, &allocate, NULL, &out.memory) != VK_SUCCESS || vkBindImageMemory(device, out.image, out.memory, 0) != VK_SUCCESS || vkMapMemory(device, out.memory, 0, VK_WHOLE_SIZE, 0, &mapped) != VK_SUCCESS || vkCreateImageView(device, &view, NULL, &out.view) != VK_SUCCESS)
    {
        vk_image_destroy(out);
        return false;
    }
    VkImageSubresource subresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0};
    VkSubresourceLayout layout;
    vkGetImageSubresourceLayout(device, out.image, &subresource, &layout);
    out.mapped = (uint8_t*)mapped + layout.offset;
    out.row_pitch = layout.rowPitch;
    out.width = width;
    out.height = height;
    VkCommandBuffer commands = vk_once_begin();
    vk_barrier(commands, out.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_PREINITIALIZED, VK_IMAGE_LAYOUT_GENERAL, VK_PIPELINE_STAGE_HOST_BIT, VK_ACCESS_HOST_WRITE_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_ACCESS_SHADER_READ_BIT);
    vk_once_end(commands);
    return true;
}

void vk_host_image_write(vk_image& image, const uint8_t* rgba)
{
    for (uint32_t row = 0; row < image.height; row++)
    {
        memcpy(image.mapped + row * image.row_pitch, rgba + (size_t)row * image.width * 4, (size_t)image.width * 4);
    }
}

void vk_barrier(VkCommandBuffer commands, VkImage image, VkImageAspectFlags aspect, VkImageLayout from, VkImageLayout to, VkPipelineStageFlags source_stage, VkAccessFlags source_access, VkPipelineStageFlags target_stage, VkAccessFlags target_access)
{
    VkImageMemoryBarrier barrier{};
    barrier.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    barrier.srcAccessMask = source_access;
    barrier.dstAccessMask = target_access;
    barrier.oldLayout = from;
    barrier.newLayout = to;
    barrier.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    barrier.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    barrier.image = image;
    barrier.subresourceRange = {aspect, 0, VK_REMAINING_MIP_LEVELS, 0, 1};
    vkCmdPipelineBarrier(commands, source_stage, target_stage, 0, 0, NULL, 0, NULL, 1, &barrier);
}

static VkShaderModule shader_module(const uint32_t* code, size_t size)
{
    VkShaderModuleCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO;
    info.codeSize = size;
    info.pCode = code;
    VkShaderModule module = VK_NULL_HANDLE;
    vkCreateShaderModule(device, &info, NULL, &module);
    return module;
}

static VkPipeline make_pipeline(VkShaderModule vertex, VkShaderModule fragment, const uint32_t* constants, uint32_t constant_count, VkPipelineLayout layout, VkSampleCountFlagBits samples, const VkFormat* formats, uint32_t format_count, bool surface, int blend)
{
    VkSpecializationMapEntry entries[2] = {{0, 0, 4}, {1, 4, 4}};
    VkSpecializationInfo specialization = {constant_count, entries, constant_count * 4, constants};
    VkPipelineShaderStageCreateInfo stages[2] = {};
    for (int index = 0; index < 2; index++)
    {
        stages[index].sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO;
        stages[index].stage = index == 0 ? VK_SHADER_STAGE_VERTEX_BIT : VK_SHADER_STAGE_FRAGMENT_BIT;
        stages[index].module = index == 0 ? vertex : fragment;
        stages[index].pName = "main";
        stages[index].pSpecializationInfo = &specialization;
    }
    VkPipelineVertexInputStateCreateInfo vertex_input{};
    vertex_input.sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO;
    VkPipelineInputAssemblyStateCreateInfo assembly{};
    assembly.sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO;
    assembly.topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST;
    VkPipelineViewportStateCreateInfo viewport{};
    viewport.sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO;
    viewport.viewportCount = 1;
    viewport.scissorCount = 1;
    VkPipelineRasterizationStateCreateInfo raster{};
    raster.sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO;
    raster.polygonMode = VK_POLYGON_MODE_FILL;
    raster.frontFace = VK_FRONT_FACE_COUNTER_CLOCKWISE;
    raster.lineWidth = 1.0f;
    VkPipelineMultisampleStateCreateInfo multisample{};
    multisample.sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO;
    multisample.rasterizationSamples = samples;
    VkPipelineDepthStencilStateCreateInfo depth{};
    depth.sType = VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO;
    depth.depthTestEnable = surface ? VK_TRUE : VK_FALSE;
    depth.depthCompareOp = VK_COMPARE_OP_GREATER_OR_EQUAL;
    VkPipelineColorBlendAttachmentState attachments[2] = {};
    for (VkPipelineColorBlendAttachmentState& attachment : attachments)
    {
        attachment.colorWriteMask = VK_COLOR_COMPONENT_R_BIT | VK_COLOR_COMPONENT_G_BIT | VK_COLOR_COMPONENT_B_BIT | VK_COLOR_COMPONENT_A_BIT;
    }
    if (blend != blend_none)
    {
        attachments[0].blendEnable = VK_TRUE;
        attachments[0].srcColorBlendFactor = attachments[0].srcAlphaBlendFactor = blend == blend_under ? VK_BLEND_FACTOR_ONE_MINUS_DST_ALPHA : VK_BLEND_FACTOR_ONE;
        attachments[0].dstColorBlendFactor = attachments[0].dstAlphaBlendFactor = blend == blend_under ? VK_BLEND_FACTOR_ONE : VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA;
    }
    VkPipelineColorBlendStateCreateInfo blending{};
    blending.sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO;
    blending.attachmentCount = format_count;
    blending.pAttachments = attachments;
    VkDynamicState states[] = {VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR, VK_DYNAMIC_STATE_CULL_MODE, VK_DYNAMIC_STATE_DEPTH_WRITE_ENABLE, VK_DYNAMIC_STATE_FRONT_FACE};
    VkPipelineDynamicStateCreateInfo dynamic{};
    dynamic.sType = VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO;
    dynamic.dynamicStateCount = surface ? 5 : 2;
    dynamic.pDynamicStates = states;
    VkPipelineRenderingCreateInfo rendering{};
    rendering.sType = VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO;
    rendering.colorAttachmentCount = format_count;
    rendering.pColorAttachmentFormats = formats;
    rendering.depthAttachmentFormat = samples == kSamples ? kDepthFormat : VK_FORMAT_UNDEFINED;
    VkGraphicsPipelineCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO;
    info.pNext = &rendering;
    info.stageCount = 2;
    info.pStages = stages;
    info.pVertexInputState = &vertex_input;
    info.pInputAssemblyState = &assembly;
    info.pViewportState = &viewport;
    info.pRasterizationState = &raster;
    info.pMultisampleState = &multisample;
    info.pDepthStencilState = &depth;
    info.pColorBlendState = &blending;
    info.pDynamicState = &dynamic;
    info.layout = layout;
    VkPipeline pipeline = VK_NULL_HANDLE;
    if (vkCreateGraphicsPipelines(device, VK_NULL_HANDLE, 1, &info, NULL, &pipeline) != VK_SUCCESS)
    {
        fprintf(stderr, "engine: pipeline failed\n");
        return VK_NULL_HANDLE;
    }
    return pipeline;
}

static VkDescriptorSetLayout make_set_layout(VkDescriptorType type, uint32_t count, bool per_binding, VkShaderStageFlags stages)
{
    std::vector<VkDescriptorSetLayoutBinding> bindings;
    for (uint32_t index = 0; index < (per_binding ? count : 1u); index++)
    {
        bindings.push_back({index, type, per_binding ? 1u : count, stages, NULL});
    }
    VkDescriptorSetLayoutCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO;
    info.bindingCount = (uint32_t)bindings.size();
    info.pBindings = bindings.data();
    VkDescriptorSetLayout layout = VK_NULL_HANDLE;
    vkCreateDescriptorSetLayout(device, &info, NULL, &layout);
    return layout;
}

static VkPipelineLayout make_layout(const VkDescriptorSetLayout* sets, uint32_t set_count, VkShaderStageFlags push_stage, uint32_t push_size)
{
    VkPushConstantRange push = {push_stage, 0, push_size};
    VkPipelineLayoutCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO;
    info.setLayoutCount = set_count;
    info.pSetLayouts = sets;
    info.pushConstantRangeCount = 1;
    info.pPushConstantRanges = &push;
    VkPipelineLayout layout = VK_NULL_HANDLE;
    vkCreatePipelineLayout(device, &info, NULL, &layout);
    return layout;
}

bool vk_renderer_start()
{
    if (pipeline_layout != VK_NULL_HANDLE)
    {
        return true;
    }
    VkPhysicalDeviceProperties properties;
    vkGetPhysicalDeviceProperties(physical_device, &properties);
    uniform_alignment = std::max(properties.limits.minUniformBufferOffsetAlignment, (VkDeviceSize)16);
    VkShaderStageFlags both = VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT;
    set_layouts[0] = make_set_layout(VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC, 2, true, both);
    set_layouts[1] = make_set_layout(VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, kSlotCount, false, both);
    set_layouts[2] = make_set_layout(VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, kBufferSlots, true, VK_SHADER_STAGE_VERTEX_BIT);
    screen_set_layout = make_set_layout(VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 2, true, VK_SHADER_STAGE_FRAGMENT_BIT);
    pipeline_layout = make_layout(set_layouts, 3, VK_SHADER_STAGE_VERTEX_BIT, sizeof(draw_uniforms));
    screen_layout = make_layout(&screen_set_layout, 1, VK_SHADER_STAGE_FRAGMENT_BIT, 16);
    VkShaderModule modules[5] = {shader_module(surface_vertex_spv, sizeof(surface_vertex_spv)), shader_module(surface_fragment_spv, sizeof(surface_fragment_spv)), shader_module(screen_vertex_spv, sizeof(screen_vertex_spv)), shader_module(under_fragment_spv, sizeof(under_fragment_spv)), shader_module(share_fragment_spv, sizeof(share_fragment_spv))};
    bool ok = pipeline_layout != VK_NULL_HANDLE && screen_layout != VK_NULL_HANDLE;
    for (VkShaderModule module : modules)
    {
        ok = ok && module != VK_NULL_HANDLE;
    }
    for (int kind = 0; ok && kind < 3; kind++)
    {
        for (int outline = 0; ok && outline < 2; outline++)
        {
            for (int blend = 0; ok && blend < 2; blend++)
            {
                if (outline == 1 && kind != HV_MATERIAL_MTOON)
                {
                    continue;
                }
                uint32_t constants[2] = {(uint32_t)kind, (uint32_t)outline};
                surface_pipelines[kind][outline][blend] = make_pipeline(modules[0], modules[1], constants, 2, pipeline_layout, kSamples, &kColorFormat, 1, true, blend == 1 ? blend_over : blend_none);
                ok = surface_pipelines[kind][outline][blend] != VK_NULL_HANDLE;
            }
        }
    }
    VkFormat share_formats[2] = {kColorFormat, kColorFormat};
    under_pipeline = ok ? make_pipeline(modules[2], modules[3], NULL, 0, screen_layout, kSamples, &kColorFormat, 1, false, blend_under) : VK_NULL_HANDLE;
    share_pipeline = ok ? make_pipeline(modules[2], modules[4], NULL, 0, screen_layout, VK_SAMPLE_COUNT_1_BIT, share_formats, 2, false, blend_none) : VK_NULL_HANDLE;
    for (VkShaderModule module : modules)
    {
        vkDestroyShaderModule(device, module, NULL);
    }
    if (under_pipeline == VK_NULL_HANDLE || share_pipeline == VK_NULL_HANDLE || !vk_image_create(blank_texture, 1, 1, 1, VK_FORMAT_R8G8B8A8_UNORM, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT, VK_IMAGE_ASPECT_COLOR_BIT) || !vk_buffer_create(blank_buffer, 64, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, true))
    {
        return false;
    }
    VkCommandBuffer commands = vk_once_begin();
    vk_barrier(commands, blank_texture.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, 0, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_ACCESS_TRANSFER_WRITE_BIT);
    VkClearColorValue white = {{1.0f, 1.0f, 1.0f, 1.0f}};
    VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    vkCmdClearColorImage(commands, blank_texture.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &white, 1, &range);
    vk_barrier(commands, blank_texture.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_ACCESS_TRANSFER_WRITE_BIT, VK_PIPELINE_STAGE_VERTEX_SHADER_BIT | VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_ACCESS_SHADER_READ_BIT);
    vk_once_end(commands);
    VkSamplerCreateInfo sampler{};
    sampler.sType = VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO;
    sampler.magFilter = VK_FILTER_LINEAR;
    sampler.minFilter = VK_FILTER_LINEAR;
    sampler.addressModeU = sampler.addressModeV = sampler.addressModeW = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
    VkDescriptorPoolSize pool_size = {VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 4};
    VkDescriptorPoolCreateInfo pool_info{};
    pool_info.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO;
    pool_info.maxSets = 2;
    pool_info.poolSizeCount = 1;
    pool_info.pPoolSizes = &pool_size;
    VkDescriptorSetLayout screen_layouts[2] = {screen_set_layout, screen_set_layout};
    VkDescriptorSetAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO;
    allocate.descriptorSetCount = 2;
    allocate.pSetLayouts = screen_layouts;
    if (vkCreateSampler(device, &sampler, NULL, &default_sampler) != VK_SUCCESS || vkCreateDescriptorPool(device, &pool_info, NULL, &screen_pool) != VK_SUCCESS)
    {
        return false;
    }
    allocate.descriptorPool = screen_pool;
    return vkAllocateDescriptorSets(device, &allocate, screen_sets) == VK_SUCCESS;
}

static bool build_material(vk_renderer* r, material_gpu& out, const hv_material* material)
{
    if (!bind_material(r, out, material, blank_texture.view, default_sampler))
    {
        return true;
    }
    VkDescriptorImageInfo images[kSlotCount];
    for (int slot = 0; slot < kSlotCount; slot++)
    {
        images[slot] = {out.samplers[slot], out.textures[slot], VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    }
    if (out.set != VK_NULL_HANDLE)
    {
        vkDeviceWaitIdle(device);
    }
    else
    {
        VkDescriptorSetAllocateInfo allocate{};
        allocate.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO;
        allocate.descriptorPool = r->pool;
        allocate.descriptorSetCount = 1;
        allocate.pSetLayouts = &set_layouts[1];
        if (vkAllocateDescriptorSets(device, &allocate, &out.set) != VK_SUCCESS)
        {
            return false;
        }
    }
    VkWriteDescriptorSet write{};
    write.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET;
    write.dstSet = out.set;
    write.descriptorCount = kSlotCount;
    write.descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
    write.pImageInfo = images;
    vkUpdateDescriptorSets(device, 1, &write, 0, NULL);
    return true;
}

struct uploader
{
    VkCommandBuffer commands = VK_NULL_HANDLE;
    std::vector<vk_buffer> staging;
};

static vk_buffer make_buffer(uploader& upload, const void* bytes, size_t length)
{
    vk_buffer out;
    vk_buffer staging;
    if (bytes == NULL || length == 0 || !vk_buffer_create(staging, length, VK_BUFFER_USAGE_TRANSFER_SRC_BIT, true))
    {
        return out;
    }
    memcpy(staging.mapped, bytes, length);
    upload.staging.push_back(staging);
    if (!vk_buffer_create(out, length, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_INDEX_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT, false))
    {
        return vk_buffer();
    }
    VkBufferCopy region = {0, 0, length};
    vkCmdCopyBuffer(upload.commands, staging.buffer, out.buffer, 1, &region);
    return out;
}

static vk_buffer shared_buffer(std::vector<vk_buffer>& statics, size_t& next, uploader& upload, const void* bytes, size_t length)
{
    if (next == statics.size())
    {
        statics.push_back(make_buffer(upload, bytes, length));
    }
    return statics[next++];
}

void vk_renderer_destroy(vk_renderer* r, hv_model* model)
{
    if (r != NULL)
    {
        vkDeviceWaitIdle(device);
        for (primitive_gpu& gpu : r->primitives)
        {
            vk_buffer_destroy(gpu.buffers[8]);
        }
        for (vk_buffer& buffer : r->skins)
        {
            vk_buffer_destroy(buffer);
        }
        vk_buffer_destroy(r->ring);
        vk_image_destroy(r->msaa);
        vk_image_destroy(r->depth);
        vkDestroyDescriptorPool(device, r->pool, NULL);
        delete r;
    }
    hv_free(model);
}

vk_renderer* vk_renderer_create(hv_model* model, const std::vector<VkImageView>& srgb_textures, const std::vector<VkImageView>& linear_textures, const std::vector<VkSampler>& samplers, std::vector<vk_buffer>& statics)
{
    vk_renderer* r = new vk_renderer();
    bool reuse = !statics.empty();
    size_t next = 0;
    r->model = model;
    r->srgb_textures = srgb_textures;
    r->linear_textures = linear_textures;
    r->samplers = samplers;
    int32_t material_count = hv_material_count(model);
    int32_t primitive_count = hv_primitive_count(model);
    VkDescriptorPoolSize sizes[3] = {
        {VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC, 2},
        {VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, (uint32_t)(material_count + 1) * kSlotCount},
        {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, (uint32_t)std::max(primitive_count, 1) * kBufferSlots},
    };
    VkDescriptorPoolCreateInfo pool_info{};
    pool_info.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO;
    pool_info.maxSets = (uint32_t)(material_count + primitive_count + 2);
    pool_info.poolSizeCount = 3;
    pool_info.pPoolSizes = sizes;
    r->material_stride = (sizeof(material_uniforms) + uniform_alignment - 1) / uniform_alignment * uniform_alignment;
    VkDeviceSize ring_size = r->material_stride * (VkDeviceSize)(primitive_count * 2 + 1);
    if (vkCreateDescriptorPool(device, &pool_info, NULL, &r->pool) != VK_SUCCESS || !vk_buffer_create(r->ring, ring_size, VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT, true))
    {
        vk_renderer_destroy(r, NULL);
        return NULL;
    }
    VkDescriptorSetAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO;
    allocate.descriptorPool = r->pool;
    allocate.descriptorSetCount = 1;
    allocate.pSetLayouts = &set_layouts[0];
    if (vkAllocateDescriptorSets(device, &allocate, &r->uniform_set) != VK_SUCCESS)
    {
        vk_renderer_destroy(r, NULL);
        return NULL;
    }
    VkDescriptorBufferInfo uniform_infos[2] = {{r->ring.buffer, 0, sizeof(frame_uniforms)}, {r->ring.buffer, 0, sizeof(material_uniforms)}};
    VkWriteDescriptorSet uniform_writes[2] = {};
    for (uint32_t index = 0; index < 2; index++)
    {
        uniform_writes[index].sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET;
        uniform_writes[index].dstSet = r->uniform_set;
        uniform_writes[index].dstBinding = index;
        uniform_writes[index].descriptorCount = 1;
        uniform_writes[index].descriptorType = VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC;
        uniform_writes[index].pBufferInfo = &uniform_infos[index];
    }
    vkUpdateDescriptorSets(device, 2, uniform_writes, 0, NULL);
    r->materials.resize((size_t)material_count + 1);
    bool built = true;
    for (int32_t index = 0; index < material_count; index++)
    {
        built = build_material(r, r->materials[index], hv_material_at(model, index)) && built;
    }
    hv_material fallback = fallback_material();
    built = build_material(r, r->materials[material_count], &fallback) && built;
    if (!built)
    {
        vk_renderer_destroy(r, NULL);
        return NULL;
    }
    for (int32_t skin = 0; skin < hv_skin_count(model); skin++)
    {
        vk_buffer buffer;
        if (!vk_buffer_create(buffer, std::max((size_t)hv_skin_joint_count(model, skin), (size_t)1) * 64, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, true))
        {
            vk_renderer_destroy(r, NULL);
            return NULL;
        }
        r->skins.push_back(buffer);
        r->skin_memory.push_back(buffer.mapped);
    }
    uploader upload;
    upload.commands = reuse ? VK_NULL_HANDLE : vk_once_begin();
    r->primitives.resize((size_t)primitive_count);
    allocate.pSetLayouts = &set_layouts[2];
    for (int32_t index = 0; index < primitive_count; index++)
    {
        const hv_primitive* source = hv_primitive_at(model, index);
        primitive_gpu& gpu = r->primitives[index];
        size_t vertices = source->vertex_count;
        gpu.source = source;
        gpu.material = source->material >= 0 && source->material < material_count ? source->material : material_count;
        gpu.buffers[0] = shared_buffer(statics, next, upload, source->positions, vertices * 12);
        gpu.buffers[1] = shared_buffer(statics, next, upload, source->normals, vertices * 12);
        gpu.buffers[2] = shared_buffer(statics, next, upload, source->uvs, vertices * 8);
        bool skinned = source->skin >= 0 && source->skin < hv_skin_count(model) && source->joints != NULL && source->weights != NULL;
        if (skinned)
        {
            gpu.buffers[3] = shared_buffer(statics, next, upload, source->joints, vertices * 8);
            gpu.buffers[4] = shared_buffer(statics, next, upload, source->weights, vertices * 16);
        }
        gpu.buffers[5] = shared_buffer(statics, next, upload, source->indices, (size_t)source->index_count * 4);
        if (source->target_count > 0 && source->target_positions != NULL)
        {
            gpu.buffers[6] = shared_buffer(statics, next, upload, source->target_positions, (size_t)source->target_count * vertices * 12);
            gpu.buffers[7] = shared_buffer(statics, next, upload, source->target_normals, (size_t)source->target_count * vertices * 12);
            vk_buffer_create(gpu.buffers[8], source->target_count * sizeof(active_target), VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, true);
            gpu.mapped_targets = (active_target*)gpu.buffers[8].mapped;
        }
        gpu.flags = (skinned ? draw_skinned : 0) | (gpu.buffers[1].buffer != VK_NULL_HANDLE ? draw_normals : 0) | (gpu.buffers[2].buffer != VK_NULL_HANDLE ? draw_uvs : 0) | (gpu.buffers[7].buffer != VK_NULL_HANDLE ? draw_target_normals : 0);
        if (gpu.buffers[0].buffer == VK_NULL_HANDLE || gpu.buffers[5].buffer == VK_NULL_HANDLE || (skinned && (gpu.buffers[3].buffer == VK_NULL_HANDLE || gpu.buffers[4].buffer == VK_NULL_HANDLE)) || vkAllocateDescriptorSets(device, &allocate, &gpu.set) != VK_SUCCESS)
        {
            gpu.source = NULL;
            continue;
        }
        VkDescriptorBufferInfo infos[kBufferSlots];
        VkWriteDescriptorSet writes[kBufferSlots] = {};
        for (int slot = 0; slot < kBufferSlots; slot++)
        {
            VkBuffer buffer = slot == 5 ? (skinned ? r->skins[source->skin].buffer : VK_NULL_HANDLE) : gpu.buffers[slot].buffer;
            infos[slot] = {buffer != VK_NULL_HANDLE ? buffer : blank_buffer.buffer, 0, VK_WHOLE_SIZE};
            writes[slot].sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET;
            writes[slot].dstSet = gpu.set;
            writes[slot].dstBinding = (uint32_t)slot;
            writes[slot].descriptorCount = 1;
            writes[slot].descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;
            writes[slot].pBufferInfo = &infos[slot];
        }
        vkUpdateDescriptorSets(device, kBufferSlots, writes, 0, NULL);
        place_primitive(model, gpu);
    }
    if (!reuse)
    {
        vk_once_end(upload.commands);
    }
    for (vk_buffer& staging : upload.staging)
    {
        vk_buffer_destroy(staging);
    }
    draw_orders(r->materials, r->primitives, r->solid_order, r->blend_order);
    return r;
}

static void encode_primitive(vk_renderer* r, VkCommandBuffer commands, int index, bool outline, float seconds, uint32_t& draw_index)
{
    const primitive_gpu& gpu = r->primitives[index];
    const material_gpu& material = r->materials[gpu.material];
    draw_uniforms draw = draw_for(gpu);
    VkDeviceSize offset = r->material_stride * ++draw_index;
    animated_uniforms(*(material_uniforms*)((uint8_t*)r->ring.mapped + offset), material, seconds);
    vkCmdBindPipeline(commands, VK_PIPELINE_BIND_POINT_GRAPHICS, surface_pipelines[material.kind][outline ? 1 : 0][material.alpha_mode == HV_ALPHA_BLEND ? 1 : 0]);
    vkCmdSetDepthWriteEnable(commands, material.depth_write ? VK_TRUE : VK_FALSE);
    vkCmdSetCullMode(commands, outline ? VK_CULL_MODE_FRONT_BIT : (material.double_sided ? VK_CULL_MODE_NONE : VK_CULL_MODE_BACK_BIT));
    vkCmdSetFrontFace(commands, draw_mirrored(gpu) ? VK_FRONT_FACE_CLOCKWISE : VK_FRONT_FACE_COUNTER_CLOCKWISE);
    vkCmdPushConstants(commands, pipeline_layout, VK_SHADER_STAGE_VERTEX_BIT, 0, sizeof(draw), &draw);
    uint32_t offsets[2] = {0, (uint32_t)offset};
    VkDescriptorSet sets[3] = {r->uniform_set, material.set, gpu.set};
    vkCmdBindDescriptorSets(commands, VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline_layout, 0, 3, sets, 2, offsets);
    vkCmdBindIndexBuffer(commands, gpu.buffers[5].buffer, 0, VK_INDEX_TYPE_UINT32);
    vkCmdDrawIndexed(commands, gpu.source->index_count, 1, 0, 0, 0);
}

static void draw_primitive(vk_renderer* r, VkCommandBuffer commands, int index, float seconds, uint32_t& draw_index)
{
    encode_primitive(r, commands, index, false, seconds, draw_index);
    if (r->materials[r->primitives[index].material].outline)
    {
        encode_primitive(r, commands, index, true, seconds, draw_index);
    }
}

static bool size_targets(vk_renderer* r, int width, int height)
{
    if (r->depth.image != VK_NULL_HANDLE && r->msaa.image != VK_NULL_HANDLE && (int)r->depth.width == width && (int)r->depth.height == height)
    {
        return true;
    }
    vkDeviceWaitIdle(device);
    vk_image_destroy(r->msaa);
    vk_image_destroy(r->depth);
    return vk_image_create(r->depth, width, height, 1, kDepthFormat, kSamples, VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT, VK_IMAGE_ASPECT_DEPTH_BIT) &&
           vk_image_create(r->msaa, width, height, 1, kColorFormat, kSamples, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT, VK_IMAGE_ASPECT_COLOR_BIT);
}

static void bind_screen(VkCommandBuffer commands, int which, VkImageView model, VkImageView back, int width, int height)
{
    VkImageView views[2] = {model != VK_NULL_HANDLE ? model : blank_texture.view, back != VK_NULL_HANDLE ? back : blank_texture.view};
    if (screen_bound[which][0] != views[0] || screen_bound[which][1] != views[1])
    {
        VkDescriptorImageInfo images[2] = {{default_sampler, views[0], VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL}, {default_sampler, views[1], back != VK_NULL_HANDLE ? VK_IMAGE_LAYOUT_GENERAL : VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL}};
        VkWriteDescriptorSet write{};
        write.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET;
        write.dstSet = screen_sets[which];
        write.descriptorCount = 2;
        write.descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
        write.pImageInfo = images;
        vkUpdateDescriptorSets(device, 1, &write, 0, NULL);
        screen_bound[which][0] = views[0];
        screen_bound[which][1] = views[1];
    }
    float size[4] = {(float)width, (float)height, back != VK_NULL_HANDLE ? 1.0f : 0.0f, 0.0f};
    vkCmdBindDescriptorSets(commands, VK_PIPELINE_BIND_POINT_GRAPHICS, screen_layout, 0, 1, &screen_sets[which], 0, NULL);
    vkCmdPushConstants(commands, screen_layout, VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(size), size);
}

void vk_renderer_draw(vk_renderer* r, VkCommandBuffer commands, VkImageView target, int width, int height, const float* view_projection, const float* eye, const float* light_direction, const float* light_color, const float* ambient, double seconds, uint32_t changed, VkImageView back, bool opaque)
{
    if (upload_changes(r, changed))
    {
        for (int32_t index = 0; index < hv_material_count(r->model); index++)
        {
            build_material(r, r->materials[index], hv_material_at(r->model, index));
        }
    }
    sort_blend(r->model, r->primitives, r->blend_order, eye);
    if (!size_targets(r, width, height))
    {
        return;
    }
    *(frame_uniforms*)r->ring.mapped = frame_for(view_projection, eye, light_direction, light_color, ambient, width, height);
    VkPipelineStageFlags attachment_stages = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT | VK_PIPELINE_STAGE_EARLY_FRAGMENT_TESTS_BIT | VK_PIPELINE_STAGE_LATE_FRAGMENT_TESTS_BIT;
    vk_barrier(commands, r->msaa.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, attachment_stages, 0, attachment_stages, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT);
    vk_barrier(commands, r->depth.image, VK_IMAGE_ASPECT_DEPTH_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL, attachment_stages, 0, attachment_stages, VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT | VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_READ_BIT);
    VkRenderingAttachmentInfo color{};
    color.sType = VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO;
    color.imageView = r->msaa.view;
    color.imageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    color.resolveMode = VK_RESOLVE_MODE_AVERAGE_BIT;
    color.resolveImageView = target;
    color.resolveImageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    color.loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR;
    color.storeOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    VkRenderingAttachmentInfo depth{};
    depth.sType = VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO;
    depth.imageView = r->depth.view;
    depth.imageLayout = VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL;
    depth.loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR;
    depth.storeOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    VkRenderingInfo rendering{};
    rendering.sType = VK_STRUCTURE_TYPE_RENDERING_INFO;
    rendering.renderArea = {{0, 0}, {(uint32_t)width, (uint32_t)height}};
    rendering.layerCount = 1;
    rendering.colorAttachmentCount = 1;
    rendering.pColorAttachments = &color;
    rendering.pDepthAttachment = &depth;
    vkCmdBeginRendering(commands, &rendering);
    VkViewport viewport = {0, (float)height, (float)width, -(float)height, 0, 1};
    VkRect2D scissor = {{0, 0}, {(uint32_t)width, (uint32_t)height}};
    vkCmdSetViewport(commands, 0, 1, &viewport);
    vkCmdSetScissor(commands, 0, 1, &scissor);
    float clock = (float)seconds;
    uint32_t draw_index = 0;
    for (int index : r->solid_order)
    {
        draw_primitive(r, commands, index, clock, draw_index);
    }
    for (const blend_entry& entry : r->blend_order)
    {
        draw_primitive(r, commands, entry.primitive, clock, draw_index);
    }
    if (opaque)
    {
        VkViewport screen = {0, 0, (float)width, (float)height, 0, 1};
        vkCmdSetViewport(commands, 0, 1, &screen);
        vkCmdBindPipeline(commands, VK_PIPELINE_BIND_POINT_GRAPHICS, under_pipeline);
        bind_screen(commands, 0, VK_NULL_HANDLE, back, width, height);
        vkCmdDraw(commands, 3, 1, 0, 0);
    }
    vkCmdEndRendering(commands);
}

void vk_renderer_share(VkCommandBuffer commands, VkImageView model, VkImageView back, VkImageView window_target, VkImageView share_target, int width, int height)
{
    VkRenderingAttachmentInfo attachments[2] = {};
    VkImageView views[2] = {window_target, share_target};
    for (int index = 0; index < 2; index++)
    {
        attachments[index].sType = VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO;
        attachments[index].imageView = views[index];
        attachments[index].imageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
        attachments[index].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
        attachments[index].storeOp = VK_ATTACHMENT_STORE_OP_STORE;
    }
    VkRenderingInfo rendering{};
    rendering.sType = VK_STRUCTURE_TYPE_RENDERING_INFO;
    rendering.renderArea = {{0, 0}, {(uint32_t)width, (uint32_t)height}};
    rendering.layerCount = 1;
    rendering.colorAttachmentCount = 2;
    rendering.pColorAttachments = attachments;
    vkCmdBeginRendering(commands, &rendering);
    VkViewport viewport = {0, 0, (float)width, (float)height, 0, 1};
    VkRect2D scissor = {{0, 0}, {(uint32_t)width, (uint32_t)height}};
    vkCmdSetViewport(commands, 0, 1, &viewport);
    vkCmdSetScissor(commands, 0, 1, &scissor);
    vkCmdBindPipeline(commands, VK_PIPELINE_BIND_POINT_GRAPHICS, share_pipeline);
    bind_screen(commands, 1, model, back, width, height);
    vkCmdDraw(commands, 3, 1, 0, 0);
    vkCmdEndRendering(commands);
}
