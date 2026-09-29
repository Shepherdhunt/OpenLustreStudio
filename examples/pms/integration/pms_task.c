/* The PMS cyclic task: read, step, write, once per PMS_PERIOD_MS.
 *
 *   pms_mission              run as fast as possible (tests, CI)
 *   pms_mission --realtime   one step every PMS_PERIOD_MS, as on the vehicle
 *
 * On the flight computer this loop is the RTOS task (or the timer
 * interrupt) that owns the PMS; the platform functions are its drivers.
 */
#if !defined(_WIN32) && !defined(__APPLE__)
#define _POSIX_C_SOURCE 200809L
#endif
#include <stdint.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <time.h>
#endif

#include "pms_platform.h"

/* The period clock: milliseconds from a monotonic clock. The task sleeps
   until the next period's start (not for a fixed time), so it never
   drifts. On the flight computer, the RTOS's periodic timer replaces this. */
static uint64_t now_ms(void) {
#ifdef _WIN32
    return (uint64_t)GetTickCount64();
#else
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u;
#endif
}

static void sleep_ms(uint64_t ms) {
#ifdef _WIN32
    Sleep((DWORD)ms);
#else
    struct timespec d = { (time_t)(ms / 1000u), (long)(ms % 1000u) * 1000000L };
    nanosleep(&d, NULL);
#endif
}

static void wait_next_period(uint64_t* next) {
    *next += PMS_PERIOD_MS;
    uint64_t now = now_ms();
    if (*next > now) {
        sleep_ms(*next - now);
    }
}

int main(int argc, char** argv) {
    bool realtime = argc > 1 && strcmp(argv[1], "--realtime") == 0;
    PMS_State state;
    PMS_Input in;
    PMS_Output out;
    uint64_t next = now_ms();

    PMS_init(&state);
    for (uint32_t cycle = 0;; cycle++) {
        memset(&in, 0, sizeof in);
        if (!pms_platform_read(cycle, &in)) {
            break;
        }
        PMS_step(&state, &in, &out);
        pms_platform_write(cycle, &in, &out);
        if (realtime) {
            wait_next_period(&next);
        }
    }
    return 0;
}
