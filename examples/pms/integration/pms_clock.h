/* The period clock of the PMS task: a monotonic millisecond clock and a
 * sleep. Kept apart from the generated code so that no platform header
 * (windows.h defines names such as `Unknown`) meets the model's names. On
 * the flight computer, the RTOS's periodic timer replaces this file. */
#ifndef PMS_CLOCK_H
#define PMS_CLOCK_H

#include <stdint.h>

uint64_t pms_clock_now_ms(void);
void pms_clock_sleep_ms(uint64_t ms);

#endif
