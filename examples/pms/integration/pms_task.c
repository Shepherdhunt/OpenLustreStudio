/* The PMS cyclic task: read, step, write, once per PMS_PERIOD_MS.
 *
 *   pms_mission              run as fast as possible (tests, CI)
 *   pms_mission --realtime   one step every PMS_PERIOD_MS, as on the vehicle
 *
 * On the flight computer this loop is the RTOS task (or the timer
 * interrupt) that owns the PMS; the platform functions are its drivers and
 * the RTOS's timer replaces pms_clock.c.
 */
#include <stdint.h>
#include <string.h>

#include "pms_clock.h"
#include "pms_platform.h"

/* Sleep until the next period's start (not for a fixed time), so the task
   never drifts. */
static void wait_next_period(uint64_t* next) {
    *next += PMS_PERIOD_MS;
    uint64_t now = pms_clock_now_ms();
    if (*next > now) {
        pms_clock_sleep_ms(*next - now);
    }
}

int main(int argc, char** argv) {
    bool realtime = argc > 1 && strcmp(argv[1], "--realtime") == 0;
    PMS_State state;
    PMS_Input in;
    PMS_Output out;
    uint64_t next = pms_clock_now_ms();

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
