/* The SMS on the flight computer: what the platform provides around the
 * generated code (out/code/clite/openlustre_generated.{h,c}, from
 * `openlustre emit-clite sms.wksc --root SMS`).
 *
 * The generated code is one pure step function over explicit state:
 *
 *     SMS_State st;  SMS_init(&st);
 *     every period:  SMS_step(&st, &in, &out);
 *
 * no allocation, no globals, no library calls — so it runs on anything with
 * a C11 compiler. The platform samples the sensors and operator commands
 * into an SMS_Input, calls SMS_step once per period, and drives the release
 * hooks from the SMS_Output. The timing constants of the model
 * (PULSE_CYCLES, VERIFY_CYCLES) count periods of SMS_PERIOD_MS.
 */
#ifndef SMS_PLATFORM_H
#define SMS_PLATFORM_H

#include <stdbool.h>
#include <stdint.h>

#include "openlustre_generated.h"

/* 100 Hz: a 3-cycle release pulse lasts 30 ms. */
#define SMS_PERIOD_MS 10u

/* Sample this period's inputs: hook sensors, tag readers, weight on wheels,
 * altitude, and the operator's commands. Returns false to stop the task. */
bool sms_platform_read(uint32_t cycle, SMS_Input* in);

/* Act on this period's outputs: drive the hook actuators (fire1..4) and
 * publish the inventory, balance and release state to the operator. */
void sms_platform_write(uint32_t cycle, const SMS_Input* in, const SMS_Output* out);

#endif
