/* A scripted mission for the PNT manager on a desktop: the platform side
 * of pnt_platform.h, with a simulated vehicle, GNSS receiver, IMU and
 * barometer instead of drivers.
 *
 * The vehicle flies at (1, 0.4) m/s climbing 0.1 m/s from (100, 50) m;
 * the sensors are consistent with its motion. Over 60 s the mission
 * exercises every PNT behaviour: a cold start (the filter snaps to the
 * first fix), a 3 s GNSS dropout (inside the coast budget), a receiver
 * that lies by 300 m for 3 s (the innovation gate rejects it), an IMU
 * self-test failure (the filter pauses in Hold), a receiver whose
 * ALTITUDE lies while the barometer stays truthful (the vertical gate
 * rejects it; the horizontal solution is untouched), a barometer outage
 * (the vertical falls back to GNSS altitude), and an 8 s outage (past
 * the budget: coast, timeout, hold, re-acquisition). Every change is
 * logged, one line per event, so the run reads as a flight log and CI
 * compares it with expected_mission.txt.
 */
#include <stdio.h>
#include <string.h>

#include "pnt_platform.h"

/* 10 cycles a second (PNT_PERIOD_MS). */
#define AT(seconds) ((uint32_t)((seconds) * 10))
#define END_CYCLE AT(62)

static const char* status_name(PvtStatus s) {
    static const char* names[] = { "invalid", "acquiring", "fused", "coasting", "hold" };
    return (unsigned)s < 5 ? names[s] : "?";
}

static const char* alert_name(Alert a) {
    static const char* names[] = { "none", "sensor", "timeout", "integrity" };
    return (unsigned)a < 4 ? names[a] : "?";
}

static const char* health_name(Health h) {
    static const char* names[] = { "ok", "suspect", "fault" };
    return (unsigned)h < 3 ? names[h] : "?";
}

/* The scripted world, per cycle. */
static bool gnss_on(uint32_t c) {
    return c >= AT(2)                       /* a 2 s cold start */
        && !(c >= AT(10) && c < AT(13))     /* a 3 s dropout */
        && !(c >= AT(50) && c < AT(58));    /* the long 8 s outage */
}
static bool gnss_lying(uint32_t c) {
    return c >= AT(20) && c < AT(23);       /* 300 m off, still "valid" */
}
static bool gnss_z_lying(uint32_t c) {
    return c >= AT(40) && c < AT(42);       /* altitude +200 m, baro truthful */
}
static bool imu_on(uint32_t c) {
    return !(c >= AT(30) && c < AT(35));    /* self-test failure */
}
static bool baro_on(uint32_t c) {
    return !(c >= AT(44) && c < AT(47));    /* barometer outage */
}

/* The vehicle: constant (1, 0.4) m/s climbing 0.1 m/s from (100, 50) m. */
static float truth_x(uint32_t c) { return 100.0f + 0.1f * (float)c; }
static float truth_y(uint32_t c) { return 50.0f + 0.04f * (float)c; }
static float truth_alt(uint32_t c) { return 50.0f + 0.01f * (float)c; }

static PNT_Output prev;
static bool have_prev;

bool pnt_platform_read(uint32_t cycle, PNT_Input* in) {
    bool fix = gnss_on(cycle);
    in->gnss_valid = fix;
    in->fix = fix ? Fix3D : NoFix;
    in->sats = fix ? 9 : 0;
    in->hdop = fix ? 150 : 0;
    in->gnss_x = truth_x(cycle) + (gnss_lying(cycle) ? 300.0f : 0.0f);
    in->gnss_y = truth_y(cycle);
    in->gnss_z = truth_alt(cycle) + (gnss_z_lying(cycle) ? 200.0f : 0.0f);
    in->gnss_time_valid = fix;
    in->gnss_tow_s = 3600 + (int32_t)(cycle / 10);
    in->imu_valid = imu_on(cycle);
    in->imu_selftest = imu_on(cycle);
    in->dvx = 0.1f;  /* the true motion; the model ignores it while the */
    in->dvy = 0.04f; /* IMU is unusable, and clamps it regardless       */
    in->dvz = 0.01f;
    in->baro_valid = baro_on(cycle);
    in->baro_selftest = baro_on(cycle);
    in->baro_alt = truth_alt(cycle);
    return cycle < END_CYCLE;
}

static void line(uint32_t cycle, const char* what) {
    printf("t=%6.2f s #%4u  %s\n", cycle / 10.0, cycle, what);
}

void pnt_platform_write(uint32_t cycle, const PNT_Input* in, const PNT_Output* out) {
    char buf[160];
    (void)in;
    if (!have_prev) {
        snprintf(buf, sizeof buf, "boot: sources %s/%s/%s",
                 health_name(out->gnss_health), health_name(out->imu_health),
                 health_name(out->baro_health));
        line(cycle, buf);
    } else {
        if (out->sol_status != prev.sol_status) {
            snprintf(buf, sizeof buf, "status: %s (x=%.1f, y=%.1f, alt=%.1f, tow=%u, prot=%.0f)",
                     status_name(out->sol_status), out->sol_x, out->sol_y, out->sol_alt,
                     (unsigned)out->sol_tow, out->protection_m);
            line(cycle, buf);
        }
        if (out->alert != prev.alert) {
            snprintf(buf, sizeof buf, "alert: %s", alert_name(out->alert));
            line(cycle, buf);
        }
        if (out->time_synced != prev.time_synced) {
            snprintf(buf, sizeof buf, "time: %s (tow=%u, holdover=%d)",
                     out->time_synced ? "synced" : "holdover",
                     (unsigned)out->sol_tow, out->holdover_s);
            line(cycle, buf);
        }
    }
    if (cycle + 1 == END_CYCLE) {
        snprintf(buf, sizeof buf, "mission end: %s, %s",
                 status_name(out->sol_status), alert_name(out->alert));
        line(cycle, buf);
    }
    prev = *out;
    have_prev = true;
}
