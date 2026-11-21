# fdobjd serves eventfd and timerfd objects, and its probe checks the public calls

Status: accepted

## Context

relibc's `eventfd` and `timerfd` are served by `fdobjd`, a daemon started from `config/base.toml`. It arms kernel timeouts on the time scheme, runs in a null namespace after start-up, and must keep serving whatever intervals a caller asks for. `fd-primitives-probe` checks the calls a program makes.

## Decision

### The daemon

- A timer descriptor that has been rearmed or closed keeps its older generations until their kernel timeouts fire: closing cannot cancel a kernel timeout.
- Before publishing an armed timer, `call` checks that the next period is representable, so a caller's interval cannot make the asynchronous rearm stop the daemon.
- `retry` runs the pending eventfd operations again until none completes, because a later reader that drains the counter can unblock an earlier writer.
- `run` opens the root capability of `/scheme/time` from the namespace before it drops namespace access; opening the scheme's path itself would open an empty clock name.

### The probe

- `fd-primitives-probe` uses the public `eventfd` and `timerfd` calls only, not scheme paths or a private descriptor map of the C library, and is linked statically, so it exercises the image's relibc. Each pass line names a completed contract. An alarm bounds blocking reads, readiness waits, `fork` and `exec` failures. It runs alone or through `scripts/vm-test.py fd-primitives`, on an image with `fdobjd`.
- `signalfd` is out of scope and is never reported as passing.
- `poll` calls are kept apart from active `epoll` watches, and the epoll descriptor is closed before polling a timerfd, so poll's registrations add nothing to a live epoll queue.
- FerrionOS refuses `TFD_TIMER_CANCEL_ON_SET` with `EINVAL`; Linux accepts it. The probe checks the refusal and neither tests clock-change delivery nor changes the machine's clock.

## Consequences

Closing a timer's kernel timeout early, or skipping the period check, can leave a timeout firing on a freed slot or stop the daemon. A probe that reached into relibc's private map would pass without the public calls working.

## Where

- `recipes/core/fdobjd/crate/src/main.rs`: `Timer`, `Objects::schedule`, `Objects::call`, `Requests::retry`, `run`
- `recipes/tests/fd-primitives-probe/fd-primitives-probe.c`, `recipes/tests/fd-primitives-probe/recipe.toml`
