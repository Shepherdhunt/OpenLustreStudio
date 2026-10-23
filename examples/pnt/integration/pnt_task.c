/* The PNT cyclic task: read, step, write, once per PNT_PERIOD_MS.
 *
 *   pnt_mission              run as fast as possible (tests, CI)
 *   pnt_mission --realtime   one step every PNT_PERIOD_MS, as on the vehicle
 *
 * On the flight computer this loop is the RTOS task (or the timer
 * interrupt) that owns the PNT manager; the platform functions are its
 * drivers and the RTOS's timer replaces pnt_clock.c.
 */
#include <stdint.h>
#include <string.h>

#include "pnt_clock.h"
#include "pnt_platform.h"

/* Sleep until the next period's start (not for a fixed time), so the task
   never drifts. */
static void wait_next_period(uint64_t* next) {
    *next += PNT_PERIOD_MS;
    uint64_t now = pnt_clock_now_ms();
    if (*next > now) {
        pnt_clock_sleep_ms(*next - now);
    }
}

int main(int argc, char** argv) {
    bool realtime = argc > 1 && strcmp(argv[1], "--realtime") == 0;
    PNT_State state;
    PNT_Input in;
    PNT_Output out;
    uint64_t next = pnt_clock_now_ms();

    PNT_init(&state);
    for (uint32_t cycle = 0;; cycle++) {
        memset(&in, 0, sizeof in);
        if (!pnt_platform_read(cycle, &in)) {
            break;
        }
        PNT_step(&state, &in, &out);
        pnt_platform_write(cycle, &in, &out);
        if (realtime) {
            wait_next_period(&next);
        }
    }
    return 0;
}
