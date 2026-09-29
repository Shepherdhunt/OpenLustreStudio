/* A scripted mission for the PMS on a desktop: the platform side of
 * pms_platform.h, with a simulated vehicle and operator instead of drivers.
 *
 * Four stores are loaded on the ground (hook 4's release mechanism is
 * jammed); the drone arms, takes off and climbs at 3 m/s to 30 m; the
 * operator asks for stores — too early, then in turn — and finally
 * jettisons the rest before landing: 50 s of flight, 5000 PMS cycles.
 * Every change the PMS makes is logged, one line per event, so the run
 * reads as a flight log and CI compares it with expected_mission.txt.
 *
 * The simulated hooks: a store leaves the cycle after its hook is fired
 * (the tag goes with it), except on a jammed hook, which only lets go under
 * an emergency jettison.
 */
#include <stdio.h>
#include <string.h>

#include "pms_platform.h"

/* 100 cycles a second (PMS_PERIOD_MS). */
#define AT(seconds) ((uint32_t)((seconds) * 100))
#define END_CYCLE AT(50)

/* Catalogue tag codes (see StationDecode in the model). */
enum { TAG_EMPTY = 0, TAG_MEDKIT = 1, TAG_SENSORPOD = 2, TAG_WATERPACK = 3, TAG_SUPPLYCRATE = 4 };

static bool present[5] = { false, true, true, true, true };
static int32_t tag[5] = { TAG_EMPTY, TAG_MEDKIT, TAG_MEDKIT, TAG_WATERPACK, TAG_SENSORPOD };
static const bool jammed[5] = { false, false, false, false, true };
static bool leaving[5];

static const char* event;   /* what the script did this cycle, if anything */
static bool requested;      /* a release request was made this cycle */
static PMS_Output prev;
static bool have_prev;

static const char* kind_name(StoreKind k) {
    static const char* names[] = { "Empty", "MedKit", "SensorPod", "WaterPack", "SupplyCrate", "Unknown" };
    return (unsigned)k < 6 ? names[k] : "?";
}

static const char* status_name(StationStatus s) {
    static const char* names[] = { "Vacant", "Loaded", "Mismatch", "Hung" };
    return (unsigned)s < 4 ? names[s] : "?";
}

static const char* phase_name(SeqPhase p) {
    static const char* names[] = { "PhSafe", "PhReady", "PhFiring", "PhJettison", "PhVerify" };
    return (unsigned)p < 5 ? names[p] : "?";
}

static const char* inhibit_name(Inhibit i) {
    static const char* names[] = { "Clear", "NotArmed", "OnGround", "StationFault", "LowAltitude",
                                   "NoMatchingStore", "WouldUnbalance" };
    return (unsigned)i < 7 ? names[i] : "?";
}

/* `1-3-` for hooks 1 and 3. */
static const char* hooks(bool a, bool b, bool c, bool d) {
    static char s[5];
    s[0] = a ? '1' : '-';
    s[1] = b ? '2' : '-';
    s[2] = c ? '3' : '-';
    s[3] = d ? '4' : '-';
    s[4] = '\0';
    return s;
}

/* Metres: take off at 3 s, climb at 3 m/s to 30 m, descend from 35 s. */
static int32_t altitude(uint32_t c) {
    if (c < AT(3)) return 0;
    if (c < AT(13)) return (int32_t)(c - AT(3)) * 3 / 100;
    if (c < AT(35)) return 30;
    if (c < AT(45)) return 30 - (int32_t)(c - AT(35)) * 3 / 100;
    return 0;
}

bool pms_platform_read(uint32_t cycle, PMS_Input* in) {
    if (cycle >= END_CYCLE) {
        return false;
    }
    /* The hooks fired last cycle let their stores go. */
    for (int k = 1; k <= 4; k++) {
        if (leaving[k]) {
            present[k] = false;
            tag[k] = TAG_EMPTY;
            leaving[k] = false;
        }
    }
    event = NULL;
    requested = false;
    in->master_arm = cycle >= AT(1) && cycle < AT(46);
    in->wow = cycle < AT(3) || cycle >= AT(45);
    in->alt_m = altitude(cycle);
    in->req_kind = Empty;
    if (cycle == AT(1)) event = "operator: master arm on";
    if (cycle == AT(3)) event = "vehicle: takeoff";
    if (cycle == AT(45)) event = "vehicle: landed";
    if (cycle == AT(46)) event = "operator: master arm off";
    /* Release requests: a one-cycle press of the release button. */
    struct { uint32_t at; StoreKind kind; } requests[] = {
        { AT(2), MedKit },       /* on the ground: refused */
        { AT(5), MedKit },       /* climbing through 6 m: refused */
        { AT(15), MedKit },      /* at 30 m: from the hook that keeps the balance */
        { AT(20), SensorPod },   /* hook 4 is jammed: the store hangs */
        { AT(25), WaterPack },   /* with the hung store still on: would unbalance */
        { AT(27), SupplyCrate }, /* none on board */
    };
    for (size_t i = 0; i < sizeof requests / sizeof requests[0]; i++) {
        if (requests[i].at == cycle) {
            in->release_req = true;
            in->req_kind = requests[i].kind;
            requested = true;
        }
    }
    in->jettison_req = cycle == AT(30);
    if (cycle == AT(30)) event = "operator: JETTISON";
    in->maint_reset = cycle == AT(48);
    if (cycle == AT(48)) event = "ground crew: maintenance reset";
    in->present1 = present[1]; in->id1 = tag[1];
    in->present2 = present[2]; in->id2 = tag[2];
    in->present3 = present[3]; in->id3 = tag[3];
    in->present4 = present[4]; in->id4 = tag[4];
    return true;
}

static void line(uint32_t cycle, const char* text) {
    uint32_t ms = cycle * PMS_PERIOD_MS;
    printf("t=%3u.%02u s  #%-4u  %s\n", ms / 1000u, ms % 1000u / 10u, cycle, text);
}

void pms_platform_write(uint32_t cycle, const PMS_Input* in, const PMS_Output* out) {
    char buf[160];
    const bool fire[5] = { false, out->fire1, out->fire2, out->fire3, out->fire4 };
    for (int k = 1; k <= 4; k++) {
        /* A jammed hook only lets go under an emergency jettison. */
        if (fire[k] && present[k] && (!jammed[k] || out->phase == PhJettison)) {
            leaving[k] = true;
        }
    }

    if (!have_prev) {
        snprintf(buf, sizeof buf, "inventory: 1 %s, 2 %s, 3 %s, 4 %s", kind_name(out->kind1),
                 kind_name(out->kind2), kind_name(out->kind3), kind_name(out->kind4));
        line(cycle, buf);
    }
    if (event) {
        line(cycle, event);
    }
    if (requested) {
        if (out->inhibit == Clear) {
            snprintf(buf, sizeof buf, "operator: release %s -> accepted, plan %s", kind_name(in->req_kind),
                     hooks(out->next1, out->next2, out->next3, out->next4));
        } else {
            snprintf(buf, sizeof buf, "operator: release %s -> refused: %s (alt %d m)", kind_name(in->req_kind),
                     inhibit_name(out->inhibit), (int)in->alt_m);
        }
        line(cycle, buf);
    }
    if (!have_prev || out->phase != prev.phase) {
        snprintf(buf, sizeof buf, "sequencer: %s", phase_name(out->phase));
        line(cycle, buf);
    }
    if (!have_prev || out->fire1 != prev.fire1 || out->fire2 != prev.fire2 || out->fire3 != prev.fire3 ||
        out->fire4 != prev.fire4) {
        bool any = out->fire1 || out->fire2 || out->fire3 || out->fire4;
        snprintf(buf, sizeof buf, any ? "hooks: FIRE %s" : "hooks: closed",
                 hooks(out->fire1, out->fire2, out->fire3, out->fire4));
        line(cycle, buf);
    }
    const StationStatus st[5] = { Vacant, out->status1, out->status2, out->status3, out->status4 };
    const StationStatus pst[5] = { Vacant, prev.status1, prev.status2, prev.status3, prev.status4 };
    for (int k = 1; k <= 4; k++) {
        if (!have_prev || st[k] != pst[k]) {
            snprintf(buf, sizeof buf, "station %d: %s", k, status_name(st[k]));
            line(cycle, buf);
        }
    }
    if (!have_prev || out->total_mass != prev.total_mass || out->balanced != prev.balanced) {
        snprintf(buf, sizeof buf, "payload: %d g, roll %d g*mm, pitch %d g*mm, %s", (int)out->total_mass,
                 (int)out->roll, (int)out->pitch, out->balanced ? "balanced" : "UNBALANCED");
        line(cycle, buf);
    }
    prev = *out;
    have_prev = true;
}
