// Calls the core through include/devload.h only, like the Swift app does.
// Built and run by `make abi-test`.
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "devload.h"

static atomic_int ticks;

static void on_event(void *ctx, DlEvent ev) {
    (void)ctx;
    if (ev.kind == DL_EVENT_TICK) atomic_fetch_add(&ticks, 1);
}

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); return 1; } } while (0)

int main(int argc, char **argv) {
    CHECK(argc == 2);
    CHECK(dl_start(argv[1], on_event, NULL) == DL_OK);
    for (int i = 0; i < 100 && atomic_load(&ticks) < 1; i++) usleep(100000);
    CHECK(atomic_load(&ticks) >= 1);

    DlSnapshot *s = dl_snapshot_get();
    CHECK(s && s->ready && s->total > 0 && s->used <= s->total);
    printf("used %.1f / %.0f GB, pressure %u, cpu %u%%, %u dev rows, %u app rows\n",
           s->used / 1073741824.0, s->total / 1073741824.0, s->pressure, s->cpu_pct, s->dev_len, s->apps_len);
    for (uint32_t i = 0; i < s->apps_len; i++) CHECK(s->apps[i].kind == 1 && strlen(s->apps[i].name) > 0);
    if (s->apps_len > 0) {
        DlParts *ps = dl_parts_get(s->apps[0].id);
        CHECK(ps && ps->len >= 1 && strlen(ps->parts[0].label) > 0);
        printf("top app: %u parts, %u processes\n", ps->len, ps->processes);
        dl_parts_free(ps);
    }
    CHECK(dl_parts_get(UINT32_MAX) == NULL);
    CHECK(!dl_quit(UINT32_MAX));
    dl_snapshot_free(s);

    enum { W = 300 };
    float lo[W], hi[W], stack[4 * W];
    uint8_t level[W];
    bool has[W];
    DlSeriesMeta m;
    CHECK(dl_series_fill(0, 0, W, lo, hi, level, NULL, NULL, &m));
    CHECK(m.to - m.from == 3600 && level[W - 1] <= 2);
    CHECK(dl_series_fill(0, 1, W, NULL, NULL, NULL, stack, has, &m));
    CHECK(has[W - 1] && m.top_len >= 1 && m.top_names);
    dl_string_free(m.top_names);

    char *p = dl_prompt_get(0);
    CHECK(p && strstr(p, "devload.sqlite"));
    printf("prompt: %zu chars\n", strlen(p));
    dl_string_free(p);

    dl_stop();
    CHECK(dl_snapshot_get() == NULL);
    puts("ok");
    return 0;
}
