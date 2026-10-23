/* The PNT manager on the flight computer: what the platform provides
 * around the generated code (out/code/clite/openlustre_generated.{h,c},
 * from `openlustre emit-clite pnt.wksc --root PNT`).
 *
 * The generated code is one pure step function over explicit state:
 *
 *     PNT_State st;  PNT_init(&st);
 *     every period:  PNT_step(&st, &in, &out);
 *
 * no allocation, no globals, no library calls — so it runs on anything
 * with a C11 compiler. The platform samples the sensor plugins (the
 * GNSS receiver and the IMU) into a PNT_Input, calls PNT_step once per
 * period, and publishes the PVT solution from the PNT_Output. The
 * timing constants of the model (CONVERGE_CYCLES, COAST_BUDGET_CYCLES,
 * DRIFT_WINDOW_CYCLES) count periods of PNT_PERIOD_MS.
 */
#ifndef PNT_PLATFORM_H
#define PNT_PLATFORM_H

#include <stdbool.h>
#include <stdint.h>

#include "openlustre_generated.h"

/* 10 Hz: the base clock of the model. */
#define PNT_PERIOD_MS 100u

/* Sample this period's plugin reports: the GNSS receiver (validity, fix
 * type, satellites, HDOP, position, time) and the IMU (validity,
 * self-test, velocity increments over the period). Returns false to stop
 * the task. */
bool pnt_platform_read(uint32_t cycle, PNT_Input* in);

/* Act on this period's outputs: publish the PVT solution, its status,
 * protection level and alerts. */
void pnt_platform_write(uint32_t cycle, const PNT_Input* in, const PNT_Output* out);

#endif
