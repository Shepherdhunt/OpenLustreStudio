/* The period clock for macOS, Linux and Windows. */
#ifndef PNT_CLOCK_H
#define PNT_CLOCK_H

#include <stdint.h>

uint64_t pnt_clock_now_ms(void);
void pnt_clock_sleep_ms(uint64_t ms);

#endif
