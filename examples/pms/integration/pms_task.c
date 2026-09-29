/* The PMS cyclic task: read, step, write, once per PMS_PERIOD_MS.
 *
 *   pms_mission              run as fast as possible (tests, CI)
 *   pms_mission --realtime   one step every PMS_PERIOD_MS, as on the vehicle
 *
 * On the flight computer this loop is the RTOS task (or the timer
 * interrupt) that owns the PMS; the platform functions are its drivers.
 */
#define _POSIX_C_SOURCE 200809L
#include <string.h>
#include <time.h>

#include "pms_platform.h"

static void wait_next_period(struct timespec* next) {
    next->tv_nsec += (long)PMS_PERIOD_MS * 1000000L;
    while (next->tv_nsec >= 1000000000L) {
        next->tv_nsec -= 1000000000L;
        next->tv_sec += 1;
    }
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, next, NULL);
}

int main(int argc, char** argv) {
    bool realtime = argc > 1 && strcmp(argv[1], "--realtime") == 0;
    PMS_State state;
    PMS_Input in;
    PMS_Output out;
    struct timespec next;
    clock_gettime(CLOCK_MONOTONIC, &next);

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
