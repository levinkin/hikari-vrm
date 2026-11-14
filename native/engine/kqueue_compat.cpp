#include <chrono>
#include <cstdint>
#include <thread>
#ifdef _WIN32
#include <windows.h>
#endif

struct timer_timeout
{
    int64_t tv_sec;
    int64_t tv_nsec;
};

extern "C" int kqueue()
{
    return 0;
}

extern "C" int kevent(int queue, const void* changes, int change_count, void* events, int event_count, const timer_timeout* timeout)
{
    if (timeout == NULL)
    {
        return 0;
    }
    int64_t ns = timeout->tv_sec * 1000000000 + timeout->tv_nsec;
#ifdef _WIN32
    HANDLE timer = CreateWaitableTimerExW(NULL, NULL, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
    LARGE_INTEGER due;
    due.QuadPart = -(ns / 100);
    SetWaitableTimer(timer, &due, 0, NULL, NULL, FALSE);
    WaitForSingleObject(timer, INFINITE);
    CloseHandle(timer);
#else
    std::this_thread::sleep_for(std::chrono::nanoseconds(ns));
#endif
    return 0;
}
