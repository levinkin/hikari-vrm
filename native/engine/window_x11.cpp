#define VK_USE_PLATFORM_XLIB_KHR
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <X11/keysym.h>
#include <cstdio>
#include <cstdlib>
#include <unistd.h>
#include "window_vk.h"

struct window
{
    Display* display;
    Window handle;
    Atom wm_delete;
    Atom net_wm_state;
    Atom net_wm_state_fullscreen;
    int width;
    int height;
    bool open;
    bool panning = false;
    int drag_x = 0;
    int drag_y = 0;
    float pan_x = 0;
    float pan_y = 0;
    float zoom = 1;
    float cursor_x = 0;
    float cursor_y = 0;
    bool turn_ccw = false;
    bool turn_cw = false;
    int keys = 0;
};

static bool window_fullscreen(window* w)
{
    Atom type = 0;
    int format = 0;
    unsigned long count = 0;
    unsigned long remaining = 0;
    unsigned char* data = nullptr;
    bool found = false;
    if (XGetWindowProperty(w->display, w->handle, w->net_wm_state, 0, 64, False, XA_ATOM, &type, &format, &count, &remaining, &data) == Success && data) {
        Atom* atoms = reinterpret_cast<Atom*>(data);
        for (unsigned long i = 0; i < count; i++) {
            found = found || atoms[i] == w->net_wm_state_fullscreen;
        }
        XFree(data);
    }
    return found;
}

static void window_fullscreen_set(window* w, bool on)
{
    XEvent event = {};
    event.xclient.type = ClientMessage;
    event.xclient.window = w->handle;
    event.xclient.message_type = w->net_wm_state;
    event.xclient.format = 32;
    event.xclient.data.l[0] = on ? 1 : 0;
    event.xclient.data.l[1] = static_cast<long>(w->net_wm_state_fullscreen);
    event.xclient.data.l[2] = 0;
    event.xclient.data.l[3] = 1;
    XSendEvent(w->display, DefaultRootWindow(w->display), False, SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(w->display);
}

window* window_open(const char* title, int width, int height)
{
    Display* display = XOpenDisplay(nullptr);
    if (!display) {
        return nullptr;
    }
    int screen = DefaultScreen(display);
    XSetWindowAttributes attributes = {};
    attributes.background_pixel = BlackPixel(display, screen);
    attributes.event_mask = StructureNotifyMask | KeyPressMask | ButtonPressMask | ButtonReleaseMask | Button1MotionMask | KeyReleaseMask | FocusChangeMask;
    Window handle = XCreateWindow(display, RootWindow(display, screen), 0, 0, static_cast<unsigned>(width), static_cast<unsigned>(height), 0, CopyFromParent, InputOutput, CopyFromParent, CWBackPixel | CWEventMask, &attributes);
    if (!handle) {
        XCloseDisplay(display);
        return nullptr;
    }
    window* w = new window{display, handle, XInternAtom(display, "WM_DELETE_WINDOW", False), XInternAtom(display, "_NET_WM_STATE", False), XInternAtom(display, "_NET_WM_STATE_FULLSCREEN", False), width, height, true};
    Xutf8SetWMProperties(display, handle, title, title, nullptr, 0, nullptr, nullptr, nullptr);
    XSetWMProtocols(display, handle, &w->wm_delete, 1);
    XMapWindow(display, handle);
    XFlush(display);
    return w;
}

bool window_poll(window* w, int* width, int* height, engine_hook frame, void* context)
{
    while (XPending(w->display)) {
        XEvent event;
        XNextEvent(w->display, &event);
        if (event.type == ConfigureNotify) {
            w->width = event.xconfigure.width;
            w->height = event.xconfigure.height;
        } else if (event.type == ClientMessage && static_cast<Atom>(event.xclient.data.l[0]) == w->wm_delete) {
            w->open = false;
        } else if (event.type == DestroyNotify) {
            w->open = false;
            w->handle = 0;
        } else if (event.type == ButtonPress && event.xbutton.button == Button1) {
            w->panning = true;
            w->drag_x = event.xbutton.x;
            w->drag_y = event.xbutton.y;
        } else if (event.type == ButtonRelease && event.xbutton.button == Button1) {
            w->panning = false;
        } else if (event.type == MotionNotify && w->panning && w->width > 0 && w->height > 0) {
            w->pan_x += static_cast<float>(event.xmotion.x - w->drag_x) / w->width;
            w->pan_y -= static_cast<float>(event.xmotion.y - w->drag_y) / w->height;
            w->drag_x = event.xmotion.x;
            w->drag_y = event.xmotion.y;
        } else if (event.type == ButtonPress && (event.xbutton.button == Button4 || event.xbutton.button == Button5) && w->width > 0 && w->height > 0) {
            w->zoom *= event.xbutton.button == Button4 ? 1.1f : 1 / 1.1f;
            w->cursor_x = 2.0f * event.xbutton.x / w->width - 1;
            w->cursor_y = 1 - 2.0f * event.xbutton.y / w->height;
        } else if (event.type == FocusOut) {
            w->turn_ccw = false;
            w->turn_cw = false;
        } else if (event.type == KeyRelease) {
            XEvent next = {};
            if (XEventsQueued(w->display, QueuedAfterReading)) {
                XPeekEvent(w->display, &next);
            }
            if (next.type == KeyPress && next.xkey.time == event.xkey.time && next.xkey.keycode == event.xkey.keycode) {
                XNextEvent(w->display, &next);
                continue;
            }
            KeySym key = XLookupKeysym(&event.xkey, 0);
            if (key == XK_q || key == XK_e) {
                w->keys |= key == XK_q ? 4 : 8;
            }
        } else if (event.type == KeyPress) {
            KeySym key = XLookupKeysym(&event.xkey, 0);
            if (key == XK_q || key == XK_e) {
                (key == XK_q ? w->turn_ccw : w->turn_cw) = true;
                w->keys &= key == XK_q ? ~4 : ~8;
            } else if (key == XK_space) {
                w->keys |= 1;
            } else if (key == XK_s && (event.xkey.state & ControlMask)) {
                w->keys |= 2;
            }
            if ((key == XK_Return || key == XK_KP_Enter) && (event.xkey.state & Mod1Mask)) {
                window_fullscreen_set(w, !window_fullscreen(w));
            } else if (key == XK_Escape && window_fullscreen(w)) {
                window_fullscreen_set(w, false);
            } else if (key == XK_Escape) {
                w->open = false;
            }
        }
    }
    *width = w->width;
    *height = w->height;
    return w->open;
}

void window_size(window* w, int* width, int* height)
{
    *width = w->width;
    *height = w->height;
}

#include "window_input.inc"

int window_ask_save(window* w, const char* model)
{
    if (isatty(STDIN_FILENO))
    {
        fprintf(stderr, "save view for %s? [y/n] ", model);
        char* answer = NULL;
        size_t capacity = 0;
        ssize_t length = getline(&answer, &capacity, stdin);
        bool save = length > 0 && (answer[0] == 'y' || answer[0] == 'Y');
        free(answer);
        if (length >= 0)
        {
            return save;
        }
    }
    fprintf(stderr, "hikari: closed with unsaved view for %s\n", model);
    return 0;
}

void window_close(window* w)
{
    if (!w) {
        return;
    }
    window_release_target(w);
    if (w->handle) {
        XDestroyWindow(w->display, w->handle);
    }
    XCloseDisplay(w->display);
    delete w;
}

const char* window_surface_extension()
{
    return VK_KHR_XLIB_SURFACE_EXTENSION_NAME;
}

VkSurfaceKHR window_surface(window* w, VkInstance instance)
{
    VkXlibSurfaceCreateInfoKHR info = {};
    info.sType = VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR;
    info.dpy = w->display;
    info.window = w->handle;
    VkSurfaceKHR surface = VK_NULL_HANDLE;
    if (vkCreateXlibSurfaceKHR(instance, &info, nullptr, &surface) != VK_SUCCESS) {
        return VK_NULL_HANDLE;
    }
    return surface;
}
