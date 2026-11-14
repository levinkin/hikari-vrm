extern "C" void engine_frame_ms(engine* e, double* gpu_ms, double* cpu_ms);

static const int kWidth = 960;
static const int kHeight = 720;

static double percentile(std::vector<double> values, double fraction)
{
    std::sort(values.begin(), values.end());
    return values[(size_t)std::min((double)values.size() - 1, std::floor(fraction * (double)(values.size() - 1) + 0.5))];
}

static void set_param(engine* e, const char* id, float value)
{
    int32_t index = engine_param_index(e, id);
    if (index >= 0)
    {
        engine_param_set(e, index, value);
    }
}

static int hostile_background_frames(engine* e)
{
    std::vector<uint8_t> gif = {'G', 'I', 'F', '8', '9', 'a', 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff};
    for (int frame = 0; frame < 200; frame++)
    {
        gif.insert(gif.end(), {0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00});
    }
    gif.push_back(';');
    return engine_background_load(e, gif.data(), gif.size(), "image/gif");
}

static void animate(engine* e, int frame, int frames)
{
    float t = (float)frame / (float)std::max(frames - 1, 1);
    float eye = fabsf(t * 4.0f - 1.0f) < 0.25f ? 0.0f : 1.0f;
    set_param(e, "ParamEyeLOpen", eye);
    set_param(e, "ParamEyeROpen", eye);
    set_param(e, "ParamMouthOpenY", t > 0.4f && t < 0.7f ? 1.0f : 0.0f);
    set_param(e, "ParamAngleX", t > 0.7f ? 30.0f * sinf((t - 0.7f) / 0.3f * 3.14159265f) : 0.0f);
    engine_update(e, 1000.0f / 30.0f, NULL, NULL);
}
