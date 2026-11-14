#include <chrono>
#include <filesystem>
#include <map>
#include <mutex>
#include <new>

static const float kBackgroundStepMs = 2000.0f / 24.0f;
static const float kFieldOfViewDegrees = 30.0f;
static const float kNearPlane = 0.05f;
static const float kFrameMargin = 1.1f;
static const float kLightDirection[3] = {0.0f, 0.70710678f, 0.70710678f};
static const float kLightColor[3] = {1.0f, 1.0f, 1.0f};
static const float kAmbientColor[3] = {0.2f, 0.2f, 0.2f};
static const auto kTargetIdle = std::chrono::seconds(10);
static const uint64_t kBackgroundMaxFrames = 1024;
static const uint64_t kBackgroundMaxBytes = 512ull << 20;
static const uint64_t kBackgroundMaxPixels = 1ull << 25;
static const uintmax_t kModelMaxBytes = 512u << 20;

struct size_target : public target_gpu
{
    std::vector<uint8_t> rgba;
    std::vector<std::vector<uint8_t>> background_frames;
    uint64_t background_load = 0;
    uint64_t rendered_serial = 0;
    const uint8_t* rendered_back = NULL;
    uint64_t rendered_back_load = 0;
    std::chrono::steady_clock::time_point used;
};

struct engine : public engine_gpu
{
    hv_model* model = NULL;
    int width = 0;
    int height = 0;
    std::mutex lock;
    std::map<std::pair<int, int>, size_target> targets;
    std::vector<uint8_t> background_bytes;
    std::string background_type;
    std::vector<std::vector<uint8_t>> background_frames;
    std::vector<float> background_ms;
    float background_total_ms = 0;
    uint64_t background_serial = 0;
    float view_zoom = 1;
    float view_x = 0;
    float view_y = 0;
    float view_rot = 0;
    float eye[3] = {0, 0, 0};
    float target[3] = {0, 0, 0};
    uint32_t pending = 0xffffffffu;
    uint64_t serial = 1;
    double clock_seconds = 0;
    bool uv_animated = false;
    double gpu_ms = 0;
    double cpu_ms = 0;
    std::string model_key;
};

struct present_state
{
    const engine* presented = NULL;
    uint64_t serial = 0;
    const uint8_t* back = NULL;
    bool share = false;
    const uint8_t* background_uploaded = NULL;
    uint64_t background_uploaded_load = 0;
};

static std::string gpu_name;

inline std::string model_key(const char* vrm_path)
{
    std::error_code stamp_error;
    std::filesystem::path file = std::filesystem::u8path(vrm_path);
    return std::string(vrm_path) + "|" + std::to_string((long long)std::filesystem::file_size(file, stamp_error)) + "|" + std::to_string((long long)std::filesystem::last_write_time(file, stamp_error).time_since_epoch().count());
}

static bool model_uv_animated(hv_model* model)
{
    for (int32_t index = 0; index < hv_material_count(model); index++)
    {
        const hv_material* material = hv_material_at(model, index);
        if (material->kind == HV_MATERIAL_MTOON && (material->uv_animation_scroll_x != 0.0f || material->uv_animation_scroll_y != 0.0f || material->uv_animation_rotation != 0.0f))
        {
            return true;
        }
    }
    return false;
}

static std::mutex model_cache_lock;
static std::map<std::string, model_set> model_cache;

static void model_release(engine* e)
{
    std::lock_guard<std::mutex> hold(model_cache_lock);
    auto found = model_cache.find(e->model_key);
    if (found != model_cache.end() && --found->second.users == 0)
    {
        model_cache.erase(found);
    }
}

static std::vector<uint8_t> read_file(const std::string& path)
{
    std::error_code size_error;
    uintmax_t size = std::filesystem::file_size(std::filesystem::u8path(path), size_error);
    if (size_error || size > kModelMaxBytes)
    {
        return {};
    }
    std::vector<uint8_t> bytes;
    try
    {
        bytes.resize((size_t)size);
    }
    catch (const std::bad_alloc&)
    {
        return {};
    }
    std::ifstream file(std::filesystem::u8path(path), std::ios::binary);
    file.read((char*)bytes.data(), (std::streamsize)size);
    bytes.resize((size_t)file.gcount());
    return bytes;
}

static bool background_fits(uint64_t frames, uint64_t source_width, uint64_t source_height, int width, int height)
{
    if (frames == 0 || frames > kBackgroundMaxFrames || source_width == 0 || source_height == 0)
    {
        return false;
    }
    uint64_t source_pixels = source_width * source_height;
    return source_pixels <= kBackgroundMaxPixels && source_pixels * 4 * frames <= kBackgroundMaxBytes && (uint64_t)width * (uint64_t)height * 4 * frames <= kBackgroundMaxBytes;
}

static bool declared_size(const hv_texture* source, size_t width, size_t height)
{
    const uint8_t* bytes = source->bytes;
    size_t length = source->length;
    static const uint8_t png[8] = {0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n'};
    if (length >= 24 && memcmp(bytes, png, 8) == 0 && memcmp(bytes + 12, "IHDR", 4) == 0)
    {
        return width == ((size_t)bytes[16] << 24 | (size_t)bytes[17] << 16 | (size_t)bytes[18] << 8 | bytes[19]) && height == ((size_t)bytes[20] << 24 | (size_t)bytes[21] << 16 | (size_t)bytes[22] << 8 | bytes[23]);
    }
    if (length < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8)
    {
        return false;
    }
    size_t position = 2;
    while (position + 4 <= length && bytes[position] == 0xFF && bytes[position + 1] != 0xD9 && bytes[position + 1] != 0xDA)
    {
        uint8_t marker = bytes[position + 1];
        if (marker == 0xFF)
        {
            position += 1;
        }
        else if (marker == 0x01 || (marker >= 0xD0 && marker <= 0xD7))
        {
            position += 2;
        }
        else if (marker >= 0xC0 && marker <= 0xCF && marker != 0xC4 && marker != 0xC8 && marker != 0xCC)
        {
            return position + 9 <= length && height == ((size_t)bytes[position + 5] << 8 | bytes[position + 6]) && width == ((size_t)bytes[position + 7] << 8 | bytes[position + 8]);
        }
        else
        {
            position += 2 + std::max<size_t>(2, (size_t)bytes[position + 2] << 8 | bytes[position + 3]);
        }
    }
    return false;
}

static void write_error(char* error, int error_capacity, const std::string& message)
{
    if (error != NULL && error_capacity > 0)
    {
        snprintf(error, (size_t)error_capacity, "%s", message.c_str());
    }
}

static hv_model* load_model(const char* vrm_path, char* error, int error_capacity)
{
    std::string path(vrm_path);
    std::string name = std::filesystem::u8path(path).filename().u8string();
    std::vector<uint8_t> bytes = read_file(path);
    if (bytes.empty())
    {
        write_error(error, error_capacity, name + ": cannot read file or larger than 512 MiB");
        return NULL;
    }
    char message[512] = {};
    hv_model* model = hv_load(bytes.data(), bytes.size(), message, sizeof(message));
    if (model == NULL)
    {
        write_error(error, error_capacity, name + ": " + message);
    }
    return model;
}

static void frame_camera(engine* e)
{
    float minimum[3];
    float maximum[3];
    hv_bounds(e->model, minimum, maximum);
    float head[3] = {(minimum[0] + maximum[0]) * 0.5f, maximum[1], (minimum[2] + maximum[2]) * 0.5f};
    hv_bone_position(e->model, "head", head);
    float hips[3] = {head[0], (minimum[1] + maximum[1]) * 0.5f, head[2]};
    if (!hv_bone_position(e->model, "hips", hips))
    {
        hv_bone_position(e->model, "chest", hips);
    }
    float top = std::max(maximum[1], head[1]);
    float bottom = std::min(hips[1], top - 0.1f);
    float half_height = (top - bottom) * 0.5f * kFrameMargin;
    float distance = half_height / tanf(kFieldOfViewDegrees * 0.5f * (float)M_PI / 180.0f);
    e->target[0] = head[0];
    e->target[1] = (top + bottom) * 0.5f;
    e->target[2] = head[2];
    e->eye[0] = head[0];
    e->eye[1] = e->target[1];
    e->eye[2] = head[2] + distance;
}

static void multiply_matrix(const float* left, const float* right, float* out)
{
    for (int column = 0; column < 4; column++)
    {
        for (int row = 0; row < 4; row++)
        {
            float sum = 0;
            for (int k = 0; k < 4; k++)
            {
                sum += left[k * 4 + row] * right[column * 4 + k];
            }
            out[column * 4 + row] = sum;
        }
    }
}

static void view_projection(const engine* e, int width, int height, float* out)
{
    float forward[3] = {e->target[0] - e->eye[0], e->target[1] - e->eye[1], e->target[2] - e->eye[2]};
    float forward_length = sqrtf(forward[0] * forward[0] + forward[1] * forward[1] + forward[2] * forward[2]);
    for (float& component : forward)
    {
        component /= forward_length;
    }
    float side[3] = {-forward[2], 0.0f, forward[0]};
    float side_length = sqrtf(side[0] * side[0] + side[2] * side[2]);
    side[0] /= side_length;
    side[2] /= side_length;
    float up[3] = {side[1] * forward[2] - side[2] * forward[1], side[2] * forward[0] - side[0] * forward[2], side[0] * forward[1] - side[1] * forward[0]};
    float view[16] = {
        side[0], up[0], -forward[0], 0,
        side[1], up[1], -forward[1], 0,
        side[2], up[2], -forward[2], 0,
        -(side[0] * e->eye[0] + side[1] * e->eye[1] + side[2] * e->eye[2]),
        -(up[0] * e->eye[0] + up[1] * e->eye[1] + up[2] * e->eye[2]),
        forward[0] * e->eye[0] + forward[1] * e->eye[1] + forward[2] * e->eye[2], 1};
    float aspect = (float)width / (float)height;
    float focal = 1.0f / tanf(kFieldOfViewDegrees * 0.5f * (float)M_PI / 180.0f);
    float projection[16] = {
        focal / aspect, 0, 0, 0,
        0, focal, 0, 0,
        0, 0, 0, -1,
        0, 0, kNearPlane, 0};
    float angle = e->view_rot * (float)M_PI / 180.0f;
    float cosine = cosf(angle) * e->view_zoom;
    float sine = sinf(angle) * e->view_zoom;
    float screen[16] = {
        cosine, sine * aspect, 0, 0,
        -sine / aspect, cosine, 0, 0,
        0, 0, 1, 0,
        2.0f * e->view_x, 2.0f * e->view_y, 0, 1};
    float projected[16];
    multiply_matrix(projection, view, projected);
    multiply_matrix(screen, projected, out);
}

static bool view_apply(engine* e, float zoom, float x, float y, float rot)
{
    float before[4] = {e->view_zoom, e->view_x, e->view_y, e->view_rot};
    if (std::isfinite(zoom) && zoom > 0)
    {
        e->view_zoom = zoom;
    }
    if (std::isfinite(x))
    {
        e->view_x = x;
    }
    if (std::isfinite(y))
    {
        e->view_y = y;
    }
    if (std::isfinite(rot))
    {
        e->view_rot = rot;
    }
    if (before[0] == e->view_zoom && before[1] == e->view_x && before[2] == e->view_y && before[3] == e->view_rot)
    {
        return false;
    }
    e->serial++;
    return true;
}

bool engine_view_set(engine* e, float zoom, float x, float y, float rot)
{
    std::lock_guard<std::mutex> hold(e->lock);
    return view_apply(e, zoom, x, y, rot);
}

const char* engine_gpu_name(engine* e)
{
    return gpu_name.c_str();
}

int32_t engine_param_index(engine* e, const char* id)
{
    int32_t count = hv_param_count(e->model);
    for (int32_t index = 0; index < count; index++)
    {
        if (strcmp(hv_param_id(e->model, index), id) == 0)
        {
            return index;
        }
    }
    return -1;
}

bool engine_has_param(engine* e, const char* id)
{
    return engine_param_index(e, id) >= 0;
}

int32_t engine_param_count(engine* e)
{
    return hv_param_count(e->model);
}

const char* engine_param_at(engine* e, int index, float* minimum, float* maximum, float* default_value)
{
    if (index < 0 || index >= hv_param_count(e->model))
    {
        return NULL;
    }
    hv_param_range(e->model, index, minimum, maximum, default_value);
    return hv_param_id(e->model, index);
}

void engine_param_set(engine* e, int32_t index, float value)
{
    if (index >= 0 && index < hv_param_count(e->model) && std::isfinite(value))
    {
        hv_param_values(e->model)[index] = value;
    }
}

float* engine_bone_poses(engine* e)
{
    return hv_bone_poses(e->model);
}

void engine_update(engine* e, float dt_ms, engine_hook before_model_update, void* context)
{
    std::lock_guard<std::mutex> hold(e->lock);
    if (before_model_update != NULL)
    {
        before_model_update(context);
    }
    uint32_t changed = hv_update(e->model, dt_ms / 1000.0f);
    e->clock_seconds += dt_ms / 1000.0;
    e->pending |= changed;
    if (changed != 0 || e->uv_animated)
    {
        e->serial++;
    }
}

void engine_sweep(engine* e)
{
    std::lock_guard<std::mutex> hold(e->lock);
    auto now = std::chrono::steady_clock::now();
    for (auto it = e->targets.begin(); it != e->targets.end();)
    {
        it = now - it->second.used > kTargetIdle ? e->targets.erase(it) : std::next(it);
    }
}

static bool decode_background(const uint8_t* bytes, size_t length, const char* expected_type, int width, int height, std::vector<std::vector<uint8_t>>& frames, std::vector<float>& ms);

int engine_background_load(engine* e, const uint8_t* bytes, size_t length, const char* expected_type)
{
    std::vector<std::vector<uint8_t>> frames;
    std::vector<float> ms;
    if (!decode_background(bytes, length, expected_type, e->width, e->height, frames, ms))
    {
        return 0;
    }
    std::lock_guard<std::mutex> hold(e->lock);
    e->background_bytes.assign(bytes, bytes + length);
    e->background_type = expected_type;
    e->background_frames = std::move(frames);
    e->background_ms = std::move(ms);
    e->background_total_ms = 0;
    for (float frame_ms : e->background_ms)
    {
        e->background_total_ms += frame_ms;
    }
    e->background_serial++;
    e->serial++;
    return (int)e->background_frames.size();
}

void engine_background_clear(engine* e)
{
    std::lock_guard<std::mutex> hold(e->lock);
    e->background_bytes.clear();
    e->background_frames.clear();
    e->background_ms.clear();
    e->background_total_ms = 0;
    e->background_serial++;
    e->serial++;
}

static size_t background_index(const engine* e, float background_clock_ms)
{
    float t = fmodf(floorf(background_clock_ms / kBackgroundStepMs) * kBackgroundStepMs, e->background_total_ms);
    size_t index = 0;
    while (index + 1 < e->background_frames.size() && t >= e->background_ms[index])
    {
        t -= e->background_ms[index];
        index++;
    }
    return index;
}

static const uint8_t* frame_at(const engine* e, const std::vector<std::vector<uint8_t>>& frames, float background_clock_ms)
{
    if (frames.empty() || frames.size() != e->background_frames.size())
    {
        return NULL;
    }
    return frames[background_index(e, background_clock_ms)].data();
}

static size_target& target_at(engine* e, int width, int height)
{
    size_target& t = e->targets[{width, height}];
    t.used = std::chrono::steady_clock::now();
    if (t.background_load != e->background_serial)
    {
        t.background_load = e->background_serial;
        t.background_frames.clear();
        std::vector<float> ms;
        if ((width != e->width || height != e->height) && !e->background_bytes.empty())
        {
            decode_background(e->background_bytes.data(), e->background_bytes.size(), e->background_type.c_str(), width, height, t.background_frames, ms);
        }
    }
    return t;
}

static bool render_cached(const engine* e, const size_target& t, const uint8_t* back)
{
    return t.rendered_serial == e->serial && t.rendered_back == back && t.rendered_back_load == e->background_serial;
}

static uint8_t srgb_under(int top, int under, int keep)
{
    float sum = 0;
    float weight = 1;
    for (int value : {top, under})
    {
        float encoded = value / 255.0f;
        sum += weight * (encoded <= 0.04045f ? encoded / 12.92f : powf((encoded + 0.055f) / 1.055f, 2.4f));
        weight = keep / 255.0f;
    }
    sum = std::min(sum, 1.0f);
    float encoded = sum <= 0.0031308f ? sum * 12.92f : 1.055f * powf(sum, 1.0f / 2.4f) - 0.055f;
    return (uint8_t)(encoded * 255.0f + 0.5f);
}

static void compose_rgba(uint8_t* out, size_t count, const uint8_t* bgra, const uint8_t* back)
{
    for (size_t p = 0; p < count; p++, out += 4, bgra += 4)
    {
        int keep = 255 - bgra[3];
        int tops[3] = {bgra[2], bgra[1], bgra[0]};
        for (int channel = 0; channel < 3; channel++)
        {
            int top = tops[channel];
            int under = back != NULL ? back[p * 4 + channel] : 0;
            out[channel] = keep == 0 || under == 0 || (top == 0 && keep == 255) ? (uint8_t)((top * 255 + under * keep + 127) / 255) : srgb_under(top, under, keep);
        }
        out[3] = 255;
    }
}

extern "C" void engine_frame_ms(engine* e, double* gpu_ms, double* cpu_ms)
{
    *gpu_ms = e->gpu_ms;
    *cpu_ms = e->cpu_ms;
}

static bool present_unchanged(const present_state& shown, const engine* e, const uint8_t* back, bool share)
{
    return shown.presented == e && shown.serial == e->serial && shown.back == back && shown.share == share;
}

static void present_mark(present_state& shown, const engine* e, const uint8_t* back, bool share)
{
    shown.presented = e;
    shown.serial = e->serial;
    shown.back = back;
    shown.share = share;
}

static bool background_stale(present_state& shown, const engine* e, const uint8_t* back)
{
    if (back == shown.background_uploaded && e->background_serial == shown.background_uploaded_load)
    {
        return false;
    }
    shown.background_uploaded = back;
    shown.background_uploaded_load = e->background_serial;
    return true;
}
