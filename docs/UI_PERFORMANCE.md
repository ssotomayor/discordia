# Selective UI updates

Baseline: `b0ed772`. Windows, debug build, same machine and fixture: 200 online
members with cached 16 KiB avatars and 200 updates per scenario. CPU and duration
exclude panel setup; seven samples, median. RAM uses three fresh processes per
scenario and Windows process counters, median. These are VirtualDom tests;
WebView, GPU, networking, native audio and process detection are excluded.

| Scenario | CPU before → after (ms) | CPU reduction | Duration before → after (ms) | Peak resident before → after (MiB) | Final private before → after (MiB) |
|---|---:|---:|---:|---:|---:|
| Messages | 2078.1 → 312.5 | 85.0% | 2089.2 → 304.7 | 20.95 → 23.91 | 11.42 → 13.14 |
| Speaking | 2109.4 → 578.1 | 72.6% | 2109.0 → 584.1 | 20.79 → 23.78 | 11.38 → 13.18 |
| Game details | 2093.8 → 312.5 | 85.1% | 2111.6 → 307.3 | 20.83 → 23.63 | 11.46 → 13.14 |

Selective subscriptions cost roughly 3 MiB resident and 2 MiB private memory
in this fixture. Shared image bytes and fewer row components limit that cost.
These CPU reductions do not describe total app usage or release performance.

| Functional check | Before | After |
|---|---:|---:|
| Panel/each row executions for 100 unrelated changes, two members | 101 | 1 |
| DOM edits for 200 speaking changes | 200 | 200 |
| DOM edits for 200 messages/game-detail changes | 0 | 0 |

CPU savings come from avoiding component work and sorting. Clock tests cover
focus/visibility suspension, catch-up, removal and preserving Dioxus text nodes.
Full-app visual validation and RAM including WebView were unavailable locally.
Branch checks: 638 client unit tests, 28 grid-layout tests and 35 JavaScript
tests passed; formatting and Clippy passed.

| Reproduce | Command |
|---|---|
| CPU/duration/DOM edits | `cargo test -p discordia --bin Discordia benchmark_selective_ui -- --ignored --nocapture --test-threads=1` |
| Build RAM fixture | `cargo test -p discordia --bin Discordia --no-run` |
| Sample RAM (Windows) | `python scripts/measure-ui-memory.py <test-executable> --samples 3` |
| Clock regression tests | `node --test scripts/test-activity-clock.mjs` |

Run the identical benchmark fixture on both versions for a comparison.
