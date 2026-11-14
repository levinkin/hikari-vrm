#pragma once
#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef struct engine engine;
typedef void (*engine_hook)(void* context);
typedef struct window window;
engine* engine_create(const char* vrm_path, int width, int height, char* error, int error_capacity);
void engine_destroy(engine* e);
const char* engine_gpu_name(engine* e);
bool engine_has_param(engine* e, const char* id);
int32_t engine_param_index(engine* e, const char* id);
int32_t engine_param_count(engine* e);
const char* engine_param_at(engine* e, int index, float* minimum, float* maximum, float* default_value);
void engine_param_set(engine* e, int32_t index, float value);
float* engine_bone_poses(engine* e);
void engine_update(engine* e, float dt_ms, engine_hook before_model_update, void* context);
void engine_sweep(engine* e);
int engine_background_load(engine* e, const uint8_t* bytes, size_t length, const char* expected_type);
void engine_background_clear(engine* e);
bool engine_view_set(engine* e, float zoom, float x, float y, float rot);
const uint8_t* engine_render(engine* e, int width, int height, float zoom, float x, float y, float rot, float background_clock_ms);
window* window_open(const char* title, int width, int height);
bool window_poll(window* w, int* width, int* height, engine_hook frame, void* context);
void window_close(window* w);
int window_ask_save(window* w, const char* model);
bool window_input(window* w, float* pan_x, float* pan_y, float* zoom, float* cursor_x, float* cursor_y, float* turn, int* keys);
bool engine_present(engine* e, window* w, float background_clock_ms, float frame_seconds, bool share);
#ifdef __cplusplus
}
#endif
