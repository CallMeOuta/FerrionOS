# Scheduling

The kernel that FerrionOS builds decides which context runs next in `src/context/switch.rs` of the kernel source (recipe `recipes/core/kernel`, fetched to `recipes/core/kernel/source` by `make f.kernel`). A context is what the kernel schedules, typically one user-space thread, and its scheduling state is in the `Context` struct in `src/context/context.rs`. The comment on `select_next_context`, the function that makes the choice, says that it uses EEVDF ([earliest eligible virtual deadline first](https://en.wikipedia.org/wiki/Earliest_eligible_virtual_deadline_first_scheduling)). The comment at the top of the file still describes a round-robin scheduler.

At the commit that the recipe pins, the code does the following:

- The timer [interrupt handler](https://wiki.osdev.org/Interrupts) calls `tick`, which counts ticks for each CPU and calls `switch` when the count reaches `TICK_INTERVAL` (3). Other parts of the kernel call `switch` directly, for example when a context has to wait.
- Each CPU has its own run queue, ordered by virtual deadline (`RunContextData` in `src/context/mod.rs`).
- Each context has a priority, `prio`, from 0 to 39, with 20 as the default. The table `SCHED_PRIO_TO_WEIGHT` turns it into a weight: a lower number gives a larger weight, and one step changes the weight by a factor of about 1.25.
- Each context has a virtual run time, `vtime`. After a context has run, its `vtime` grows by the elapsed time divided by its weight, so a context with a larger weight is charged more slowly.
- Each context has a time slice, `rem_slice`, and a virtual deadline, `vd`. When the slice is used up, the context gets a new slice of `BASE_SLICE_TICKS` (9 units of `NANOS_PER_TICK`, which is 2.25 ms) and a new deadline: its `vtime` plus the slice divided by its weight.
- Each run queue has a virtual time of its own, `v`, which grows by the elapsed time divided by the total weight of the queue's contexts. A context is eligible when its `vtime` is not ahead of `v`. `select_next_context` picks the eligible context with the earliest virtual deadline. When no context is eligible, it moves `v` forward to the smallest `vtime` it found.
- `sched_affinity` holds the set of CPUs a context may run on, and a CPU skips a context whose set does not contain it.
- A CPU regularly looks at the run queues of the other CPUs. When another CPU's load is at least `STEAL_THRESHOLD` (2) contexts above its own, it moves up to `MAX_STEAL` (2) contexts from that queue to itself.
- When no context can run, the CPU switches to its idle context.

For background on scheduling algorithms in general, see the [scheduling documentation](https://wiki.osdev.org/Scheduling_Algorithms) of the OSDev wiki.
