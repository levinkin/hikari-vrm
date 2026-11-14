#pragma once
#include <vulkan/vulkan.h>
#include "engine.h"
const char* window_surface_extension();
VkSurfaceKHR window_surface(window* w, VkInstance instance);
void window_size(window* w, int* width, int* height);
void window_release_target(window* w);
