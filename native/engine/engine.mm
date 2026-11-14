#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>
#import <CoreGraphics/CoreGraphics.h>
#import <AppKit/AppKit.h>
#import <QuartzCore/CAMetalLayer.h>
#import "SyphonServerBase.h"
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <filesystem>
#include <iterator>
#include <map>
#include <mutex>
#include <string>
#include <vector>
#include "engine.h"
#include "hv.h"

static const char metal_shaders[] = {
#embed "mtoon.metal"
    , 0};

struct metal_renderer;
bool metal_renderer_start(id<MTLDevice> device, const char* shader_source);
std::vector<uint8_t> renderer_texture_usage(hv_model* model);
metal_renderer* metal_renderer_create(hv_model* model, const std::vector<id<MTLTexture>>& srgb_textures, const std::vector<id<MTLTexture>>& linear_textures, const std::vector<id<MTLSamplerState>>& samplers, std::vector<id<MTLBuffer>>& statics);
void metal_renderer_destroy(metal_renderer* r, hv_model* model);
void metal_renderer_draw(metal_renderer* r, id<MTLCommandBuffer> command_buffer, id<MTLTexture> target, const float* view_projection, const float* eye, const float* light_direction, const float* light_color, const float* ambient, double seconds, uint32_t changed, id<MTLTexture> back, bool opaque);
bool metal_renderer_share(id<MTLCommandBuffer> command_buffer, id<MTLTexture> model, id<MTLTexture> back, id<MTLTexture> window_target, id<MTLTexture> share_target);

struct engine_gpu
{
    metal_renderer* own = NULL;
    id<MTLCommandBuffer> presented = nil;
};

struct target_gpu
{
    id<MTLTexture> color_texture = nil;
};

struct model_set
{
    std::vector<id<MTLTexture>> srgb;
    std::vector<id<MTLTexture>> linear;
    std::vector<id<MTLSamplerState>> samplers;
    std::vector<id<MTLBuffer>> statics;
    int users = 0;
};

#include "engine_common.inc"

static id<MTLDevice> device = nil;
static id<MTLCommandQueue> queue = nil;

static bool start_metal()
{
    device = MTLCreateSystemDefaultDevice();
    if (device == nil)
    {
        return false;
    }
    queue = [device newCommandQueue];
    gpu_name = std::string([[device name] UTF8String]);
    return metal_renderer_start(device, metal_shaders);
}

static bool decode_image(const hv_texture* source, std::vector<uint8_t>& pixels, size_t& width, size_t& height)
{
    NSData* data = [NSData dataWithBytesNoCopy:(void*)source->bytes length:source->length freeWhenDone:NO];
    CGImageSourceRef image_source = CGImageSourceCreateWithData((__bridge CFDataRef)data, NULL);
    if (image_source == NULL)
    {
        return false;
    }
    CGImageRef image = CGImageSourceCreateImageAtIndex(image_source, 0, NULL);
    CFRelease(image_source);
    if (image == NULL)
    {
        return false;
    }
    width = CGImageGetWidth(image);
    height = CGImageGetHeight(image);
    if (!declared_size(source, width, height))
    {
        CGImageRelease(image);
        return false;
    }
    pixels.assign(width * height * 4, 255);
    CGImageAlphaInfo alpha = CGImageGetAlphaInfo(image);
    size_t pixel_bits = CGImageGetBitsPerPixel(image);
    CGColorSpaceRef image_space = CGImageGetColorSpace(image);
    bool raw = CGImageGetBitsPerComponent(image) == 8 && (CGImageGetBitmapInfo(image) & kCGBitmapByteOrderMask) == kCGBitmapByteOrderDefault && image_space != NULL && CGColorSpaceGetModel(image_space) == kCGColorSpaceModelRGB &&
               ((pixel_bits == 32 && (alpha == kCGImageAlphaLast || alpha == kCGImageAlphaNoneSkipLast)) || (pixel_bits == 24 && alpha == kCGImageAlphaNone));
    CFDataRef decoded = raw ? CGDataProviderCopyData(CGImageGetDataProvider(image)) : NULL;
    if (decoded != NULL)
    {
        const uint8_t* bytes = CFDataGetBytePtr(decoded);
        size_t row_bytes = CGImageGetBytesPerRow(image);
        size_t step = pixel_bits / 8;
        for (size_t y = 0; y < height; y++)
        {
            const uint8_t* in = bytes + y * row_bytes;
            uint8_t* out = pixels.data() + y * width * 4;
            for (size_t x = 0; x < width; x++, in += step, out += 4)
            {
                out[0] = in[0];
                out[1] = in[1];
                out[2] = in[2];
                out[3] = alpha == kCGImageAlphaLast ? in[3] : 255;
            }
        }
        CFRelease(decoded);
        CGImageRelease(image);
        return true;
    }
    CGColorSpaceRef space = image_space != NULL && CGColorSpaceGetModel(image_space) == kCGColorSpaceModelRGB ? CGColorSpaceRetain(image_space) : CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef context = CGBitmapContextCreate(pixels.data(), width, height, 8, width * 4, space, kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
    CGColorSpaceRelease(space);
    if (context == NULL)
    {
        CGImageRelease(image);
        return false;
    }
    CGContextSetBlendMode(context, kCGBlendModeCopy);
    CGContextDrawImage(context, CGRectMake(0, 0, width, height), image);
    CGContextRelease(context);
    CGImageRelease(image);
    for (size_t p = 0; p < width * height; p++)
    {
        uint8_t* out = pixels.data() + p * 4;
        if (out[3] != 0 && out[3] != 255)
        {
            for (int channel = 0; channel < 3; channel++)
            {
                out[channel] = (uint8_t)std::min(255, (out[channel] * 255 + out[3] / 2) / out[3]);
            }
        }
    }
    return true;
}

static id<MTLTexture> upload_texture(const std::vector<uint8_t>& pixels, size_t width, size_t height, MTLPixelFormat format, id<MTLBlitCommandEncoder> blit)
{
    MTLTextureDescriptor* descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:format width:width height:height mipmapped:YES];
    descriptor.storageMode = MTLStorageModeShared;
    descriptor.usage = MTLTextureUsageShaderRead;
    id<MTLTexture> texture = [device newTextureWithDescriptor:descriptor];
    if (texture != nil)
    {
        [texture replaceRegion:MTLRegionMake2D(0, 0, width, height) mipmapLevel:0 withBytes:pixels.data() bytesPerRow:width * 4];
        [blit generateMipmapsForTexture:texture];
    }
    return texture;
}

static MTLSamplerAddressMode address_mode(int32_t wrap)
{
    if (wrap == 33071)
    {
        return MTLSamplerAddressModeClampToEdge;
    }
    return wrap == 33648 ? MTLSamplerAddressModeMirrorRepeat : MTLSamplerAddressModeRepeat;
}

static id<MTLSamplerState> make_sampler(const hv_texture* source)
{
    MTLSamplerDescriptor* descriptor = [MTLSamplerDescriptor new];
    int32_t min_filter = source->min_filter;
    descriptor.magFilter = source->mag_filter == 9728 ? MTLSamplerMinMagFilterNearest : MTLSamplerMinMagFilterLinear;
    descriptor.minFilter = min_filter == 9728 || min_filter == 9984 || min_filter == 9986 ? MTLSamplerMinMagFilterNearest : MTLSamplerMinMagFilterLinear;
    descriptor.mipFilter = min_filter == 9728 || min_filter == 9729 ? MTLSamplerMipFilterNotMipmapped : (min_filter == 9984 || min_filter == 9985 ? MTLSamplerMipFilterNearest : MTLSamplerMipFilterLinear);
    descriptor.sAddressMode = address_mode(source->wrap_s);
    descriptor.tAddressMode = address_mode(source->wrap_t);
    descriptor.maxAnisotropy = 4;
    return [device newSamplerStateWithDescriptor:descriptor];
}

static void engine_release(engine* e)
{
    metal_renderer_destroy(e->own, e->model);
    model_release(e);
    delete e;
}

engine* engine_create(const char* vrm_path, int width, int height, char* error, int error_capacity)
{
    @autoreleasepool
    {
        if (vrm_path == NULL || width <= 0 || height <= 0)
        {
            write_error(error, error_capacity, "bad engine arguments");
            return NULL;
        }
        static const bool started = start_metal();
        if (!started)
        {
            write_error(error, error_capacity, "metal unavailable");
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
        std::unique_lock<std::mutex> hold(model_cache_lock);
        model_set& set = model_cache[e->model_key];
        if (set.users == 0)
        {
            std::vector<uint8_t> usage = renderer_texture_usage(model);
            std::vector<id<MTLTexture>> srgb_textures(usage.size(), nil);
            std::vector<id<MTLTexture>> linear_textures(usage.size(), nil);
            std::vector<id<MTLSamplerState>> samplers(usage.size(), nil);
            id<MTLCommandBuffer> command_buffer = [queue commandBuffer];
            id<MTLBlitCommandEncoder> blit = [command_buffer blitCommandEncoder];
            std::vector<uint8_t> pixels;
            for (size_t index = 0; index < usage.size(); index++)
            {
                const hv_texture* source = hv_texture_at(model, (int32_t)index);
                size_t image_width = 0;
                size_t image_height = 0;
                samplers[index] = make_sampler(source);
                if (usage[index] == 0 || !decode_image(source, pixels, image_width, image_height))
                {
                    continue;
                }
                if (usage[index] & 1)
                {
                    srgb_textures[index] = upload_texture(pixels, image_width, image_height, MTLPixelFormatRGBA8Unorm_sRGB, blit);
                }
                if (usage[index] & 2)
                {
                    linear_textures[index] = upload_texture(pixels, image_width, image_height, MTLPixelFormatRGBA8Unorm, blit);
                }
            }
            [blit endEncoding];
            [command_buffer commit];
            [command_buffer waitUntilCompleted];
            set.srgb = std::move(srgb_textures);
            set.linear = std::move(linear_textures);
            set.samplers = std::move(samplers);
        }
        hv_textures_release(model);
        set.users++;
        e->own = metal_renderer_create(model, set.srgb, set.linear, set.samplers, set.statics);
        hold.unlock();
        if (e->own == NULL)
        {
            write_error(error, error_capacity, std::filesystem::u8path(vrm_path).filename().u8string() + ": gpu buffers failed");
            engine_release(e);
            return NULL;
        }
        hv_geometry_release(model);
        e->uv_animated = model_uv_animated(model);
        e->pending = hv_update(model, 0.0f) | HV_CHANGED_JOINTS | HV_CHANGED_MORPHS;
        frame_camera(e);
        return e;
    }
}

void engine_destroy(engine* e)
{
    @autoreleasepool
    {
        engine_release(e);
    }
}

static bool decode_background(const uint8_t* bytes, size_t length, const char* expected_type, int width, int height, std::vector<std::vector<uint8_t>>& frames, std::vector<float>& ms)
{
    @autoreleasepool
    {
        NSDictionary* formats = @{
            @"image/png": @[@"public.png", [NSData dataWithBytes:"IEND\xae\x42\x60\x82" length:8]],
            @"image/jpeg": @[@"public.jpeg", [NSData dataWithBytes:"\xff\xd9" length:2]],
            @"image/gif": @[@"com.compuserve.gif", [NSData dataWithBytes:";" length:1]],
        };
        NSArray* format = expected_type != NULL ? formats[[NSString stringWithUTF8String:expected_type]] : nil;
        NSData* trailer = format[1];
        NSData* data = [NSData dataWithBytes:bytes length:length];
        CGImageSourceRef source = CGImageSourceCreateWithData((__bridge CFDataRef)data, NULL);
        if (source == NULL)
        {
            return false;
        }
        size_t count = CGImageSourceGetCount(source);
        NSDictionary* first = count > 0 ? (__bridge_transfer NSDictionary*)CGImageSourceCopyPropertiesAtIndex(source, 0, NULL) : nil;
        NSNumber* source_width = first[(__bridge NSString*)kCGImagePropertyPixelWidth];
        NSNumber* source_height = first[(__bridge NSString*)kCGImagePropertyPixelHeight];
        bool ok = background_fits(count, source_width.unsignedLongLongValue, source_height.unsignedLongLongValue, width, height) && format != nil && [(__bridge NSString*)CGImageSourceGetType(source) isEqualToString:format[0]] && CGImageSourceGetStatus(source) == kCGImageStatusComplete && length >= trailer.length && memcmp(bytes + length - trailer.length, trailer.bytes, trailer.length) == 0;
        CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        for (size_t i = 0; ok && i < count; i++)
        {
            NSDictionary* properties = (__bridge_transfer NSDictionary*)CGImageSourceCopyPropertiesAtIndex(source, i, NULL);
            if (![properties[(__bridge NSString*)kCGImagePropertyPixelWidth] isEqual:source_width] || ![properties[(__bridge NSString*)kCGImagePropertyPixelHeight] isEqual:source_height])
            {
                ok = false;
                break;
            }
            CGImageRef image = CGImageSourceCreateImageAtIndex(source, i, NULL);
            if (image == NULL)
            {
                ok = false;
                break;
            }
            NSNumber* delay = nil;
            NSDictionary* gif = properties[(__bridge NSString*)kCGImagePropertyGIFDictionary];
            NSDictionary* png = properties[(__bridge NSString*)kCGImagePropertyPNGDictionary];
            NSDictionary* webp = properties[(__bridge NSString*)kCGImagePropertyWebPDictionary];
            if (gif != nil)
            {
                delay = gif[(__bridge NSString*)kCGImagePropertyGIFUnclampedDelayTime] ?: gif[(__bridge NSString*)kCGImagePropertyGIFDelayTime];
            }
            if (png != nil)
            {
                delay = png[(__bridge NSString*)kCGImagePropertyAPNGUnclampedDelayTime] ?: png[(__bridge NSString*)kCGImagePropertyAPNGDelayTime];
            }
            if (webp != nil)
            {
                delay = webp[(__bridge NSString*)kCGImagePropertyWebPUnclampedDelayTime] ?: webp[(__bridge NSString*)kCGImagePropertyWebPDelayTime];
            }
            float frame_ms = delay != nil ? [delay floatValue] * 1000.0f : 100.0f;
            if (frame_ms < 20.0f)
            {
                frame_ms = 100.0f;
            }
            double iw = CGImageGetWidth(image);
            double ih = CGImageGetHeight(image);
            double scale = fmax(width / iw, height / ih);
            double bw = ceil(iw * scale);
            double bh = ceil(ih * scale);
            std::vector<uint8_t> frame((size_t)width * height * 4);
            CGContextRef context = CGBitmapContextCreate(frame.data(), width, height, 8, width * 4, space, kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
            CGContextSetRGBFillColor(context, 0, 0, 0, 1);
            CGContextFillRect(context, CGRectMake(0, 0, width, height));
            CGContextSetInterpolationQuality(context, kCGInterpolationHigh);
            CGContextDrawImage(context, CGRectMake((width - bw) / 2, (height - bh) / 2, bw, bh), image);
            CGContextRelease(context);
            CGImageRelease(image);
            frames.push_back(std::move(frame));
            ms.push_back(frame_ms);
        }
        CGColorSpaceRelease(space);
        CFRelease(source);
        return ok;
    }
}

static void draw_model(engine* e, id<MTLCommandBuffer> command_buffer, id<MTLTexture> target, id<MTLTexture> back, bool opaque)
{
    float matrix[16];
    view_projection(e, (int)target.width, (int)target.height, matrix);
    metal_renderer_draw(e->own, command_buffer, target, matrix, e->eye, kLightDirection, kLightColor, kAmbientColor, e->clock_seconds, e->pending, back, opaque);
    e->pending = 0;
}

const uint8_t* engine_render(engine* e, int width, int height, float zoom, float x, float y, float rot, float background_clock_ms)
{
    size_target& t = target_at(e, width, height);
    bool scaled = width != e->width || height != e->height;
    const uint8_t* back = frame_at(e, scaled ? t.background_frames : e->background_frames, background_clock_ms);
    if (t.color_texture == nil)
    {
        MTLTextureDescriptor* color = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm_sRGB width:width height:height mipmapped:NO];
        color.storageMode = MTLStorageModeShared;
        color.usage = MTLTextureUsageRenderTarget;
        t.color_texture = [device newTextureWithDescriptor:color];
        if (t.color_texture == nil)
        {
            return NULL;
        }
        t.rgba.resize((size_t)width * height * 4);
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
    double waited_ms = 0;
    @autoreleasepool
    {
        if (e->pending != 0 && e->presented != nil)
        {
            [e->presented waitUntilCompleted];
            e->presented = nil;
        }
        id<MTLCommandBuffer> command_buffer = [queue commandBuffer];
        draw_model(e, command_buffer, t.color_texture, nil, false);
        [command_buffer commit];
        auto committed = std::chrono::steady_clock::now();
        [command_buffer waitUntilCompleted];
        hold.unlock();
        waited_ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - committed).count();
        e->gpu_ms = (command_buffer.GPUEndTime - command_buffer.GPUStartTime) * 1000.0;
        [t.color_texture getBytes:t.rgba.data() bytesPerRow:width * 4 fromRegion:MTLRegionMake2D(0, 0, width, height) mipmapLevel:0];
    }
    compose_rgba(t.rgba.data(), (size_t)width * height, t.rgba.data(), back);
    e->cpu_ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count() - waited_ms;
    return t.rgba.data();
}

@interface SyphonServerBase (SyphonSubclassing)
- (IOSurfaceRef)newSurfaceForWidth:(size_t)width height:(size_t)height options:(NSDictionary*)options;
- (void)publish;
@end

@interface hikari_window : NSWindow
@property(nonatomic) BOOL close_requested;
@end

@implementation hikari_window
- (void)performClose:(id)sender
{
    self.close_requested = YES;
}
- (BOOL)canBecomeKeyWindow
{
    return YES;
}
- (BOOL)canBecomeMainWindow
{
    return YES;
}
@end

struct window
{
    hikari_window* ns = nil;
    NSView* view = nil;
    CAMetalLayer* layer = nil;
    id monitor = nil;
    id close_observer = nil;
    id resign_observer = nil;
    NSTimer* timer = nil;
    engine_hook frame = NULL;
    void* context = NULL;
    int* poll_width = NULL;
    int* poll_height = NULL;
    bool closed = false;
    bool fullscreen = false;
    NSRect saved_frame = NSZeroRect;
    NSWindowStyleMask saved_style = 0;
    int width = 0;
    int height = 0;
    bool panning = false;
    float pan_x = 0;
    float pan_y = 0;
    float zoom = 1;
    float cursor_x = 0;
    float cursor_y = 0;
    bool turn_ccw = false;
    bool turn_cw = false;
    int keys = 0;
    id<MTLTexture> background_texture = nil;
    id<MTLCommandBuffer> last = nil;
    SyphonServerBase* share = nil;
    id<NSObject> share_activity = nil;
    id<MTLTexture> model_texture = nil;
    id<MTLTexture> share_texture = nil;
    dispatch_source_t share_tick = nil;
    dispatch_semaphore_t share_wake = nil;
    bool share_off = false;
    present_state shown;
};

static void backing_size(window* w, int* width, int* height)
{
    NSSize size = [w->view convertSizeToBacking:w->view.bounds.size];
    *width = (int)size.width;
    *height = (int)size.height;
}

static void window_mouse(window* w, NSEvent* event)
{
    NSSize size = w->view.bounds.size;
    NSPoint at = [w->view convertPoint:event.locationInWindow fromView:nil];
    if (size.width <= 0 || size.height <= 0)
    {
        return;
    }
    if (event.type == NSEventTypeLeftMouseDown || event.type == NSEventTypeLeftMouseUp)
    {
        w->panning = event.type == NSEventTypeLeftMouseDown && NSPointInRect(at, w->view.bounds);
        return;
    }
    if (event.type == NSEventTypeLeftMouseDragged)
    {
        if (w->panning)
        {
            w->pan_x += event.deltaX / size.width;
            w->pan_y -= event.deltaY / size.height;
        }
        return;
    }
    if (event.type == NSEventTypeScrollWheel)
    {
        w->zoom *= expf(event.scrollingDeltaY * (event.hasPreciseScrollingDeltas ? 0.005f : 0.1f));
    }
    else
    {
        w->zoom *= 1 + event.magnification;
    }
    w->cursor_x = 2 * at.x / size.width - 1;
    w->cursor_y = 2 * at.y / size.height - 1;
}

#include "window_input.inc"

static void toggle_fullscreen(window* w)
{
    if ((w->ns.styleMask & NSWindowStyleMaskFullScreen) != 0)
    {
        [w->ns toggleFullScreen:nil];
        return;
    }
    if (w->fullscreen)
    {
        [w->ns setStyleMask:w->saved_style];
        [w->ns setFrame:w->saved_frame display:YES];
        [NSApp setPresentationOptions:NSApplicationPresentationDefault];
        [w->ns makeKeyAndOrderFront:nil];
        w->fullscreen = false;
        return;
    }
    NSScreen* screen = w->ns.screen != nil ? w->ns.screen : [NSScreen mainScreen];
    w->saved_style = w->ns.styleMask;
    w->saved_frame = w->ns.frame;
    [NSApp setPresentationOptions:NSApplicationPresentationHideDock | NSApplicationPresentationHideMenuBar];
    [w->ns setStyleMask:NSWindowStyleMaskBorderless];
    [w->ns setFrame:screen.frame display:YES];
    [w->ns makeKeyAndOrderFront:nil];
    w->fullscreen = true;
}

window* window_open(const char* title, int width, int height)
{
    @autoreleasepool
    {
        if (![NSThread isMainThread] || width <= 0 || height <= 0)
        {
            return NULL;
        }
        static bool app_started = false;
        if (!app_started)
        {
            [NSApplication sharedApplication];
            [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
            [NSApp finishLaunching];
            app_started = true;
        }
        window* w = new window();
        w->ns = [[hikari_window alloc] initWithContentRect:NSMakeRect(0, 0, width, height) styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
        if (w->ns == nil)
        {
            delete w;
            return NULL;
        }
        w->ns.releasedWhenClosed = NO;
        w->ns.title = [NSString stringWithUTF8String:title];
        w->ns.collectionBehavior = NSWindowCollectionBehaviorFullScreenPrimary;
        w->layer = [CAMetalLayer layer];
        w->layer.pixelFormat = MTLPixelFormatBGRA8Unorm_sRGB;
        w->layer.framebufferOnly = YES;
        w->layer.maximumDrawableCount = 2;
        CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        w->layer.colorspace = space;
        CGColorSpaceRelease(space);
        w->view = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, width, height)];
        w->view.layer = w->layer;
        w->view.wantsLayer = YES;
        w->ns.contentView = w->view;
        [w->ns center];
        [w->ns makeKeyAndOrderFront:nil];
        [NSApp activateIgnoringOtherApps:YES];
        hikari_window* ns = w->ns;
        w->close_observer = [[NSNotificationCenter defaultCenter] addObserverForName:NSWindowWillCloseNotification object:ns queue:nil usingBlock:^(NSNotification* note) {
            w->closed = true;
        }];
        w->resign_observer = [[NSNotificationCenter defaultCenter] addObserverForName:NSWindowDidResignKeyNotification object:ns queue:nil usingBlock:^(NSNotification* note) {
            w->turn_ccw = false;
            w->turn_cw = false;
        }];
        NSEventMask mouse = NSEventMaskLeftMouseDown | NSEventMaskLeftMouseUp | NSEventMaskLeftMouseDragged | NSEventMaskScrollWheel | NSEventMaskMagnify;
        w->monitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown | NSEventMaskKeyUp | mouse handler:^NSEvent*(NSEvent* event) {
            if (event.window != ns)
            {
                return event;
            }
            if (event.type != NSEventTypeKeyDown && event.type != NSEventTypeKeyUp)
            {
                window_mouse(w, event);
                return event;
            }
            if (event.type == NSEventTypeKeyUp)
            {
                if (event.keyCode == 12 || event.keyCode == 14)
                {
                    w->keys |= event.keyCode == 12 ? 4 : 8;
                    return nil;
                }
                return event;
            }
            if (event.keyCode == 12 && (event.modifierFlags & NSEventModifierFlagCommand) != 0)
            {
                ns.close_requested = YES;
                return nil;
            }
            if (event.keyCode == 12 || event.keyCode == 14)
            {
                (event.keyCode == 12 ? w->turn_ccw : w->turn_cw) = true;
                w->keys &= event.keyCode == 12 ? ~4 : ~8;
                return nil;
            }
            if (event.type == NSEventTypeKeyDown && (event.keyCode == 49 || (event.keyCode == 1 && (event.modifierFlags & (NSEventModifierFlagCommand | NSEventModifierFlagControl)) != 0)))
            {
                w->keys |= event.keyCode == 49 ? 1 : 2;
                return nil;
            }
            bool option = (event.modifierFlags & NSEventModifierFlagOption) != 0;
            if ((event.keyCode == 36 || event.keyCode == 76) && option)
            {
                toggle_fullscreen(w);
                return nil;
            }
            if (event.keyCode == 53)
            {
                if (w->fullscreen || (ns.styleMask & NSWindowStyleMaskFullScreen) != 0)
                {
                    toggle_fullscreen(w);
                }
                else
                {
                    ns.close_requested = YES;
                }
                return nil;
            }
            return event;
        }];
        w->timer = [NSTimer timerWithTimeInterval:0.001 repeats:YES block:^(NSTimer* timer) {
            if (w->frame != NULL)
            {
                backing_size(w, w->poll_width, w->poll_height);
                w->frame(w->context);
            }
        }];
        [[NSRunLoop currentRunLoop] addTimer:w->timer forMode:NSEventTrackingRunLoopMode];
        return w;
    }
}

bool window_poll(window* w, int* width, int* height, engine_hook frame, void* context)
{
    @autoreleasepool
    {
        w->frame = frame;
        w->context = context;
        w->poll_width = width;
        w->poll_height = height;
        NSEvent* event = nil;
        while ((event = [NSApp nextEventMatchingMask:NSEventMaskAny untilDate:nil inMode:NSDefaultRunLoopMode dequeue:YES]) != nil)
        {
            [NSApp sendEvent:event];
        }
        w->frame = NULL;
        backing_size(w, width, height);
        return !w->closed && !w->ns.close_requested;
    }
}

int window_ask_save(window* w, const char* model)
{
    @autoreleasepool
    {
        if (w->closed)
        {
            return 0;
        }
        NSAlert* alert = [[NSAlert alloc] init];
        alert.messageText = [NSString stringWithFormat:@"Save changes to %s's view?", model];
        [alert addButtonWithTitle:@"Save"];
        [alert addButtonWithTitle:@"Don't Save"];
        [alert addButtonWithTitle:@"Cancel"];
        NSModalResponse response = [alert runModal];
        w->ns.close_requested = NO;
        [w->ns makeKeyAndOrderFront:nil];
        if (response == NSAlertFirstButtonReturn)
        {
            return 1;
        }
        if (response == NSAlertSecondButtonReturn)
        {
            return 0;
        }
        return -1;
    }
}

static void share_stop(window* w)
{
    if (w->share_tick != nil)
    {
        dispatch_source_cancel(w->share_tick);
        w->share_tick = nil;
    }
    if (w->share != nil)
    {
        [w->share stop];
        w->share = nil;
    }
    if (w->share_activity != nil)
    {
        [[NSProcessInfo processInfo] endActivity:w->share_activity];
        w->share_activity = nil;
    }
    w->model_texture = nil;
    w->share_texture = nil;
    w->share_off = true;
}

static bool share_ready(window* w, int width, int height)
{
    if (w->share == nil)
    {
        w->share = [[SyphonServerBase alloc] initWithName:@"hikari-vrm" options:nil];
        if (w->share != nil)
        {
            w->share_activity = [[NSProcessInfo processInfo] beginActivityWithOptions:NSActivityUserInitiatedAllowingIdleSystemSleep reason:@"hikari-vrm syphon"];
        }
    }
    if (w->share != nil && (w->model_texture == nil || (int)w->model_texture.width != width || (int)w->model_texture.height != height))
    {
        MTLTextureDescriptor* descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm_sRGB width:width height:height mipmapped:NO];
        descriptor.usage = MTLTextureUsageRenderTarget;
        IOSurfaceRef surface = [w->share newSurfaceForWidth:width height:height options:nil];
        w->share_texture = surface != NULL ? [device newTextureWithDescriptor:descriptor iosurface:surface plane:0] : nil;
        if (surface != NULL)
        {
            CFRelease(surface);
        }
        descriptor.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead;
        descriptor.storageMode = MTLStorageModePrivate;
        w->model_texture = [device newTextureWithDescriptor:descriptor];
    }
    if (w->share == nil || w->share_texture == nil || w->model_texture == nil)
    {
        NSLog(@"engine: syphon share failed, drawing the window only");
        share_stop(w);
        return false;
    }
    return true;
}

void window_close(window* w)
{
    @autoreleasepool
    {
        if (w->last != nil)
        {
            [w->last waitUntilCompleted];
        }
        [w->timer invalidate];
        share_stop(w);
        [NSEvent removeMonitor:w->monitor];
        [[NSNotificationCenter defaultCenter] removeObserver:w->close_observer];
        [[NSNotificationCenter defaultCenter] removeObserver:w->resign_observer];
        if (w->fullscreen)
        {
            [NSApp setPresentationOptions:NSApplicationPresentationDefault];
        }
        if (!w->closed)
        {
            [w->ns close];
        }
        delete w;
        NSEvent* event = nil;
        while ((event = [NSApp nextEventMatchingMask:NSEventMaskAny untilDate:nil inMode:NSDefaultRunLoopMode dequeue:YES]) != nil)
        {
            [NSApp sendEvent:event];
        }
    }
}

bool engine_present(engine* e, window* w, float background_clock_ms, float frame_seconds, bool share)
{
    @autoreleasepool
    {
        NSSize size = [w->view convertSizeToBacking:w->view.bounds.size];
        int width = (int)size.width;
        int height = (int)size.height;
        if (w->closed || width <= 0 || height <= 0 || device == nil)
        {
            return false;
        }
        share = share && !w->share_off;
        {
            std::lock_guard<std::mutex> hold(e->lock);
            if (present_unchanged(w->shown, e, frame_at(e, e->background_frames, background_clock_ms), share) && w->width == width && w->height == height)
            {
                return false;
            }
        }
        bool hidden = w->ns.miniaturized || (w->ns.occlusionState & NSWindowOcclusionStateVisible) == 0;
        if (hidden && !share)
        {
            [NSThread sleepForTimeInterval:frame_seconds];
            return false;
        }
        if (hidden && w->share_tick == nil)
        {
            dispatch_semaphore_t wake = dispatch_semaphore_create(0);
            w->share_wake = wake;
            w->share_tick = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, DISPATCH_TIMER_STRICT, dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0));
            dispatch_source_set_event_handler(w->share_tick, ^{
                dispatch_semaphore_signal(wake);
            });
            dispatch_source_set_timer(w->share_tick, DISPATCH_TIME_NOW, (uint64_t)(frame_seconds * 1e9), 0);
            dispatch_resume(w->share_tick);
        }
        if (!hidden && w->share_tick != nil)
        {
            dispatch_source_cancel(w->share_tick);
            w->share_tick = nil;
        }
        if (hidden)
        {
            dispatch_semaphore_wait(w->share_wake, DISPATCH_TIME_FOREVER);
        }
        if (w->last != nil)
        {
            [w->last waitUntilCompleted];
            w->last = nil;
        }
        w->layer.device = device;
        w->layer.displaySyncEnabled = YES;
        if (w->width != width || w->height != height)
        {
            w->layer.drawableSize = CGSizeMake(width, height);
            w->width = width;
            w->height = height;
        }
        id<CAMetalDrawable> drawable = hidden ? nil : [w->layer nextDrawable];
        std::lock_guard<std::mutex> hold(e->lock);
        const uint8_t* back = frame_at(e, e->background_frames, background_clock_ms);
        if (back != NULL)
        {
            if (w->background_texture == nil || (int)w->background_texture.width != e->width || (int)w->background_texture.height != e->height)
            {
                MTLTextureDescriptor* descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm_sRGB width:e->width height:e->height mipmapped:NO];
                descriptor.storageMode = MTLStorageModeShared;
                descriptor.usage = MTLTextureUsageShaderRead;
                w->background_texture = [device newTextureWithDescriptor:descriptor];
                w->shown.background_uploaded = NULL;
            }
            if (background_stale(w->shown, e, back))
            {
                [w->background_texture replaceRegion:MTLRegionMake2D(0, 0, e->width, e->height) mipmapLevel:0 withBytes:back bytesPerRow:e->width * 4];
            }
        }
        present_mark(w->shown, e, back, share);
        id<MTLCommandBuffer> command_buffer = [queue commandBuffer];
        e->presented = command_buffer;
        if (share && share_ready(w, width, height))
        {
            draw_model(e, command_buffer, w->model_texture, nil, false);
            if (metal_renderer_share(command_buffer, w->model_texture, back != NULL ? w->background_texture : nil, drawable.texture, w->share_texture))
            {
                SyphonServerBase* server = w->share;
                [command_buffer addCompletedHandler:^(id<MTLCommandBuffer> done) {
                    [server publish];
                }];
                if (drawable != nil)
                {
                    [command_buffer presentDrawable:drawable afterMinimumDuration:frame_seconds];
                }
                [command_buffer commit];
                w->last = command_buffer;
                return true;
            }
            share_stop(w);
        }
        if (drawable == nil)
        {
            [command_buffer commit];
            w->last = command_buffer;
            w->shown.presented = NULL;
            return false;
        }
        draw_model(e, command_buffer, drawable.texture, back != NULL ? w->background_texture : nil, true);
        [command_buffer presentDrawable:drawable afterMinimumDuration:frame_seconds];
        [command_buffer commit];
        w->last = command_buffer;
        return true;
    }
}
