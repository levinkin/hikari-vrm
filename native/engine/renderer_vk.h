#pragma once
#include <cstdint>
#include <vector>
#include <vulkan/vulkan.h>
#include "hv.h"

struct vk_buffer
{
    VkBuffer buffer = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    void* mapped = NULL;
};

struct vk_image
{
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    VkImageView view = VK_NULL_HANDLE;
    uint32_t width = 0;
    uint32_t height = 0;
    uint8_t* mapped = NULL;
    VkDeviceSize row_pitch = 0;
};

struct vk_renderer;

static const VkFormat kColorFormat = VK_FORMAT_B8G8R8A8_SRGB;

extern VkPhysicalDevice physical_device;
extern VkDevice device;

bool vk_buffer_create(vk_buffer& out, VkDeviceSize size, VkBufferUsageFlags usage, bool host);
void vk_buffer_destroy(vk_buffer& buffer);
bool vk_image_create(vk_image& out, uint32_t width, uint32_t height, uint32_t levels, VkFormat format, VkSampleCountFlagBits samples, VkImageUsageFlags usage, VkImageAspectFlags aspect);
void vk_image_destroy(vk_image& image);
bool vk_host_image_create(vk_image& out, uint32_t width, uint32_t height, VkFormat format);
void vk_host_image_write(vk_image& image, const uint8_t* rgba);
void vk_barrier(VkCommandBuffer commands, VkImage image, VkImageAspectFlags aspect, VkImageLayout from, VkImageLayout to, VkPipelineStageFlags source_stage, VkAccessFlags source_access, VkPipelineStageFlags target_stage, VkAccessFlags target_access);
VkCommandBuffer vk_once_begin();
void vk_once_end(VkCommandBuffer commands);

bool vk_renderer_start();
std::vector<uint8_t> renderer_texture_usage(hv_model* model);
vk_renderer* vk_renderer_create(hv_model* model, const std::vector<VkImageView>& srgb_textures, const std::vector<VkImageView>& linear_textures, const std::vector<VkSampler>& samplers, std::vector<vk_buffer>& statics);
void vk_renderer_destroy(vk_renderer* r, hv_model* model);
void vk_renderer_draw(vk_renderer* r, VkCommandBuffer commands, VkImageView target, int width, int height, const float* view_projection, const float* eye, const float* light_direction, const float* light_color, const float* ambient, double seconds, uint32_t changed, VkImageView back, bool opaque);
void vk_renderer_share(VkCommandBuffer commands, VkImageView model, VkImageView back, VkImageView window_target, VkImageView share_target, int width, int height);
