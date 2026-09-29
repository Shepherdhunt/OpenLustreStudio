/* The PMS on the flight computer: what the platform provides around the
 * generated code (out/code/clite/openlustre_generated.{h,c}, from
 * `openlustre emit-clite pms.wksc --root PMS`).
 *
 * The generated code is one pure step function over explicit state:
 *
 *     PMS_State st;  PMS_init(&st);
 *     every period:  PMS_step(&st, &in, &out);
 *
 * no allocation, no globals, no library calls — so it runs on anything with
 * a C11 compiler. The platform samples the sensors and operator commands
 * into an PMS_Input, calls PMS_step once per period, and drives the release
 * hooks from the PMS_Output. The timing constants of the model
 * (PULSE_CYCLES, VERIFY_CYCLES) count periods of PMS_PERIOD_MS.
 */
#ifndef PMS_PLATFORM_H
#define PMS_PLATFORM_H

#include <stdbool.h>
#include <stdint.h>

#include "openlustre_generated.h"

/* 100 Hz: a 3-cycle release pulse lasts 30 ms. */
#define PMS_PERIOD_MS 10u

/* Sample this period's inputs: hook sensors, tag readers, weight on wheels,
 * altitude, and the operator's commands. Returns false to stop the task. */
bool pms_platform_read(uint32_t cycle, PMS_Input* in);

/* Act on this period's outputs: drive the hook actuators (fire1..4) and
 * publish the inventory, balance and release state to the operator. */
void pms_platform_write(uint32_t cycle, const PMS_Input* in, const PMS_Output* out);

#endif
