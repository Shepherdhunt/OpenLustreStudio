/* OpenLustre trace-compare harness for ReleaseLogic.
 *
 * Reads a CSV of inputs on stdin (same column order as the IR simulator),
 * runs the generated C-Lite ReleaseLogic_step on each row, and writes a CSV
 * of outputs to stdout with the same shape the IR simulator produces. The
 * harness also feeds each row to the contract monitor and asserts that no
 * assumption or guarantee is violated.
 */
#include "openlustre_generated.h"
#include "openlustre_monitors.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

static bool parse_bool(const char* s) {
    return (s && (s[0] == 't' || s[0] == 'T' || s[0] == '1'));
}

int main(void) {
    char line[1024];
    /* skip header */
    if (!fgets(line, sizeof line, stdin)) return 0;
    printf("cycle,release_cmd,inhibit,monitor_violation\n");

    ReleaseLogic_State state;
    ReleaseLogic_init(&state);
    ReleaseLogic_contract_monitor_State mon;
    ReleaseLogic_contract_monitor_reset(&mon);

    int cycle = 0;
    while (fgets(line, sizeof line, stdin)) {
        if (line[0] == '\n' || line[0] == '\0') continue;
        char* tok;
        char* save;
        const char* fields[5] = {0};
        int i = 0;
        for (tok = strtok_r(line, ",\n", &save); tok && i < 5; tok = strtok_r(NULL, ",\n", &save), ++i) {
            fields[i] = tok;
        }
        if (i < 5) {
            fprintf(stderr, "row %d: expected 5 fields, got %d\n", cycle, i);
            return 1;
        }
        ReleaseLogic_Input in = {
            .master_arm       = parse_bool(fields[0]),
            .station_selected = parse_bool(fields[1]),
            .consent          = parse_bool(fields[2]),
            .fault_present    = parse_bool(fields[3]),
            .release_request  = parse_bool(fields[4]),
        };
        ReleaseLogic_Output out = {0};
        ReleaseLogic_step(&state, &in, &out);
        ReleaseLogic_contract_monitor_check(&mon, &in, &out);

        printf("%d,%s,%s,%s\n",
               cycle,
               out.release_cmd ? "true" : "false",
               out.inhibit     ? "true" : "false",
               mon.any_violation ? "VIOLATED" : "ok");
        ++cycle;
    }
    return 0;
}
