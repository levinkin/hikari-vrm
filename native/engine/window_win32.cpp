#define VK_USE_PLATFORM_WIN32_KHR
#include <windows.h>
#include <windowsx.h>
#include <cmath>
#include <string>
#include "window_vk.h"

struct window
{
    HWND hwnd = NULL;
    bool open = true;
    bool fullscreen = false;
    engine_hook frame = NULL;
    void* context = NULL;
    int* poll_width = NULL;
    int* poll_height = NULL;
    int width = 0;
    int height = 0;
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
    WINDOWPLACEMENT placement = {sizeof(WINDOWPLACEMENT)};
};

static std::wstring widen(const char* text)
{
    int length = MultiByteToWideChar(CP_UTF8, 0, text, -1, NULL, 0);
    std::wstring wide(length > 0 ? length : 1, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, text, -1, &wide[0], length);
    return wide;
}

static void set_fullscreen(window* w, bool on)
{
    if (on == w->fullscreen)
    {
        return;
    }
    w->fullscreen = on;
    LONG_PTR style = GetWindowLongPtrW(w->hwnd, GWL_STYLE);
    if (on)
    {
        MONITORINFO monitor = {sizeof(MONITORINFO)};
        GetWindowPlacement(w->hwnd, &w->placement);
        GetMonitorInfoW(MonitorFromWindow(w->hwnd, MONITOR_DEFAULTTONEAREST), &monitor);
        SetWindowLongPtrW(w->hwnd, GWL_STYLE, style & ~WS_OVERLAPPEDWINDOW);
        SetWindowPos(w->hwnd, HWND_TOP, monitor.rcMonitor.left, monitor.rcMonitor.top, monitor.rcMonitor.right - monitor.rcMonitor.left, monitor.rcMonitor.bottom - monitor.rcMonitor.top, SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
    }
    else
    {
        SetWindowLongPtrW(w->hwnd, GWL_STYLE, style | WS_OVERLAPPEDWINDOW);
        SetWindowPlacement(w->hwnd, &w->placement);
        SetWindowPos(w->hwnd, NULL, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
    }
}

static LRESULT CALLBACK window_proc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_NCCREATE)
    {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)((CREATESTRUCTW*)lparam)->lpCreateParams);
    }
    window* w = (window*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if (w == NULL)
    {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    switch (message)
    {
    case WM_CLOSE:
        w->open = false;
        return 0;
    case WM_SIZE:
        w->width = LOWORD(lparam);
        w->height = HIWORD(lparam);
        return 0;
    case WM_ENTERSIZEMOVE:
        SetTimer(hwnd, 1, USER_TIMER_MINIMUM, NULL);
        return 0;
    case WM_EXITSIZEMOVE:
        KillTimer(hwnd, 1);
        return 0;
    case WM_TIMER:
        if (w->frame != NULL)
        {
            *w->poll_width = w->width;
            *w->poll_height = w->height;
            w->frame(w->context);
        }
        return 0;
    case WM_DPICHANGED:
    {
        RECT* suggested = (RECT*)lparam;
        SetWindowPos(hwnd, NULL, suggested->left, suggested->top, suggested->right - suggested->left, suggested->bottom - suggested->top, SWP_NOZORDER | SWP_NOACTIVATE);
        return 0;
    }
    case WM_SYSKEYDOWN:
        if (wparam == VK_RETURN && (lparam & (1 << 29)))
        {
            if (!(lparam & (1 << 30)))
            {
                set_fullscreen(w, !w->fullscreen);
            }
            return 0;
        }
        break;
    case WM_SYSCHAR:
        if (wparam == VK_RETURN)
        {
            return 0;
        }
        break;
    case WM_LBUTTONDOWN:
        SetCapture(hwnd);
        w->panning = true;
        w->drag_x = GET_X_LPARAM(lparam);
        w->drag_y = GET_Y_LPARAM(lparam);
        return 0;
    case WM_LBUTTONUP:
        ReleaseCapture();
        w->panning = false;
        return 0;
    case WM_MOUSEMOVE:
        if (w->panning && (wparam & MK_LBUTTON) && w->width > 0 && w->height > 0)
        {
            w->pan_x += (float)(GET_X_LPARAM(lparam) - w->drag_x) / w->width;
            w->pan_y -= (float)(GET_Y_LPARAM(lparam) - w->drag_y) / w->height;
            w->drag_x = GET_X_LPARAM(lparam);
            w->drag_y = GET_Y_LPARAM(lparam);
        }
        return 0;
    case WM_MOUSEWHEEL:
        if (w->width > 0 && w->height > 0)
        {
            POINT at = {GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
            ScreenToClient(hwnd, &at);
            w->zoom *= powf(1.1f, (float)GET_WHEEL_DELTA_WPARAM(wparam) / WHEEL_DELTA);
            w->cursor_x = 2.0f * at.x / w->width - 1;
            w->cursor_y = 1 - 2.0f * at.y / w->height;
        }
        return 0;
    case WM_KILLFOCUS:
        w->turn_ccw = false;
        w->turn_cw = false;
        break;
    case WM_KEYUP:
        if (wparam == 'Q' || wparam == 'E')
        {
            w->keys |= wparam == 'Q' ? 4 : 8;
            return 0;
        }
        break;
    case WM_KEYDOWN:
        if (wparam == 'Q' || wparam == 'E')
        {
            (wparam == 'Q' ? w->turn_ccw : w->turn_cw) = true;
            w->keys &= wparam == 'Q' ? ~4 : ~8;
            return 0;
        }
        if (wparam == VK_SPACE || (wparam == 'S' && GetKeyState(VK_CONTROL) < 0))
        {
            w->keys |= wparam == VK_SPACE ? 1 : 2;
            return 0;
        }
        if (wparam == VK_ESCAPE)
        {
            if (!w->fullscreen)
            {
                w->open = false;
            }
            set_fullscreen(w, false);
            return 0;
        }
        break;
    }
    return DefWindowProcW(hwnd, message, wparam, lparam);
}

window* window_open(const char* title, int width, int height)
{
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    HINSTANCE instance = GetModuleHandleW(NULL);
    WNDCLASSEXW window_class = {sizeof(WNDCLASSEXW)};
    window_class.style = CS_HREDRAW | CS_VREDRAW;
    window_class.lpfnWndProc = window_proc;
    window_class.hInstance = instance;
    window_class.hCursor = LoadCursorW(NULL, (LPCWSTR)IDC_ARROW);
    window_class.lpszClassName = L"hikari_window";
    RegisterClassExW(&window_class);
    std::wstring wide_title = widen(title);
    window* w = new window();
    w->hwnd = CreateWindowExW(0, L"hikari_window", wide_title.c_str(), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, width, height, NULL, NULL, instance, w);
    if (w->hwnd == NULL)
    {
        delete w;
        return NULL;
    }
    UINT dpi = GetDpiForWindow(w->hwnd);
    RECT frame = {0, 0, MulDiv(width, dpi, 96), MulDiv(height, dpi, 96)};
    AdjustWindowRectExForDpi(&frame, WS_OVERLAPPEDWINDOW, FALSE, 0, dpi);
    SetWindowPos(w->hwnd, NULL, 0, 0, frame.right - frame.left, frame.bottom - frame.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    ShowWindow(w->hwnd, SW_SHOWNORMAL);
    return w;
}

bool window_poll(window* w, int* width, int* height, engine_hook frame, void* context)
{
    w->frame = frame;
    w->context = context;
    w->poll_width = width;
    w->poll_height = height;
    MSG message;
    while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE))
    {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    w->frame = NULL;
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
    std::string text = std::string("Save changes to ") + model + "'s view?";
    std::wstring wide = widen(text.c_str());
    int answer = MessageBoxW(w->hwnd, wide.c_str(), L"hikari", MB_YESNOCANCEL | MB_ICONQUESTION);
    w->open = true;
    if (answer == IDYES)
    {
        return 1;
    }
    if (answer == IDNO)
    {
        return 0;
    }
    return -1;
}

void window_close(window* w)
{
    window_release_target(w);
    DestroyWindow(w->hwnd);
    delete w;
}

const char* window_surface_extension()
{
    return VK_KHR_WIN32_SURFACE_EXTENSION_NAME;
}

VkSurfaceKHR window_surface(window* w, VkInstance instance)
{
    VkWin32SurfaceCreateInfoKHR info{};
    info.sType = VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR;
    info.hinstance = GetModuleHandleW(NULL);
    info.hwnd = w->hwnd;
    VkSurfaceKHR surface = VK_NULL_HANDLE;
    vkCreateWin32SurfaceKHR(instance, &info, NULL, &surface);
    return surface;
}
