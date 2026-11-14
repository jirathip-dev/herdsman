# Performance budgets

The board stays fast by never re-sending everything (#337, #351): the step takes one look and saves it, a request
replays it, the live stream sends changes, and history opens bounded. Each budget below is asserted by a test; a miss
fails CI. The timings run through the real server, `serve_on` on a socket with its sampler and watcher threads, on a
fixture runner that answers like shed, git and ssh do (30 lanes on 2 hosts, +300 ms per remote call); a call-count
probe on that runner proves the server, not a hand-built page, produced them.

| Budget | Path it covers | Test |
|---|---|---|
| a step makes ≤ 1 remote call per host; a request or `status` makes none | `drive --fast`, then `serve_on` + the CLI | `perf.rs` `a_step_makes_one_remote_call_per_host_and_requests_make_none` |
| first page `GET /` < 150 ms and < 40 KB | `serve_on`'s socket (warm) | same |
| `/herd.json` < 100 ms, `status` < 300 ms | `serve_on`'s socket; the CLI | same |
| first contentful paint in WebKit < 300 ms (warm) | `serve_on`'s socket, WebKit | `perf.rs` `the_first_paint_in_webkit_is_under_300ms` |
| a steady-state `/live` change < 5 KB: the changed part's data only (#269: `needs`), no Lanes or [chat] | `serve_on`'s `/live` stream and watcher; the board draws the same cards as from API v1 | `perf.rs` `a_steady_state_live_change_is_one_part_under_5kb` |
| a chat opens with ≤ 64 KB: its last 10 turns (3.2 KB a turn) and the lines that fit in 32 KB; 10 turns more a scroll | `GET /lane` | `history.rs` `a_chat_opens_with_its_last_10_turns_within_64kb` |
| Events opens with 100 rows a view, then 100 a page | `GET /frag?id=events&skip=` | `history.rs` `events_open_with_100_rows_then_page` |
| 100 board requests on a 30,000-event store (20 lanes, a new event every 10th) < 2 s of CPU in release: a request reads only the events appended since the last (`events::shared`), the watcher renders only for a listening audience | `serve_on`'s socket | `perf.rs` `a_hundred_board_requests_on_a_30k_event_store_stay_under_the_cpu_budget` |
| a fast pass (`drive --fast`) over a 100,000-event log < 10 s with at most one full-log read: every read of the log after the first takes only the rows appended since (`events::shared`); a pass longer than `[limits] pass_max_secs` (default 120) logs `pass-slow` naming its slowest step (#825) | `drive --fast` | `pass_budget.rs` `a_fast_pass_over_a_100k_event_log_reads_it_once_and_stays_under_its_bound` |
| `/api/v1/state` on a 150,000-event log (5 fleets, asks, threads, lanes) < 300 ms, median of 5, in release; no read builds an ask index from a copy of the log (`events::REBUILDS`); an answered ask is gone from the next read (#859) | `serve_on`'s socket | `perf.rs` `the_state_read_stays_under_300ms_on_a_150k_event_log` |
| no board GET starts a process: shed, ssh and the rest are read by serve's background threads, each source on its own `[limits]` interval (`serve_shed_secs` 5, `serve_remote_secs` 10, `serve_host_secs` 10); `/api/v1/state` and `/api/v1/health` carry each one's `refreshed_at` in `sources` (#859) | `serve_on`'s socket, a runner that panics on a call during a request | `perf.rs` `no_board_request_starts_a_process` |
| `/api/v1` p95 over `[limits] slo_api_p95_ms` (1000) for `slo_api_minutes` (10): one `slo-api` push and Drover card naming the slowest route; `/api/v1/health`'s `routes` lists each route's p50/p95/count (#859) | `Server::slo` | `board_fresh.rs` `a_slow_api_for_10_minutes_alerts_once_and_never_after_it_recovers` |

The numbers go to the CI job summary (`$GITHUB_STEP_SUMMARY`). How each path works: [serve.md](serve.md).
