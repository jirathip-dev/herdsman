# Performance budgets

The board stays fast by never re-sending everything (#337, #351): the step takes one look and saves it, a request
replays it, the live stream sends changes, and history opens bounded. Each budget below is asserted by a test; a miss
fails CI. The timings run through the real server, `serve_on` on a socket with its sampler and watcher threads, on a
fixture runner that answers like herdr, git and ssh do (30 lanes on 2 hosts, +300 ms per remote call); a call-count
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

The numbers go to the CI job summary (`$GITHUB_STEP_SUMMARY`). How each path works: [serve.md](serve.md).
