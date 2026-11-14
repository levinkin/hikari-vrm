#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <string>
#include <vector>
#include "engine.h"

#include "selftest_common.inc"

static bool write_ppm(const uint8_t* rgba, const std::string& path)
{
    std::ofstream file(path, std::ios::binary);
    file << "P6\n" << kWidth << " " << kHeight << "\n255\n";
    for (int p = 0; p < kWidth * kHeight; p++)
    {
        file.write((const char*)rgba + p * 4, 3);
    }
    return file.good();
}

int main(int argc, char** argv)
{
    if (argc < 3)
    {
        fprintf(stderr, "usage: engine_selftest model.vrm out_dir [frames]\n");
        return 2;
    }
    char* frames_end = NULL;
    long frames = argc > 3 ? strtol(argv[3], &frames_end, 10) : 60;
    if ((argc > 3 && *frames_end != 0) || frames < 1 || frames > 100000)
    {
        fprintf(stderr, "frames must be 1..100000\n");
        return 2;
    }
    std::string out_dir = argv[2];
    char error[512] = {};
    engine* e = engine_create(argv[1], kWidth, kHeight, error, sizeof(error));
    if (e == NULL)
    {
        fprintf(stderr, "engine_create failed: %s\n", error);
        return 1;
    }
    printf("gpu=%s params=%d\n", engine_gpu_name(e), engine_param_count(e));
    if (engine_has_param(e, "NoSuchParam") || !engine_has_param(e, "ParamEyeLOpen"))
    {
        fprintf(stderr, "param lookup wrong\n");
        return 1;
    }
    std::vector<double> gpu;
    std::vector<double> cpu;
    long lit = 0;
    bool written = true;
    for (int frame = 0; frame < frames; frame++)
    {
        auto started = std::chrono::steady_clock::now();
        animate(e, frame, frames);
        double update_ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count();
        const uint8_t* rgba = engine_render(e, kWidth, kHeight, 1, 0, 0, 0, frame * 1000.0f / 30.0f);
        if (rgba == NULL)
        {
            fprintf(stderr, "render failed at frame %d\n", frame);
            engine_destroy(e);
            return 1;
        }
        double gpu_ms = 0;
        double cpu_ms = 0;
        engine_frame_ms(e, &gpu_ms, &cpu_ms);
        gpu.push_back(gpu_ms);
        cpu.push_back(cpu_ms + update_ms);
        printf("frame=%d gpu_ms=%.3f cpu_ms=%.3f update_ms=%.3f\n", frame, gpu_ms, cpu_ms + update_ms, update_ms);
        char name[64];
        snprintf(name, sizeof(name), "/frame_%03d.ppm", frame);
        written = write_ppm(rgba, out_dir + name) && written;
        if (frame == 0)
        {
            for (int p = 0; p < kWidth * kHeight; p++)
            {
                lit += rgba[p * 4] + rgba[p * 4 + 1] + rgba[p * 4 + 2] > 60;
            }
        }
    }
    printf("gpu_ms p50=%.3f p90=%.3f cpu_ms p50=%.3f p90=%.3f lit_pixels=%ld\n", percentile(gpu, 0.5), percentile(gpu, 0.9), percentile(cpu, 0.5), percentile(cpu, 0.9), lit);
    std::vector<uint8_t> white = {'G', 'I', 'F', '8', '9', 'a', 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, ';'};
    int background_frames = engine_background_load(e, white.data(), white.size(), "image/gif");
    int hostile_frames = hostile_background_frames(e);
    const uint8_t* with_background = engine_render(e, kWidth, kHeight, 1, 0, 0, 0, 0);
    written = with_background != NULL && write_ppm(with_background, out_dir + "/background.ppm") && written;
    engine_background_clear(e);
    engine_destroy(e);
    printf("background_frames=%d hostile_frames=%d\n", background_frames, hostile_frames);
    if (!written || lit < 1000 || background_frames != 1 || hostile_frames != 0)
    {
        fprintf(stderr, "selftest failed written=%d lit=%ld background_frames=%d hostile_frames=%d\n", written, lit, background_frames, hostile_frames);
        return 1;
    }
    printf("selftest ok %s\n", out_dir.c_str());
    return 0;
}
