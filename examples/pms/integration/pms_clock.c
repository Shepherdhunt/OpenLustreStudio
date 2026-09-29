/* The period clock for macOS, Linux and Windows (see pms_clock.h). */
#if defined(_WIN32)
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#if !defined(__APPLE__)
#define _POSIX_C_SOURCE 200809L
#endif
#include <time.h>
#endif

#include "pms_clock.h"

uint64_t pms_clock_now_ms(void) {
#if defined(_WIN32)
    return (uint64_t)GetTickCount64();
#else
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u;
#endif
}

void pms_clock_sleep_ms(uint64_t ms) {
#if defined(_WIN32)
    Sleep((DWORD)ms);
#else
    struct timespec d = { (time_t)(ms / 1000u), (long)(ms % 1000u) * 1000000L };
    nanosleep(&d, NULL);
#endif
}
