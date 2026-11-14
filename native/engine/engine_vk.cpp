#define _USE_MATH_DEFINES
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#define VK_USE_PLATFORM_WIN32_KHR
#include <windows.h>
#include <d3d11.h>
#include <dxgi.h>
#endif
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iterator>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
#include "stb/stb_image.h"
#include "engine.h"
#include "window_vk.h"
#include "renderer_vk.h"

struct engine_gpu
{
    vk_renderer* own = NULL;
    VkQueryPool timer = VK_NULL_HANDLE;
    VkCommandPool frame_pool = VK_NULL_HANDLE;
    VkCommandBuffer frame_commands = VK_NULL_HANDLE;
    VkFence frame_fence = VK_NULL_HANDLE;
};

struct target_gpu
{
    vk_image color;
    vk_buffer readback;
    ~target_gpu()
    {
        vk_image_destroy(color);
        vk_buffer_destroy(readback);
    }
};

struct model_set
{
    std::vector<vk_image> images;
    std::vector<VkImageView> srgb;
    std::vector<VkImageView> linear;
    std::vector<VkSampler> samplers;
    std::vector<vk_buffer> statics;
    int users = 0;
    ~model_set()
    {
        for (vk_image& image : images)
        {
            vk_image_destroy(image);
        }
        for (VkSampler sampler : samplers)
        {
            vkDestroySampler(device, sampler, NULL);
        }
        for (vk_buffer& buffer : statics)
        {
            vk_buffer_destroy(buffer);
        }
    }
};

#include "engine_common.inc"

VkPhysicalDevice physical_device = VK_NULL_HANDLE;
VkDevice device = VK_NULL_HANDLE;
static VkInstance instance = VK_NULL_HANDLE;
static VkQueue queue = VK_NULL_HANDLE;
static VkCommandPool command_pool = VK_NULL_HANDLE;
static float timestamp_period = 1.0f;
static bool anisotropy = false;
static bool can_present = false;
#ifdef _WIN32
static bool can_share = false;
#endif
static std::mutex queue_lock;

static int graphics_family(VkPhysicalDevice candidate)
{
    uint32_t count = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(candidate, &count, NULL);
    std::vector<VkQueueFamilyProperties> families(count);
    vkGetPhysicalDeviceQueueFamilyProperties(candidate, &count, families.data());
    for (uint32_t i = 0; i < count; i++)
    {
        if (families[i].queueFlags & VK_QUEUE_GRAPHICS_BIT)
        {
            return (int)i;
        }
    }
    return -1;
}

static bool has_extension(VkPhysicalDevice candidate, const char* name)
{
    uint32_t count = 0;
    vkEnumerateDeviceExtensionProperties(candidate, NULL, &count, NULL);
    std::vector<VkExtensionProperties> extensions(count);
    vkEnumerateDeviceExtensionProperties(candidate, NULL, &count, extensions.data());
    for (const VkExtensionProperties& extension : extensions)
    {
        if (strcmp(extension.extensionName, name) == 0)
        {
            return true;
        }
    }
    return false;
}

static void wait_idle()
{
    std::lock_guard<std::mutex> guard(queue_lock);
    vkDeviceWaitIdle(device);
}

static void submit(VkCommandBuffer commands, VkSemaphore wait, VkPipelineStageFlags wait_stage, VkSemaphore signal, VkFence fence)
{
    VkSubmitInfo info{};
    info.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
    info.waitSemaphoreCount = wait != VK_NULL_HANDLE ? 1 : 0;
    info.pWaitSemaphores = &wait;
    info.pWaitDstStageMask = &wait_stage;
    info.commandBufferCount = 1;
    info.pCommandBuffers = &commands;
    info.signalSemaphoreCount = signal != VK_NULL_HANDLE ? 1 : 0;
    info.pSignalSemaphores = &signal;
    std::lock_guard<std::mutex> guard(queue_lock);
    vkQueueSubmit(queue, 1, &info, fence);
}

struct once_pool_owner
{
    VkCommandPool pool = VK_NULL_HANDLE;
    ~once_pool_owner()
    {
        if (pool != VK_NULL_HANDLE)
        {
            vkDestroyCommandPool(device, pool, NULL);
        }
    }
};

static thread_local once_pool_owner once_pool;

VkCommandBuffer vk_once_begin()
{
    if (once_pool.pool == VK_NULL_HANDLE)
    {
        VkCommandPoolCreateInfo pool_info{};
        pool_info.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
        pool_info.flags = VK_COMMAND_POOL_CREATE_TRANSIENT_BIT;
        pool_info.queueFamilyIndex = (uint32_t)graphics_family(physical_device);
        vkCreateCommandPool(device, &pool_info, NULL, &once_pool.pool);
    }
    VkCommandBufferAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
    allocate.commandPool = once_pool.pool;
    allocate.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    allocate.commandBufferCount = 1;
    VkCommandBuffer commands = VK_NULL_HANDLE;
    vkAllocateCommandBuffers(device, &allocate, &commands);
    VkCommandBufferBeginInfo begin{};
    begin.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(commands, &begin);
    return commands;
}

void vk_once_end(VkCommandBuffer commands)
{
    vkEndCommandBuffer(commands);
    VkFenceCreateInfo fence_info{};
    fence_info.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
    VkFence fence = VK_NULL_HANDLE;
    vkCreateFence(device, &fence_info, NULL, &fence);
    submit(commands, VK_NULL_HANDLE, 0, VK_NULL_HANDLE, fence);
    vkWaitForFences(device, 1, &fence, VK_TRUE, UINT64_MAX);
    vkDestroyFence(device, fence, NULL);
    vkFreeCommandBuffers(device, once_pool.pool, 1, &commands);
}

static bool start_vulkan()
{
    VkApplicationInfo app{};
    app.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO;
    app.pApplicationName = "hikari-vrm";
    app.apiVersion = VK_API_VERSION_1_3;
    uint32_t available_count = 0;
    vkEnumerateInstanceExtensionProperties(NULL, &available_count, NULL);
    std::vector<VkExtensionProperties> available(available_count);
    vkEnumerateInstanceExtensionProperties(NULL, &available_count, available.data());
    std::vector<const char*> instance_extensions;
    for (const char* name : {VK_KHR_SURFACE_EXTENSION_NAME, window_surface_extension()})
    {
        for (const VkExtensionProperties& extension : available)
        {
            if (strcmp(extension.extensionName, name) == 0)
            {
                instance_extensions.push_back(name);
            }
        }
    }
    VkInstanceCreateInfo instance_info{};
    instance_info.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO;
    instance_info.pApplicationInfo = &app;
    instance_info.enabledExtensionCount = (uint32_t)instance_extensions.size();
    instance_info.ppEnabledExtensionNames = instance_extensions.data();
    if (vkCreateInstance(&instance_info, NULL, &instance) != VK_SUCCESS)
    {
        return false;
    }
    uint32_t count = 0;
    vkEnumeratePhysicalDevices(instance, &count, NULL);
    std::vector<VkPhysicalDevice> candidates(count);
    vkEnumeratePhysicalDevices(instance, &count, candidates.data());
    static const int rank[] = {0, 3, 4, 2, 1};
    int best_rank = -1;
    VkPhysicalDeviceProperties properties{};
    for (VkPhysicalDevice candidate : candidates)
    {
        VkPhysicalDeviceProperties candidate_properties;
        vkGetPhysicalDeviceProperties(candidate, &candidate_properties);
        int candidate_rank = candidate_properties.deviceType <= VK_PHYSICAL_DEVICE_TYPE_CPU ? rank[candidate_properties.deviceType] : 0;
        if (candidate_properties.apiVersion < VK_API_VERSION_1_3)
        {
            continue;
        }
        VkPhysicalDeviceVulkan13Features candidate13{};
        candidate13.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES;
        VkPhysicalDeviceFeatures2 candidate_features{};
        candidate_features.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2;
        candidate_features.pNext = &candidate13;
        vkGetPhysicalDeviceFeatures2(candidate, &candidate_features);
        if (candidate13.dynamicRendering && candidate13.shaderDemoteToHelperInvocation && graphics_family(candidate) >= 0 && candidate_rank > best_rank)
        {
            best_rank = candidate_rank;
            physical_device = candidate;
            properties = candidate_properties;
        }
    }
    if (physical_device == VK_NULL_HANDLE)
    {
        return false;
    }
    gpu_name = properties.deviceName;
    timestamp_period = properties.limits.timestampPeriod;
    float priority = 1.0f;
    uint32_t family = (uint32_t)graphics_family(physical_device);
    VkDeviceQueueCreateInfo queue_info{};
    queue_info.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO;
    queue_info.queueFamilyIndex = family;
    queue_info.queueCount = 1;
    queue_info.pQueuePriorities = &priority;
    VkPhysicalDeviceFeatures features{};
    vkGetPhysicalDeviceFeatures(physical_device, &features);
    VkPhysicalDeviceVulkan13Features enabled{};
    enabled.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES;
    enabled.dynamicRendering = VK_TRUE;
    enabled.shaderDemoteToHelperInvocation = VK_TRUE;
    VkPhysicalDeviceFeatures2 enabled_features{};
    enabled_features.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2;
    enabled_features.pNext = &enabled;
    enabled_features.features.samplerAnisotropy = features.samplerAnisotropy;
    anisotropy = features.samplerAnisotropy == VK_TRUE;
    can_present = instance_extensions.size() == 2 && has_extension(physical_device, VK_KHR_SWAPCHAIN_EXTENSION_NAME);
    std::vector<const char*> extensions;
    if (can_present)
    {
        extensions.push_back(VK_KHR_SWAPCHAIN_EXTENSION_NAME);
    }
#ifdef _WIN32
    can_share = has_extension(physical_device, VK_KHR_EXTERNAL_MEMORY_WIN32_EXTENSION_NAME);
    if (can_share)
    {
        extensions.push_back(VK_KHR_EXTERNAL_MEMORY_WIN32_EXTENSION_NAME);
    }
#endif
    VkDeviceCreateInfo device_info{};
    device_info.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO;
    device_info.pNext = &enabled_features;
    device_info.queueCreateInfoCount = 1;
    device_info.pQueueCreateInfos = &queue_info;
    device_info.enabledExtensionCount = (uint32_t)extensions.size();
    device_info.ppEnabledExtensionNames = extensions.data();
    if (vkCreateDevice(physical_device, &device_info, NULL, &device) != VK_SUCCESS)
    {
        device = VK_NULL_HANDLE;
        return false;
    }
    vkGetDeviceQueue(device, family, 0, &queue);
    VkCommandPoolCreateInfo pool_info{};
    pool_info.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
    pool_info.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    pool_info.queueFamilyIndex = family;
    vkCreateCommandPool(device, &pool_info, NULL, &command_pool);
    return vk_renderer_start();
}

static bool decode_image(const hv_texture* source, std::vector<uint8_t>& pixels, int& width, int& height)
{
    int channels = 0;
    stbi_uc* decoded = stbi_load_from_memory(source->bytes, (int)source->length, &width, &height, &channels, 4);
    if (decoded == NULL)
    {
        return false;
    }
    if (!declared_size(source, (size_t)width, (size_t)height))
    {
        stbi_image_free(decoded);
        return false;
    }
    pixels.assign(decoded, decoded + (size_t)width * height * 4);
    stbi_image_free(decoded);
    return true;
}

static uint32_t mip_levels(uint32_t width, uint32_t height)
{
    uint32_t levels = 1;
    while ((std::max(width, height) >> levels) > 0)
    {
        levels++;
    }
    return levels;
}

static bool upload_texture(const std::vector<uint8_t>& pixels, int width, int height, VkFormat format, vk_image& texture, VkCommandBuffer commands, std::vector<vk_buffer>& staging)
{
    uint32_t levels = mip_levels((uint32_t)width, (uint32_t)height);
    vk_buffer buffer;
    if (!vk_image_create(texture, (uint32_t)width, (uint32_t)height, levels, format, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT, VK_IMAGE_ASPECT_COLOR_BIT) || !vk_buffer_create(buffer, pixels.size(), VK_BUFFER_USAGE_TRANSFER_SRC_BIT, true))
    {
        vk_image_destroy(texture);
        return false;
    }
    memcpy(buffer.mapped, pixels.data(), pixels.size());
    staging.push_back(buffer);
    vk_barrier(commands, texture.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, 0, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_ACCESS_TRANSFER_WRITE_BIT);
    VkBufferImageCopy region{};
    region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.imageExtent = {(uint32_t)width, (uint32_t)height, 1};
    vkCmdCopyBufferToImage(commands, buffer.buffer, texture.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);
    int32_t level_width = width;
    int32_t level_height = height;
    for (uint32_t level = 1; level < levels; level++)
    {
        VkImageMemoryBarrier barrier{};
        barrier.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
        barrier.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
        barrier.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
        barrier.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
        barrier.newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
        barrier.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
        barrier.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
        barrier.image = texture.image;
        barrier.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, level - 1, 1, 0, 1};
        vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);
        int32_t next_width = std::max(level_width / 2, 1);
        int32_t next_height = std::max(level_height / 2, 1);
        VkImageBlit blit{};
        blit.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, level - 1, 0, 1};
        blit.srcOffsets[1] = {level_width, level_height, 1};
        blit.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, level, 0, 1};
        blit.dstOffsets[1] = {next_width, next_height, 1};
        vkCmdBlitImage(commands, texture.image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, texture.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &blit, VK_FILTER_LINEAR);
        barrier.srcAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
        barrier.dstAccessMask = VK_ACCESS_SHADER_READ_BIT;
        barrier.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
        barrier.newLayout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
        vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_VERTEX_SHADER_BIT | VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);
        level_width = next_width;
        level_height = next_height;
    }
    VkImageMemoryBarrier last{};
    last.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    last.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    last.dstAccessMask = VK_ACCESS_SHADER_READ_BIT;
    last.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
    last.newLayout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    last.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    last.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    last.image = texture.image;
    last.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, levels - 1, 1, 0, 1};
    vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_VERTEX_SHADER_BIT | VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, NULL, 0, NULL, 1, &last);
    return true;
}

static VkSamplerAddressMode address_mode(int32_t wrap)
{
    if (wrap == 33071)
    {
        return VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
    }
    return wrap == 33648 ? VK_SAMPLER_ADDRESS_MODE_MIRRORED_REPEAT : VK_SAMPLER_ADDRESS_MODE_REPEAT;
}

static VkSampler make_sampler(const hv_texture* source)
{
    int32_t min_filter = source->min_filter;
    VkSamplerCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO;
    info.magFilter = source->mag_filter == 9728 ? VK_FILTER_NEAREST : VK_FILTER_LINEAR;
    info.minFilter = min_filter == 9728 || min_filter == 9984 || min_filter == 9986 ? VK_FILTER_NEAREST : VK_FILTER_LINEAR;
    info.mipmapMode = min_filter == 9984 || min_filter == 9985 ? VK_SAMPLER_MIPMAP_MODE_NEAREST : VK_SAMPLER_MIPMAP_MODE_LINEAR;
    info.maxLod = min_filter == 9728 || min_filter == 9729 ? 0.0f : VK_LOD_CLAMP_NONE;
    info.addressModeU = address_mode(source->wrap_s);
    info.addressModeV = address_mode(source->wrap_t);
    info.addressModeW = VK_SAMPLER_ADDRESS_MODE_REPEAT;
    info.anisotropyEnable = anisotropy ? VK_TRUE : VK_FALSE;
    info.maxAnisotropy = 4.0f;
    VkSampler sampler = VK_NULL_HANDLE;
    vkCreateSampler(device, &info, NULL, &sampler);
    return sampler;
}

static void engine_release(engine* e)
{
    wait_idle();
    vk_renderer_destroy(e->own, e->model);
    vkDestroyQueryPool(device, e->timer, NULL);
    vkDestroyFence(device, e->frame_fence, NULL);
    vkDestroyCommandPool(device, e->frame_pool, NULL);
    model_release(e);
    delete e;
}

static void upload_textures(hv_model* model, model_set& set)
{
    std::vector<uint8_t> usage = renderer_texture_usage(model);
    set.srgb.assign(usage.size(), VK_NULL_HANDLE);
    set.linear.assign(usage.size(), VK_NULL_HANDLE);
    set.samplers.assign(usage.size(), VK_NULL_HANDLE);
    std::vector<vk_buffer> staging;
    VkCommandBuffer commands = vk_once_begin();
    std::vector<uint8_t> pixels;
    for (size_t index = 0; index < usage.size(); index++)
    {
        const hv_texture* source = hv_texture_at(model, (int32_t)index);
        int image_width = 0;
        int image_height = 0;
        set.samplers[index] = make_sampler(source);
        if (usage[index] == 0 || !decode_image(source, pixels, image_width, image_height))
        {
            continue;
        }
        for (int role = 0; role < 2; role++)
        {
            vk_image texture;
            if ((usage[index] & (1 << role)) != 0 && upload_texture(pixels, image_width, image_height, role == 0 ? VK_FORMAT_R8G8B8A8_SRGB : VK_FORMAT_R8G8B8A8_UNORM, texture, commands, staging))
            {
                (role == 0 ? set.srgb : set.linear)[index] = texture.view;
                set.images.push_back(texture);
            }
        }
    }
    vk_once_end(commands);
    for (vk_buffer& buffer : staging)
    {
        vk_buffer_destroy(buffer);
    }
}

engine* engine_create(const char* vrm_path, int width, int height, char* error, int error_capacity)
{
    if (vrm_path == NULL || width <= 0 || height <= 0)
    {
        write_error(error, error_capacity, "bad engine arguments");
        return NULL;
    }
    static const bool started = start_vulkan();
    if (!started)
    {
        write_error(error, error_capacity, "vulkan 1.3 unavailable");
        return NULL;
    }
    hv_model* model = load_model(vrm_path, error, error_capacity);
    if (model == NULL)
    {
        return NULL;
    }
    engine* e = new engine();
    e->model = model;
    e->width = width;
    e->height = height;
    e->model_key = model_key(vrm_path);
    {
        std::lock_guard<std::mutex> hold(model_cache_lock);
        model_set& set = model_cache[e->model_key];
        if (set.users == 0)
        {
            upload_textures(model, set);
        }
        hv_textures_release(model);
        set.users++;
        e->own = vk_renderer_create(model, set.srgb, set.linear, set.samplers, set.statics);
    }
    if (e->own == NULL)
    {
        write_error(error, error_capacity, std::filesystem::u8path(vrm_path).filename().u8string() + ": gpu buffers failed");
        engine_release(e);
        return NULL;
    }
    hv_geometry_release(model);
    VkQueryPoolCreateInfo timer_info{};
    timer_info.sType = VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO;
    timer_info.queryType = VK_QUERY_TYPE_TIMESTAMP;
    timer_info.queryCount = 2;
    vkCreateQueryPool(device, &timer_info, NULL, &e->timer);
    VkCommandPoolCreateInfo pool_info{};
    pool_info.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
    pool_info.queueFamilyIndex = (uint32_t)graphics_family(physical_device);
    vkCreateCommandPool(device, &pool_info, NULL, &e->frame_pool);
    VkCommandBufferAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
    allocate.commandPool = e->frame_pool;
    allocate.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    allocate.commandBufferCount = 1;
    vkAllocateCommandBuffers(device, &allocate, &e->frame_commands);
    VkFenceCreateInfo fence_info{};
    fence_info.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
    vkCreateFence(device, &fence_info, NULL, &e->frame_fence);
    e->uv_animated = model_uv_animated(model);
    e->pending = hv_update(model, 0.0f) | HV_CHANGED_JOINTS | HV_CHANGED_MORPHS;
    frame_camera(e);
    return e;
}

void engine_destroy(engine* e)
{
    engine_release(e);
}

static std::vector<std::vector<std::pair<int, float>>> axis_weights(int src, int dst, int span)
{
    std::vector<std::vector<std::pair<int, float>>> weights(dst);
    double offset = (dst - span) / 2.0;
    double step = (double)src / span;
    double radius = step > 1.0 ? step : 1.0;
    for (int i = 0; i < dst; i++)
    {
        double center = (i + 0.5 - offset) * step - 0.5;
        if (i + 0.5 < offset || i + 0.5 > offset + span)
        {
            continue;
        }
        float total = 0;
        for (int s = (int)std::ceil(center - radius); s <= (int)std::floor(center + radius); s++)
        {
            float w = (float)(1.0 - std::fabs(s - center) / radius);
            if (w <= 0)
            {
                continue;
            }
            weights[i].push_back({s < 0 ? 0 : (s >= src ? src - 1 : s), w});
            total += w;
        }
        for (auto& item : weights[i])
        {
            item.second /= total;
        }
    }
    return weights;
}

static std::vector<uint8_t> cover_frame(const stbi_uc* pixels, int iw, int ih, const std::vector<std::vector<std::pair<int, float>>>& columns, const std::vector<std::vector<std::pair<int, float>>>& rows)
{
    int width = (int)columns.size();
    int height = (int)rows.size();
    std::vector<float> premultiplied((size_t)iw * ih * 3);
    for (size_t p = 0; p < (size_t)iw * ih; p++)
    {
        for (int c = 0; c < 3; c++)
        {
            premultiplied[p * 3 + c] = pixels[p * 4 + c] * pixels[p * 4 + 3] / 255.0f;
        }
    }
    std::vector<float> wide((size_t)ih * width * 3);
    for (int y = 0; y < ih; y++)
    {
        for (int x = 0; x < width; x++)
        {
            for (auto& item : columns[x])
            {
                for (int c = 0; c < 3; c++)
                {
                    wide[((size_t)y * width + x) * 3 + c] += premultiplied[((size_t)y * iw + item.first) * 3 + c] * item.second;
                }
            }
        }
    }
    std::vector<uint8_t> frame((size_t)width * height * 4);
    for (int y = 0; y < height; y++)
    {
        for (int x = 0; x < width; x++)
        {
            float sum[3] = {0, 0, 0};
            for (auto& item : rows[y])
            {
                for (int c = 0; c < 3; c++)
                {
                    sum[c] += wide[((size_t)item.first * width + x) * 3 + c] * item.second;
                }
            }
            for (int c = 0; c < 3; c++)
            {
                frame[((size_t)y * width + x) * 4 + c] = (uint8_t)std::fmin(255.0f, std::fmax(0.0f, sum[c] + 0.5f));
            }
            frame[((size_t)y * width + x) * 4 + 3] = 255;
        }
    }
    return frame;
}

static uint32_t read_be(const uint8_t* bytes, int count)
{
    uint32_t value = 0;
    for (int i = 0; i < count; i++)
    {
        value = value << 8 | bytes[i];
    }
    return value;
}

static void append_chunk(std::string& png, const char* type, const uint8_t* data, size_t size)
{
    for (int shift = 24; shift >= 0; shift -= 8)
    {
        png.push_back((char)(size >> shift));
    }
    png.append(type, 4);
    png.append((const char*)data, size);
    png.append(4, '\0');
}

struct apng_frame
{
    uint32_t x = 0;
    uint32_t y = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    float ms = 0;
    uint8_t dispose = 0;
    uint8_t blend = 0;
    std::string data;
};

static uint64_t gif_frame_count(const uint8_t* bytes, size_t length)
{
    if (length < 13)
    {
        return 0;
    }
    size_t at = 13 + ((bytes[10] & 0x80) != 0 ? 3u << ((bytes[10] & 7) + 1) : 0);
    uint64_t frames = 0;
    while (at < length && bytes[at] != 0x3B)
    {
        if (bytes[at] == 0x2C)
        {
            if (at + 10 > length)
            {
                return 0;
            }
            frames++;
            at += 10 + ((bytes[at + 9] & 0x80) != 0 ? 3u << ((bytes[at + 9] & 7) + 1) : 0) + 1;
        }
        else if (bytes[at] == 0x21)
        {
            at += 2;
        }
        else
        {
            return 0;
        }
        while (at < length && bytes[at] != 0)
        {
            at += bytes[at] + 1;
        }
        at++;
    }
    return frames;
}

static bool decode_apng(const uint8_t* bytes, size_t length, int width, int height, int& iw, int& ih, std::vector<uint8_t>& canvases, std::vector<float>& delays)
{
    const uint8_t* header = NULL;
    std::string palette;
    std::vector<apng_frame> frames;
    bool animated = false;
    bool default_frame = false;
    uint32_t declared_frames = 0;
    uint32_t sequence = 0;
    for (size_t at = 8; at + 12 <= length;)
    {
        size_t size = read_be(bytes + at, 4);
        if (size > length - at - 12)
        {
            return false;
        }
        std::string type((const char*)bytes + at + 4, 4);
        const uint8_t* data = bytes + at + 8;
        if ((type == "fcTL" || type == "fdAT") && (size < 4 || read_be(data, 4) != sequence++))
        {
            return false;
        }
        if (type == "IHDR" && size == 13)
        {
            header = data;
        }
        else if (type == "PLTE" || type == "tRNS")
        {
            palette.append((const char*)bytes + at, size + 12);
        }
        else if (type == "acTL")
        {
            if (size != 8 || animated)
            {
                return false;
            }
            animated = true;
            declared_frames = read_be(data, 4);
        }
        else if (type == "fcTL")
        {
            if (size != 26 || data[24] > 2 || data[25] > 1)
            {
                return false;
            }
            apng_frame frame;
            frame.width = read_be(data + 4, 4);
            frame.height = read_be(data + 8, 4);
            frame.x = read_be(data + 12, 4);
            frame.y = read_be(data + 16, 4);
            uint32_t denominator = read_be(data + 22, 2);
            frame.ms = read_be(data + 20, 2) * 1000.0f / (denominator == 0 ? 100 : denominator);
            frame.dispose = data[24];
            frame.blend = data[25];
            frames.push_back(frame);
        }
        else if (type == "IDAT" && !frames.empty())
        {
            default_frame = true;
            frames.back().data.append((const char*)data, size);
        }
        else if (type == "fdAT" && !frames.empty())
        {
            frames.back().data.append((const char*)data + 4, size - 4);
        }
        at += size + 12;
    }
    if (!animated)
    {
        return true;
    }
    if (header == NULL || declared_frames != frames.size() || !background_fits(frames.size(), read_be(header, 4), read_be(header + 4, 4), width, height))
    {
        return false;
    }
    iw = (int)read_be(header, 4);
    ih = (int)read_be(header + 4, 4);
    std::vector<uint8_t> canvas((size_t)iw * ih * 4);
    for (apng_frame& frame : frames)
    {
        if ((uint64_t)frame.x + frame.width > (uint64_t)iw || (uint64_t)frame.y + frame.height > (uint64_t)ih)
        {
            return false;
        }
        uint8_t frame_header[13];
        memcpy(frame_header, header, 13);
        for (int i = 0; i < 4; i++)
        {
            frame_header[i] = (uint8_t)(frame.width >> (24 - 8 * i));
            frame_header[4 + i] = (uint8_t)(frame.height >> (24 - 8 * i));
        }
        std::string png("\x89PNG\r\n\x1a\n", 8);
        append_chunk(png, "IHDR", frame_header, 13);
        png += palette;
        append_chunk(png, "IDAT", (const uint8_t*)frame.data.data(), frame.data.size());
        append_chunk(png, "IEND", frame_header, 0);
        int fw = 0;
        int fh = 0;
        int channels = 0;
        stbi_uc* pixels = stbi_load_from_memory((const stbi_uc*)png.data(), (int)png.size(), &fw, &fh, &channels, 4);
        bool fits = (uint32_t)fw == frame.width && (uint32_t)fh == frame.height;
        bool full = frame.x == 0 && frame.y == 0 && fw == iw && fh == ih;
        if (pixels == NULL || !fits || (default_frame && &frame == &frames[0] && !full))
        {
            stbi_image_free(pixels);
            return false;
        }
        std::vector<uint8_t> prior = frame.dispose == 2 ? canvas : std::vector<uint8_t>();
        for (int row = 0; row < fh; row++)
        {
            for (int col = 0; col < fw; col++)
            {
                const uint8_t* source = pixels + ((size_t)row * fw + col) * 4;
                uint8_t* target = &canvas[(((size_t)frame.y + row) * iw + frame.x + col) * 4];
                if (frame.blend == 0 || source[3] == 255)
                {
                    memcpy(target, source, 4);
                }
                else if (source[3] != 0)
                {
                    float source_alpha = source[3] / 255.0f;
                    float target_alpha = target[3] / 255.0f * (1.0f - source_alpha);
                    float alpha = source_alpha + target_alpha;
                    for (int c = 0; c < 3; c++)
                    {
                        target[c] = (uint8_t)((source[c] * source_alpha + target[c] * target_alpha) / alpha + 0.5f);
                    }
                    target[3] = (uint8_t)(alpha * 255.0f + 0.5f);
                }
            }
        }
        stbi_image_free(pixels);
        canvases.insert(canvases.end(), canvas.begin(), canvas.end());
        delays.push_back(frame.ms);
        if (frame.dispose == 1)
        {
            for (int row = 0; row < fh; row++)
            {
                memset(&canvas[(((size_t)frame.y + row) * iw + frame.x) * 4], 0, (size_t)fw * 4);
            }
        }
        if (frame.dispose == 2)
        {
            canvas = std::move(prior);
        }
    }
    return true;
}

static bool decode_background(const uint8_t* bytes, size_t length, const char* expected_type, int width, int height, std::vector<std::vector<uint8_t>>& frames, std::vector<float>& ms)
{
    static const std::map<std::string, std::pair<std::string, std::string>> formats = {
        {"image/png", {std::string("\x89PNG", 4), std::string("IEND\xae\x42\x60\x82", 8)}},
        {"image/jpeg", {std::string("\xff\xd8", 2), std::string("\xff\xd9", 2)}},
        {"image/gif", {std::string("GIF8", 4), std::string(";", 1)}},
    };
    auto format = formats.find(expected_type);
    if (format == formats.end())
    {
        return false;
    }
    const std::string& magic = format->second.first;
    const std::string& trailer = format->second.second;
    if (length < magic.size() + trailer.size() || memcmp(bytes, magic.data(), magic.size()) != 0 || memcmp(bytes + length - trailer.size(), trailer.data(), trailer.size()) != 0)
    {
        return false;
    }
    int iw = 0;
    int ih = 0;
    int channels = 0;
    std::vector<uint8_t> apng;
    std::vector<float> delays_ms;
    if (format->first == "image/png" && !decode_apng(bytes, length, width, height, iw, ih, apng, delays_ms))
    {
        return false;
    }
    stbi_uc* pixels = NULL;
    if (delays_ms.empty())
    {
        if (!stbi_info_from_memory(bytes, (int)length, &iw, &ih, &channels) || !background_fits(format->first == "image/gif" ? gif_frame_count(bytes, length) : 1, (uint64_t)iw, (uint64_t)ih, width, height))
        {
            return false;
        }
        int count = 1;
        int* delays = NULL;
        pixels = format->first == "image/gif" ? stbi_load_gif_from_memory(bytes, (int)length, &delays, &iw, &ih, &count, &channels, 4) : stbi_load_from_memory(bytes, (int)length, &iw, &ih, &channels, 4);
        for (int i = 0; pixels != NULL && i < count; i++)
        {
            delays_ms.push_back(delays != NULL ? (float)delays[i] : 100.0f);
        }
        free(delays);
        if (pixels == NULL)
        {
            return false;
        }
    }
    const uint8_t* source = pixels != NULL ? pixels : apng.data();
    double scale = std::fmax((double)width / iw, (double)height / ih);
    std::vector<std::vector<std::pair<int, float>>> columns = axis_weights(iw, width, (int)std::ceil(iw * scale));
    std::vector<std::vector<std::pair<int, float>>> rows = axis_weights(ih, height, (int)std::ceil(ih * scale));
    for (size_t i = 0; i < delays_ms.size(); i++)
    {
        float frame_ms = delays_ms[i];
        if (frame_ms < 20.0f)
        {
            frame_ms = 100.0f;
        }
        frames.push_back(cover_frame(source + i * iw * ih * 4, iw, ih, columns, rows));
        ms.push_back(frame_ms);
    }
    stbi_image_free(pixels);
    return true;
}

static void draw_model(engine* e, VkCommandBuffer commands, VkImageView target, int width, int height, VkImageView back, bool opaque)
{
    float matrix[16];
    view_projection(e, width, height, matrix);
    vk_renderer_draw(e->own, commands, target, width, height, matrix, e->eye, kLightDirection, kLightColor, kAmbientColor, e->clock_seconds, e->pending, back, opaque);
    e->pending = 0;
}

struct present_target
{
    window* owner = NULL;
    VkSurfaceKHR surface = VK_NULL_HANDLE;
    VkSurfaceFormatKHR format{};
    VkSwapchainKHR swapchain = VK_NULL_HANDLE;
    VkExtent2D extent{};
    int polled[2] = {-1, -1};
    std::chrono::steady_clock::time_point deadline{};
    std::vector<VkImageView> views;
    std::vector<VkImage> images;
    std::vector<VkSemaphore> rendered;
    VkSemaphore acquired = VK_NULL_HANDLE;
    VkCommandBuffer commands = VK_NULL_HANDLE;
    VkFence fence = VK_NULL_HANDLE;
    vk_image background;
    vk_image model;
    present_state shown;
};

static present_target target;

static void wait_present()
{
    if (target.fence != VK_NULL_HANDLE)
    {
        vkWaitForFences(device, 1, &target.fence, VK_TRUE, UINT64_MAX);
    }
}

const uint8_t* engine_render(engine* e, int width, int height, float zoom, float x, float y, float rot, float background_clock_ms)
{
    size_target& t = target_at(e, width, height);
    bool scaled = width != e->width || height != e->height;
    const uint8_t* back = frame_at(e, scaled ? t.background_frames : e->background_frames, background_clock_ms);
    size_t count = (size_t)width * height;
    if (t.color.image == VK_NULL_HANDLE)
    {
        if (!vk_image_create(t.color, (uint32_t)width, (uint32_t)height, 1, kColorFormat, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT, VK_IMAGE_ASPECT_COLOR_BIT) || !vk_buffer_create(t.readback, count * 4, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true))
        {
            vk_image_destroy(t.color);
            vk_buffer_destroy(t.readback);
            return NULL;
        }
        t.rgba.resize(count * 4);
    }
    std::unique_lock<std::mutex> hold(e->lock);
    view_apply(e, zoom, x, y, rot);
    if (render_cached(e, t, back))
    {
        return t.rgba.data();
    }
    t.rendered_serial = e->serial;
    t.rendered_back = back;
    t.rendered_back_load = e->background_serial;
    auto started = std::chrono::steady_clock::now();
    wait_present();
    VkCommandBuffer commands = e->frame_commands;
    vkResetCommandPool(device, e->frame_pool, 0);
    VkCommandBufferBeginInfo begin{};
    begin.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(commands, &begin);
    vkCmdResetQueryPool(commands, e->timer, 0, 2);
    vkCmdWriteTimestamp(commands, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, e->timer, 0);
    vk_barrier(commands, t.color.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT);
    draw_model(e, commands, t.color.view, width, height, VK_NULL_HANDLE, false);
    vk_barrier(commands, t.color.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_ACCESS_TRANSFER_READ_BIT);
    VkBufferImageCopy region{};
    region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.imageExtent = {(uint32_t)width, (uint32_t)height, 1};
    vkCmdCopyImageToBuffer(commands, t.color.image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, t.readback.buffer, 1, &region);
    VkBufferMemoryBarrier host{};
    host.sType = VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER;
    host.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    host.dstAccessMask = VK_ACCESS_HOST_READ_BIT;
    host.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    host.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    host.buffer = t.readback.buffer;
    host.size = VK_WHOLE_SIZE;
    vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0, 0, NULL, 1, &host, 0, NULL);
    vkCmdWriteTimestamp(commands, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, e->timer, 1);
    auto committed = std::chrono::steady_clock::now();
    vkEndCommandBuffer(commands);
    vkResetFences(device, 1, &e->frame_fence);
    submit(commands, VK_NULL_HANDLE, 0, VK_NULL_HANDLE, e->frame_fence);
    vkWaitForFences(device, 1, &e->frame_fence, VK_TRUE, UINT64_MAX);
    hold.unlock();
    double waited_ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - committed).count();
    uint64_t stamps[2] = {0, 0};
    if (vkGetQueryPoolResults(device, e->timer, 0, 2, sizeof(stamps), stamps, sizeof(uint64_t), VK_QUERY_RESULT_64_BIT) == VK_SUCCESS)
    {
        e->gpu_ms = (double)(stamps[1] - stamps[0]) * timestamp_period / 1e6;
    }
    compose_rgba(t.rgba.data(), count, (const uint8_t*)t.readback.mapped, back);
    e->cpu_ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count() - waited_ms;
    return t.rgba.data();
}

#ifdef _WIN32
struct spout_info
{
    uint32_t share_handle;
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uint32_t usage;
    uint8_t description[256];
    uint32_t partner_id;
};

struct spout_sender
{
    HANDLE names = NULL;
    HANDLE names_mutex = NULL;
    HANDLE info = NULL;
    HANDLE info_mutex = NULL;
    HANDLE active = NULL;
    HANDLE active_mutex = NULL;
    HANDLE access = NULL;
    char* names_view = NULL;
    spout_info* info_view = NULL;
    char* active_view = NULL;
    size_t slots = 0;
    uint32_t family = 0;
    ID3D11Device* d3d = NULL;
    ID3D11Texture2D* texture = NULL;
    VkImage image = VK_NULL_HANDLE;
    VkImageView view = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    VkExtent2D extent{};
    bool registered = false;
    bool released = false;
    bool failed = false;
};

static spout_sender spout;
static const VkFormat spout_formats[2] = {VK_FORMAT_B8G8R8A8_UNORM, kColorFormat};
static const char spout_name[] = "hikari-vrm";

static bool spout_lock(HANDLE mutex, DWORD timeout_ms)
{
    DWORD result = WaitForSingleObject(mutex, timeout_ms);
    return result == WAIT_OBJECT_0 || result == WAIT_ABANDONED;
}

static bool spout_register(bool add)
{
    if (spout.names_view == NULL || !spout_lock(spout.names_mutex, 67))
    {
        return false;
    }
    size_t used = 0;
    size_t found = spout.slots;
    while (used < spout.slots && spout.names_view[used * 256] != 0)
    {
        if (strncmp(spout.names_view + used * 256, spout_name, 256) == 0)
        {
            found = used;
        }
        used++;
    }
    bool added = add && found == spout.slots && used < spout.slots;
    if (added)
    {
        memcpy(spout.names_view + used * 256, spout_name, sizeof(spout_name));
        if (used + 1 < spout.slots)
        {
            spout.names_view[(used + 1) * 256] = 0;
        }
    }
    if (!add && found < spout.slots)
    {
        memmove(spout.names_view + found * 256, spout.names_view + (found + 1) * 256, (used - found - 1) * 256);
        memset(spout.names_view + (used - 1) * 256, 0, 256);
    }
    if ((added || !add) && spout_lock(spout.active_mutex, 67))
    {
        if (add && spout.active_view[0] == 0)
        {
            memcpy(spout.active_view, spout_name, sizeof(spout_name));
        }
        if (!add && strncmp(spout.active_view, spout_name, 256) == 0)
        {
            memcpy(spout.active_view, spout.names_view, 256);
        }
        ReleaseMutex(spout.active_mutex);
    }
    ReleaseMutex(spout.names_mutex);
    return added;
}

static const char* spout_open()
{
    spout.info = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, sizeof(spout_info), spout_name);
    spout.info_mutex = CreateMutexA(NULL, FALSE, "hikari-vrm_mutex");
    spout.names = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 64 * 256, "SpoutSenderNames");
    spout.names_mutex = CreateMutexA(NULL, FALSE, "SpoutSenderNames_mutex");
    spout.active = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 256, "ActiveSenderName");
    spout.active_mutex = CreateMutexA(NULL, FALSE, "ActiveSenderName_mutex");
    spout.access = CreateMutexA(NULL, FALSE, "hikari-vrm_SpoutAccessMutex");
    for (HANDLE handle : {spout.info, spout.info_mutex, spout.names, spout.names_mutex, spout.active, spout.active_mutex, spout.access})
    {
        if (handle == NULL)
        {
            return "shared memory";
        }
    }
    spout.info_view = (spout_info*)MapViewOfFile(spout.info, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    spout.names_view = (char*)MapViewOfFile(spout.names, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    spout.active_view = (char*)MapViewOfFile(spout.active, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    if (spout.info_view == NULL || spout.names_view == NULL || spout.active_view == NULL)
    {
        return "shared memory view";
    }
    MEMORY_BASIC_INFORMATION region{};
    VirtualQuery(spout.names_view, &region, sizeof(region));
    spout.slots = region.RegionSize / 256;
    spout.family = (uint32_t)graphics_family(physical_device);
    spout.registered = spout_register(true);
    return spout.registered ? NULL : "sender hikari already exists or the sender list is busy";
}

static void spout_drop_image()
{
    if (spout.image != VK_NULL_HANDLE)
    {
        wait_idle();
    }
    vkDestroyImageView(device, spout.view, NULL);
    vkDestroyImage(device, spout.image, NULL);
    vkFreeMemory(device, spout.memory, NULL);
    spout.view = VK_NULL_HANDLE;
    spout.image = VK_NULL_HANDLE;
    spout.memory = VK_NULL_HANDLE;
    spout.released = false;
    if (spout.texture != NULL)
    {
        spout.texture->Release();
        spout.texture = NULL;
    }
}

static const char* spout_device()
{
    VkPhysicalDeviceExternalImageFormatInfo external_format{};
    external_format.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO;
    external_format.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_KMT_BIT;
    VkImageFormatListCreateInfo format_list{};
    format_list.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO;
    format_list.pNext = &external_format;
    format_list.viewFormatCount = 2;
    format_list.pViewFormats = spout_formats;
    VkPhysicalDeviceImageFormatInfo2 format_info{};
    format_info.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2;
    format_info.pNext = &format_list;
    format_info.format = spout_formats[0];
    format_info.type = VK_IMAGE_TYPE_2D;
    format_info.tiling = VK_IMAGE_TILING_OPTIMAL;
    format_info.usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    format_info.flags = VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
    VkExternalImageFormatProperties external_properties{};
    external_properties.sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES;
    VkImageFormatProperties2 format_properties{};
    format_properties.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
    format_properties.pNext = &external_properties;
    if (vkGetPhysicalDeviceImageFormatProperties2(physical_device, &format_info, &format_properties) != VK_SUCCESS || !(external_properties.externalMemoryProperties.externalMemoryFeatures & VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT))
    {
        return "vulkan cannot import d3d11 textures";
    }
    VkPhysicalDeviceIDProperties id{};
    id.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES;
    VkPhysicalDeviceProperties2 properties{};
    properties.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2;
    properties.pNext = &id;
    vkGetPhysicalDeviceProperties2(physical_device, &properties);
    IDXGIFactory1* factory = NULL;
    if (!id.deviceLUIDValid || FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void**)&factory)))
    {
        return "no dxgi adapter";
    }
    IDXGIAdapter1* adapter = NULL;
    for (UINT i = 0; spout.d3d == NULL && factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; i++)
    {
        DXGI_ADAPTER_DESC1 desc{};
        adapter->GetDesc1(&desc);
        if (memcmp(&desc.AdapterLuid, id.deviceLUID, VK_LUID_SIZE) == 0)
        {
            D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, NULL, 0, NULL, 0, D3D11_SDK_VERSION, &spout.d3d, NULL, NULL);
        }
        adapter->Release();
    }
    factory->Release();
    return spout.d3d == NULL ? "no d3d11 device on the vulkan adapter" : NULL;
}

static const char* spout_texture(VkExtent2D extent)
{
    spout_drop_image();
    D3D11_TEXTURE2D_DESC desc{};
    desc.Width = extent.width;
    desc.Height = extent.height;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED;
    if (FAILED(spout.d3d->CreateTexture2D(&desc, NULL, &spout.texture)))
    {
        return "d3d11 texture";
    }
    HANDLE handle = NULL;
    IDXGIResource* resource = NULL;
    if (SUCCEEDED(spout.texture->QueryInterface(__uuidof(IDXGIResource), (void**)&resource)))
    {
        resource->GetSharedHandle(&handle);
        resource->Release();
    }
    if (handle == NULL)
    {
        return "d3d11 share handle";
    }
    VkExternalMemoryImageCreateInfo external{};
    external.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
    external.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_KMT_BIT;
    VkImageFormatListCreateInfo format_list{};
    format_list.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO;
    format_list.pNext = &external;
    format_list.viewFormatCount = 2;
    format_list.pViewFormats = spout_formats;
    VkImageCreateInfo info{};
    info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    info.pNext = &format_list;
    info.flags = VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
    info.imageType = VK_IMAGE_TYPE_2D;
    info.format = spout_formats[0];
    info.extent = {extent.width, extent.height, 1};
    info.mipLevels = 1;
    info.arrayLayers = 1;
    info.samples = VK_SAMPLE_COUNT_1_BIT;
    info.tiling = VK_IMAGE_TILING_OPTIMAL;
    info.usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    if (vkCreateImage(device, &info, NULL, &spout.image) != VK_SUCCESS)
    {
        spout.image = VK_NULL_HANDLE;
        return "vulkan image";
    }
    VkMemoryRequirements requirements;
    vkGetImageMemoryRequirements(device, spout.image, &requirements);
    uint32_t bits = requirements.memoryTypeBits;
    VkMemoryWin32HandlePropertiesKHR handle_properties{};
    handle_properties.sType = VK_STRUCTURE_TYPE_MEMORY_WIN32_HANDLE_PROPERTIES_KHR;
    PFN_vkGetMemoryWin32HandlePropertiesKHR handle_query = (PFN_vkGetMemoryWin32HandlePropertiesKHR)vkGetDeviceProcAddr(device, "vkGetMemoryWin32HandlePropertiesKHR");
    if (handle_query != NULL && handle_query(device, VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_KMT_BIT, handle, &handle_properties) == VK_SUCCESS)
    {
        bits &= handle_properties.memoryTypeBits;
    }
    VkPhysicalDeviceMemoryProperties memory;
    vkGetPhysicalDeviceMemoryProperties(physical_device, &memory);
    uint32_t chosen = UINT32_MAX;
    for (VkMemoryPropertyFlags wanted : {(VkMemoryPropertyFlags)VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, (VkMemoryPropertyFlags)0})
    {
        for (uint32_t i = 0; i < memory.memoryTypeCount; i++)
        {
            if (chosen == UINT32_MAX && (bits & (1u << i)) && (memory.memoryTypes[i].propertyFlags & wanted) == wanted)
            {
                chosen = i;
            }
        }
    }
    VkMemoryDedicatedAllocateInfo dedicated{};
    dedicated.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
    dedicated.image = spout.image;
    VkImportMemoryWin32HandleInfoKHR imported{};
    imported.sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_WIN32_HANDLE_INFO_KHR;
    imported.pNext = &dedicated;
    imported.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_KMT_BIT;
    imported.handle = handle;
    VkMemoryAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate.pNext = &imported;
    allocate.allocationSize = requirements.size;
    allocate.memoryTypeIndex = chosen;
    if (chosen == UINT32_MAX || vkAllocateMemory(device, &allocate, NULL, &spout.memory) != VK_SUCCESS)
    {
        spout.memory = VK_NULL_HANDLE;
        return "vulkan memory import";
    }
    if (vkBindImageMemory(device, spout.image, spout.memory, 0) != VK_SUCCESS)
    {
        return "vulkan memory bind";
    }
    VkImageViewCreateInfo view{};
    view.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO;
    view.image = spout.image;
    view.viewType = VK_IMAGE_VIEW_TYPE_2D;
    view.format = kColorFormat;
    view.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    if (vkCreateImageView(device, &view, NULL, &spout.view) != VK_SUCCESS)
    {
        spout.view = VK_NULL_HANDLE;
        return "vulkan image view";
    }
    spout.extent = extent;
    spout_info shared{};
    shared.share_handle = (uint32_t)(uintptr_t)handle;
    shared.width = extent.width;
    shared.height = extent.height;
    shared.format = DXGI_FORMAT_B8G8R8A8_UNORM;
    if (!spout_lock(spout.info_mutex, 67))
    {
        return "sender info busy";
    }
    *spout.info_view = shared;
    ReleaseMutex(spout.info_mutex);
    return NULL;
}

static void spout_release()
{
    if (spout.registered)
    {
        spout_register(false);
    }
    spout_drop_image();
    if (spout.d3d != NULL)
    {
        spout.d3d->Release();
    }
    for (void* view : {(void*)spout.names_view, (void*)spout.info_view, (void*)spout.active_view})
    {
        if (view != NULL)
        {
            UnmapViewOfFile(view);
        }
    }
    for (HANDLE handle : {spout.info, spout.info_mutex, spout.names, spout.names_mutex, spout.active, spout.active_mutex, spout.access})
    {
        if (handle != NULL)
        {
            CloseHandle(handle);
        }
    }
    spout = spout_sender();
}

static bool spout_ready(VkExtent2D extent)
{
    if (spout.failed)
    {
        return false;
    }
    if (spout.image != VK_NULL_HANDLE && spout.extent.width == extent.width && spout.extent.height == extent.height)
    {
        return true;
    }
    const char* reason = can_share ? NULL : "VK_KHR_external_memory_win32 unsupported";
    if (reason == NULL && spout.info == NULL)
    {
        reason = spout_open();
    }
    if (reason == NULL && spout.d3d == NULL)
    {
        reason = spout_device();
    }
    if (reason == NULL)
    {
        reason = spout_texture(extent);
    }
    if (reason != NULL)
    {
        fprintf(stderr, "engine: spout share unavailable (%s)\n", reason);
        spout_release();
        spout.failed = true;
        return false;
    }
    return true;
}

static void spout_transition(VkCommandBuffer commands, bool acquire)
{
    VkImageMemoryBarrier barrier{};
    barrier.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    barrier.image = spout.image;
    barrier.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    if (acquire)
    {
        barrier.dstAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT;
        barrier.oldLayout = spout.released ? VK_IMAGE_LAYOUT_GENERAL : VK_IMAGE_LAYOUT_UNDEFINED;
        barrier.newLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
        barrier.srcQueueFamilyIndex = spout.released ? VK_QUEUE_FAMILY_EXTERNAL : VK_QUEUE_FAMILY_IGNORED;
        barrier.dstQueueFamilyIndex = spout.released ? spout.family : VK_QUEUE_FAMILY_IGNORED;
        vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);
        return;
    }
    barrier.srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT;
    barrier.oldLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    barrier.newLayout = VK_IMAGE_LAYOUT_GENERAL;
    barrier.srcQueueFamilyIndex = spout.family;
    barrier.dstQueueFamilyIndex = VK_QUEUE_FAMILY_EXTERNAL;
    vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);
    spout.released = true;
}
#endif

static void release_swapchain_images()
{
    for (size_t i = 0; i < target.views.size(); i++)
    {
        vkDestroyImageView(device, target.views[i], NULL);
        vkDestroySemaphore(device, target.rendered[i], NULL);
    }
    target.views.clear();
    target.rendered.clear();
    target.images.clear();
}

static void release_target()
{
    wait_idle();
    release_swapchain_images();
    vkDestroySwapchainKHR(device, target.swapchain, NULL);
    vkDestroySemaphore(device, target.acquired, NULL);
    vkDestroyFence(device, target.fence, NULL);
    vkFreeCommandBuffers(device, command_pool, 1, &target.commands);
    vk_image_destroy(target.background);
    vk_image_destroy(target.model);
    vkDestroySurfaceKHR(instance, target.surface, NULL);
    target = present_target();
#ifdef _WIN32
    spout_release();
#endif
}

void window_release_target(window* w)
{
    if (target.owner == w)
    {
        release_target();
    }
}

static bool open_target(window* w)
{
    target.surface = window_surface(w, instance);
    VkBool32 supported = VK_FALSE;
    if (target.surface != VK_NULL_HANDLE)
    {
        vkGetPhysicalDeviceSurfaceSupportKHR(physical_device, (uint32_t)graphics_family(physical_device), target.surface, &supported);
    }
    uint32_t count = 0;
    if (supported)
    {
        vkGetPhysicalDeviceSurfaceFormatsKHR(physical_device, target.surface, &count, NULL);
    }
    std::vector<VkSurfaceFormatKHR> formats(count);
    if (count > 0)
    {
        vkGetPhysicalDeviceSurfaceFormatsKHR(physical_device, target.surface, &count, formats.data());
    }
    for (const VkSurfaceFormatKHR& format : formats)
    {
        if (format.format == kColorFormat && format.colorSpace == VK_COLOR_SPACE_SRGB_NONLINEAR_KHR)
        {
            target.format = format;
        }
    }
    if (target.format.format != kColorFormat)
    {
        fprintf(stderr, "engine: window surface has no B8G8R8A8_SRGB format\n");
        vkDestroySurfaceKHR(instance, target.surface, NULL);
        target = present_target();
        return false;
    }
    VkSemaphoreCreateInfo semaphore_info{};
    semaphore_info.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO;
    vkCreateSemaphore(device, &semaphore_info, NULL, &target.acquired);
    VkFenceCreateInfo fence_info{};
    fence_info.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
    fence_info.flags = VK_FENCE_CREATE_SIGNALED_BIT;
    vkCreateFence(device, &fence_info, NULL, &target.fence);
    VkCommandBufferAllocateInfo allocate{};
    allocate.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
    allocate.commandPool = command_pool;
    allocate.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    allocate.commandBufferCount = 1;
    vkAllocateCommandBuffers(device, &allocate, &target.commands);
    target.owner = w;
    return true;
}

static bool build_swapchain(const VkSurfaceCapabilitiesKHR& capabilities)
{
    wait_idle();
    uint32_t count = 0;
    uint32_t images = capabilities.minImageCount + 1;
    if (capabilities.maxImageCount > 0 && images > capabilities.maxImageCount)
    {
        images = capabilities.maxImageCount;
    }
    VkSwapchainCreateInfoKHR info{};
    info.sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR;
    info.surface = target.surface;
    info.minImageCount = images;
    info.imageFormat = target.format.format;
    info.imageColorSpace = target.format.colorSpace;
    info.imageExtent = capabilities.currentExtent;
    info.imageArrayLayers = 1;
    info.imageUsage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    info.imageSharingMode = VK_SHARING_MODE_EXCLUSIVE;
    info.preTransform = capabilities.currentTransform;
    info.compositeAlpha = (capabilities.supportedCompositeAlpha & VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR) ? VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR : (VkCompositeAlphaFlagBitsKHR)(capabilities.supportedCompositeAlpha & (~capabilities.supportedCompositeAlpha + 1));
    info.presentMode = VK_PRESENT_MODE_FIFO_KHR;
    info.clipped = VK_TRUE;
    info.oldSwapchain = target.swapchain;
    VkSwapchainKHR swapchain = VK_NULL_HANDLE;
    VkResult result = vkCreateSwapchainKHR(device, &info, NULL, &swapchain);
    release_swapchain_images();
    vkDestroySwapchainKHR(device, target.swapchain, NULL);
    target.swapchain = swapchain;
    if (result != VK_SUCCESS)
    {
        target.swapchain = VK_NULL_HANDLE;
        return false;
    }
    vkGetSwapchainImagesKHR(device, swapchain, &count, NULL);
    target.images.resize(count);
    vkGetSwapchainImagesKHR(device, swapchain, &count, target.images.data());
    VkSemaphoreCreateInfo semaphore_info{};
    semaphore_info.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO;
    for (VkImage image : target.images)
    {
        VkImageViewCreateInfo view_info{};
        view_info.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO;
        view_info.image = image;
        view_info.viewType = VK_IMAGE_VIEW_TYPE_2D;
        view_info.format = target.format.format;
        view_info.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
        VkImageView view = VK_NULL_HANDLE;
        vkCreateImageView(device, &view_info, NULL, &view);
        target.views.push_back(view);
        VkSemaphore semaphore = VK_NULL_HANDLE;
        vkCreateSemaphore(device, &semaphore_info, NULL, &semaphore);
        target.rendered.push_back(semaphore);
    }
    target.extent = capabilities.currentExtent;
    return true;
}

static bool show_background(engine* e, const uint8_t* back)
{
    if (back == NULL)
    {
        return false;
    }
    if (target.background.image == VK_NULL_HANDLE || (int)target.background.width != e->width || (int)target.background.height != e->height)
    {
        vk_image_destroy(target.background);
        target.shown.background_uploaded = NULL;
        if (!vk_host_image_create(target.background, (uint32_t)e->width, (uint32_t)e->height, VK_FORMAT_R8G8B8A8_SRGB))
        {
            fprintf(stderr, "engine: background image failed\n");
            return false;
        }
    }
    if (background_stale(target.shown, e, back))
    {
        vk_host_image_write(target.background, back);
    }
    return true;
}

#ifdef _WIN32
static bool model_target(VkExtent2D extent)
{
    if (target.model.image != VK_NULL_HANDLE && target.model.width == extent.width && target.model.height == extent.height)
    {
        return true;
    }
    wait_idle();
    vk_image_destroy(target.model);
    return vk_image_create(target.model, extent.width, extent.height, 1, kColorFormat, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_SAMPLED_BIT, VK_IMAGE_ASPECT_COLOR_BIT);
}
#endif

static void pace(float frame_seconds)
{
    target.deadline += std::chrono::duration_cast<std::chrono::steady_clock::duration>(std::chrono::duration<double>(frame_seconds));
    std::chrono::steady_clock::time_point now = std::chrono::steady_clock::now();
    if (target.deadline > now)
    {
        std::this_thread::sleep_until(target.deadline);
    }
    else
    {
        target.deadline = now;
    }
}

static bool refresh_swapchain(const int* polled)
{
    if (target.swapchain != VK_NULL_HANDLE && target.extent.width != 0 && polled[0] == target.polled[0] && polled[1] == target.polled[1])
    {
        return true;
    }
    VkSurfaceCapabilitiesKHR capabilities;
    if (vkGetPhysicalDeviceSurfaceCapabilitiesKHR(physical_device, target.surface, &capabilities) != VK_SUCCESS || capabilities.currentExtent.width == 0 || capabilities.currentExtent.height == 0 || capabilities.currentExtent.width == UINT32_MAX)
    {
        return false;
    }
    if ((target.swapchain == VK_NULL_HANDLE || capabilities.currentExtent.width != target.extent.width || capabilities.currentExtent.height != target.extent.height) && !build_swapchain(capabilities))
    {
        return false;
    }
    target.polled[0] = polled[0];
    target.polled[1] = polled[1];
    return true;
}

bool engine_present(engine* e, window* w, float background_clock_ms, float frame_seconds, bool share)
{
    if (!can_present || w == NULL)
    {
        return false;
    }
    if (target.owner != w)
    {
        if (target.owner != NULL)
        {
            release_target();
        }
        if (!open_target(w))
        {
            return false;
        }
    }
    int polled[2] = {0, 0};
    window_size(w, &polled[0], &polled[1]);
    bool hidden = polled[0] <= 0 || polled[1] <= 0;
#ifdef _WIN32
    if (!share && spout.info != NULL)
    {
        spout_release();
    }
    if (hidden && share && spout.image == VK_NULL_HANDLE)
    {
        share = false;
    }
#else
    share = false;
#endif
    if (!hidden && target.swapchain != VK_NULL_HANDLE && polled[0] == target.polled[0] && polled[1] == target.polled[1])
    {
        std::lock_guard<std::mutex> hold(e->lock);
        if (present_unchanged(target.shown, e, frame_at(e, e->background_frames, background_clock_ms), share))
        {
            return false;
        }
    }
    if (hidden && !share)
    {
        std::this_thread::sleep_for(std::chrono::duration<double>(frame_seconds));
        return false;
    }
    pace(frame_seconds);
    if (!hidden && !refresh_swapchain(polled))
    {
        return false;
    }
#ifdef _WIN32
    VkExtent2D extent = hidden ? spout.extent : target.extent;
#else
    VkExtent2D extent = target.extent;
#endif
    wait_present();
    uint32_t index = 0;
    VkResult acquired = VK_SUCCESS;
    if (!hidden)
    {
        acquired = vkAcquireNextImageKHR(device, target.swapchain, UINT64_MAX, target.acquired, VK_NULL_HANDLE, &index);
        if (acquired != VK_SUCCESS && acquired != VK_SUBOPTIMAL_KHR)
        {
            target.extent = VkExtent2D{};
            return false;
        }
    }
    std::lock_guard<std::mutex> hold(e->lock);
    const uint8_t* back = frame_at(e, e->background_frames, background_clock_ms);
    bool has_back = show_background(e, back);
    VkImageView back_view = has_back ? target.background.view : VK_NULL_HANDLE;
#ifdef _WIN32
    bool shared = share && model_target(extent) && spout_ready(extent) && spout_lock(spout.access, 2);
#else
    bool shared = false;
#endif
    if (hidden && !shared)
    {
        return false;
    }
    vkResetFences(device, 1, &target.fence);
    VkCommandBuffer commands = target.commands;
    vkResetCommandBuffer(commands, 0);
    VkCommandBufferBeginInfo begin{};
    begin.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(commands, &begin);
    VkPipelineStageFlags attachment = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
    if (!hidden)
    {
        vk_barrier(commands, target.images[index], VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, attachment, 0, attachment, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT);
    }
    if (shared)
    {
        vk_barrier(commands, target.model.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, attachment, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT);
        draw_model(e, commands, target.model.view, (int)extent.width, (int)extent.height, VK_NULL_HANDLE, false);
        vk_barrier(commands, target.model.image, VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, attachment, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_ACCESS_SHADER_READ_BIT);
#ifdef _WIN32
        spout_transition(commands, true);
        vk_renderer_share(commands, target.model.view, back_view, hidden ? VK_NULL_HANDLE : target.views[index], spout.view, (int)extent.width, (int)extent.height);
        spout_transition(commands, false);
#endif
    }
    else
    {
        draw_model(e, commands, target.views[index], (int)extent.width, (int)extent.height, back_view, true);
    }
    if (!hidden)
    {
        vk_barrier(commands, target.images[index], VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_IMAGE_LAYOUT_PRESENT_SRC_KHR, attachment, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0);
    }
    vkEndCommandBuffer(commands);
    submit(commands, hidden ? VK_NULL_HANDLE : target.acquired, attachment, hidden ? VK_NULL_HANDLE : target.rendered[index], target.fence);
#ifdef _WIN32
    if (shared)
    {
        wait_present();
        ReleaseMutex(spout.access);
    }
#endif
    present_mark(target.shown, e, back, shared);
    if (hidden)
    {
        return true;
    }
    VkPresentInfoKHR present{};
    present.sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR;
    present.waitSemaphoreCount = 1;
    present.pWaitSemaphores = &target.rendered[index];
    present.swapchainCount = 1;
    present.pSwapchains = &target.swapchain;
    present.pImageIndices = &index;
    queue_lock.lock();
    VkResult presented = vkQueuePresentKHR(queue, &present);
    queue_lock.unlock();
    if (presented != VK_SUCCESS || acquired == VK_SUBOPTIMAL_KHR)
    {
        target.extent = VkExtent2D{};
    }
    return presented == VK_SUCCESS || presented == VK_SUBOPTIMAL_KHR;
}
