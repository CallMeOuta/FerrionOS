# 0055: Network Lease Daemon, Status/Control Schemes, and Driver Link State (NW2)

Date: 2026-11-25
Status: Accepted

## Context

Prior to NW2, FerrionOS obtained IPv4 leases through a oneshot `dhcpd` process at boot. It never renewed at T1, never rebound at T2, never withdrew an expired address or aborted sockets still sending from it, never probed for address conflicts, ignored RFC 3442 classless static routes and interface MTU options, lacked driver carrier-state reporting, and left unprivileged `user` processes unable to inspect `eth0` in `getifaddrs` because `/scheme/netcfg` is root-only ([ADR 0048](0048-nw1-network-stack.md)).

NW2 replaces oneshot `dhcpd` with a long-running lease daemon (`ferrion-netd`), splits read-only network status (`netstatus`) from root-only control (`netctl`), exposes read-only carrier state from `driver-network`, `e1000d`, and `virtio-netd` in `ferrion-hardware`, migrates `relibc`'s `getifaddrs` to `netstatus`, adds `ferrion network status|up|down`, releases leases on `shutdown`, and proves the full lease lifecycle through the `net-lease` lab check.

## Decisions

### `ferrion-netd` DHCPv4 state machine, timers, and bounds

`recipes/core/ferrion-netd` implements an explicit RFC 2131 DHCPv4 state machine (`Down`, `Static`, `CarrierDown`, `Init`, `Selecting`, `Requesting`, `Probing`, `Bound`, `Renewing`, `Rebinding`) driven by monotonic `std::time::Instant` deadlines and a `/scheme/event` loop. Protocol, timer, and scheme constants live in `recipes/core/ferrion-netd/crate/src/main.rs`, `dhcp.rs`, `config.rs`, and `scheme.rs`.

- **Event-driven UDP wakeup:** `ensure_udp` registers the active non-blocking `/scheme/udp/...` descriptor in `/scheme/event` with `EVENT_READ` (and `close_udp` unregisters it), logging any event-registration error. The main loop sleeps until the earliest of the next carrier poll (`LINK_POLL_INTERVAL` in `main.rs`) or the state's next protocol deadline, eliminating the earlier `100 ms`/`200 ms` polling caps in `Selecting`, `Requesting`, `Renewing`, and `Rebinding`.
- **Short-lease fidelity and timer bounds:** `compute_timers` in `dhcp.rs` honors short server leases verbatim without stretching them (`t1 = lease / 2`, `t2 = lease * 7 / 8` when omitted, clamped to `t1 <= t2 <= lease`), rejects a zero-second lease (`0`), backs off `Selecting`/`Requesting` retries with RFC 2131 section 4.1 exponential delays (`4..=64 s` with `+/- 1 s` jitter), enforces the `MIN_RENEW_RETRY_SECS` (`60 s`) floor on `Renewing`/`Rebinding` retransmits, and caps `Requesting` tries at `MAX_REQUEST_TRIES` (`main.rs`).
- **Conflict probing and bind failure backoff:** Before binding a newly acquired lease, `ferrion-netd` writes the candidate IP to `/scheme/netcfg/ifaces/eth0/arp/probe` and polls up to `MAX_ARP_POLLS` times at `ARP_POLL_MS` (`main.rs`). On `conflict`, it clears the probe, broadcasts `DHCPDECLINE`, and backs off for `DECLINE_BACKOFF` (`10 s`) before restarting discovery without ever configuring the conflicting address. Every `/scheme/netcfg` write goes through `log_netcfg`; if `addr/set` fails, `apply_and_bind` clears the interface and backs off by `DECLINE_BACKOFF` rather than retrying every second.
- **Options and RFC 7844 privacy:** Requests send the minimal Parameter Request List `[1, 3, 6, 26, 121]` and omit Option 12 (hostname) and Option 61 (client identifier). RFC 3442 Option 121 classless static routes override Option 3 when present, including on-link routes (`gw == 0.0.0.0`). For Option 26 (`OPT_INTERFACE_MTU` in `dhcp.rs`), values below `MIN_MTU` (`576`) are ignored and values above `MAX_MTU` (`1500`) are capped at `MAX_MTU`. Option 6 updates `/scheme/netcfg/resolv/nameserver` and atomically replaces `/etc/net/dns` via `/etc/net/dns.tmp`.
- **Fail-closed entropy:** `rand_bytes` in `main.rs` reads `/scheme/rand` and exits non-zero if opening or reading fails; there is no fallback PRNG.
- **Static and off modes:** `/etc/net/config.toml` (`config.rs`) selects `mode = "dhcp"` (default when absent or invalid), `mode = "static"` (address, optional gateway, DNS list, optional MTU; zero DHCP wire traffic), or `mode = "off"` (clears interface, zero DHCP wire traffic).

### Unit name, service namespace, and accepted access widenings

NW2 introduces three access widenings accepted for this slice:

1. **Long-running root boot service `ferrion-netd`:** Replaces oneshot `dhcpd` in `config/base.toml` using the existing `/usr/lib/init.d/10_dhcpd.service` unit name (`hardware = false`) so image configs and init ordering replace `dhcpd` in place without rewriting downstream init-order lists. After registering `netstatus` and `netctl`, `ferrion-netd` enters a restricted namespace of `["netcfg", "udp", "rand", "time", "event", "file"]` (`file` is required to read `/etc/net/config.toml` on boot and reload and to atomically replace `/etc/net/dns`). The `1 s` carrier poll reads `/scheme/netcfg/ifaces/eth0/link` locally and sends no wire packets.
2. **Read-only `netstatus` scheme in `user` and `ferrion-run`:** Added to `user`'s login scheme list in `config/base.toml` and `config/login-namespace.toml`, to `ferrion-run`'s `NETWORK_SCHEMES` (`recipes/core/ferrion-run/crate/src/lib.rs`), and to `SCOPE_SCHEMES` (`scripts/vm-test.py`). Because `ferrion-run` passes `NETWORK_SCHEMES` to programs with network access, `netstatus` also reaches manifest applications that declare `network`, while remaining withheld under `ferrion-run --no-network` and the `netless` login account. `netstatus` serves only read-only `/status` (JSON) and `/ifaddrs` (CIDR lines), omits the hardware MAC address, and bounds payload sizes and open handles in `recipes/core/ferrion-netd/crate/src/scheme.rs` (`MAX_STATUS_BYTES`, `MAX_IFADDRS_BYTES`, `MAX_STATUS_HANDLES`; per-caller quotas for that shared handle pool are deferred to `.scratch/network/issues/05-netstatus-shared-handle-pool.md`).
3. **Root-only `netctl` scheme:** Root-only through the namespace. `netctl` serves `/ctl` (`up`, `down`, `release`, `reload`, bounded by `MAX_CTL_HANDLES` and `MAX_CTL_WRITE_BYTES` in `recipes/core/ferrion-netd/crate/src/scheme.rs`) and is omitted from `user`'s login namespace in `config/base.toml`, so a direct write from `user` (`uid = 1000`) to `/scheme/netctl/ctl` fails with exit `1` and `/usr/bin/bash: line 1: /scheme/netctl/ctl: No such device` (`ENODEV`), leaving the active lease untouched.

### `netstack` extensions and driver link state

- **`netstack` (`components/base/netstack`):** Adds `/scheme/netcfg/ifaces/eth0/addr/clear` (removes the interface address and non-loopback routes, records the cleared IP, and aborts active TCP sockets bound to it with `ECONNABORTED` while preventing any further packets from the expired address), `/scheme/netcfg/ifaces/eth0/arp/probe` (bounded RFC 5227 probe using `0.0.0.0` sender IP, reporting `idle`, `pending`, `free`, or `conflict <mac>` in `src/link/ethernet.rs`), writable `/scheme/netcfg/ifaces/eth0/mtu` (`576..=1500`, applied to `smoltcp` so TCP SYN MSS reflects the negotiated MTU), on-link route syntax `<prefix> dev eth0` in `/scheme/netcfg/route/add` with `RouteTable::insert_rule` (`src/router/route_table.rs`) returning `ENOSPC` at `MAX_ROUTES` (loopback route insertion at startup asserts success in `src/scheme/mod.rs`), and read-only `/scheme/netcfg/ifaces/eth0/link` proxying the NIC driver's `link` resource.
- **Driver link state (`ferrion-hardware` commit `1b8507d29dfce5f44470f959a4c4fea742b14fd1`):** Pinned in `recipes/core/drivers/recipe.toml` directly on top of `98433eaad` (`scripts/drivers-lock.sh check` passes with no `recipes/core/drivers/Cargo.lock` change; identical network diff to earlier `a1e12fc` with `git patch-id --stable` `40fe28ce`). Modifies `driver-network`, `e1000d`, and `virtio-netd` (4 files) to expose a read-only `link` resource returning `"up\n"`, `"down\n"`, or `"unknown\n"`; other network drivers fall back to `NetworkAdapter::link_up`'s default and report `"unknown\n"`. When carrier reports `down`, `ferrion-netd` clears `eth0` and enters `CarrierDown`; when carrier returns, it restarts fresh DHCP discovery (a short carrier-blip grace period is deferred to `.scratch/network/issues/06-carrier-blip-grace-period.md`).

### `relibc` `getifaddrs`, `ferrion network`, and shutdown release

- **`relibc` `getifaddrs` (`components/relibc/src/header/ifaddrs/mod.rs`):** Reads `eth0` addresses from `/scheme/netstatus/ifaddrs` via a single `push_eth_cidr` helper, falling back to `/scheme/netcfg/ifaces/eth0/addr/list` for any caller whose `netstatus` read finds nothing, while keeping the bounds in `ifaddrs/mod.rs` (`MAX_IFADDRS_ENTRIES`, `MAX_NETCFG_BYTES`, `MAX_NETCFG_READS`). Unprivileged `user` processes now observe both `lo` (`0x49`) and `eth0` (`0x43`), while `ferrion-run --no-network` observes `lo` without `IFF_UP` (`0x8`).
- **`ferrion network` CLI (`recipes/core/ferrion-cli`):** Adds `ferrion network status` (human and `--json`, reading `/scheme/netstatus/status` and treating `ENOENT`/`EACCES`/`ENODEV` as offline) and root-only `ferrion network up|down` (writing `/scheme/netctl/ctl`).
- **Shutdown DHCPRELEASE (open product question 10, default yes):** `recipes/core/coreutils/ferrion-shutdown-net-release.patch` writes `release\n` to `/scheme/netctl/ctl` on a worker thread bounded by a `1,000 ms` deadline (ignoring missing or inaccessible `netctl`) before kernel poweroff so the DHCP server reclaims the lease on clean shutdown without stalling shutdown if `ferrion-netd` does not answer.

### `net-lease` lab check and `net-probe hold`

`scripts/net_lab.py` adds the 4-boot, 16-case `net-lease` check (`--make net=e1000` and `--make net=virtio`), and `recipes/tests/net-probe` adds `net-probe hold` (which ignores `SIGPIPE` and sends a 1-byte heartbeat every `200 ms` so active TX runs continuously across lease expiry, proving `ECONNABORTED` delivery and `0` leaked frames from the expired IP).

- Boot 1 exercises `case9_user_netctl_denied` (first login as `user` on a pristine serial console, checking `id -u` is nonzero, direct write to `/scheme/netctl/ctl` exits non-zero, `0` `DHCPRELEASE` frames leave the guest, the lease stays in `dnsmasq`, and `root` does see `/scheme/netctl`), `no_dhcpd`, `status_no_mac`, `option121_routes` (default + on-link, Option 3 ignored), `option26_mtu` (SYN MSS `1360` for MTU `1400`), `rfc7844_privacy`, `case1_t1_renew` (unicast at ~`60 s`), `case2_t2_rebind` (broadcast at ~`105 s`), `case3_expiry_abort` (`ENETUNREACH` on new connect, `ECONNABORTED` on active `net-probe hold`, `0` leaked packets from expired IP), `case4_new_range`, `case8_carrier` (QMP `set_link net0 off`/`on`), and `case5_release` (`ferrion network down`/`up` plus `shutdown` DHCPRELEASE clearing `dnsmasq`'s lease file).
- Boot 2 exercises `case7_arp_conflict` (`DHCPDECLINE`, never configuring the conflicting IP, `>= 9.5 s` backoff, clean IP acquired) followed immediately by `case6_spoof_ignored`: `dnsmasq-clean` is stopped, the harness captures the live unicast T1 `Renewing` `DHCPREQUEST` and its `xid`, injects four forged replies (`yiaddr` `10.77.0.98` outside `10.77.0.80..95`, wrong `server_id` NAK, wrong `server_id` ACK, wrong `xid` ACK) asserting `state == "renewing"` and clean `/etc/net/dns`, and finally injects one valid positive-control ACK carrying the live `xid` and marker DNS `192.0.2.95` asserting `state == "bound"` and `/etc/net/dns` equals `["192.0.2.95"]`.
- Boots 3 and 4 reboot into `/etc/net/config.toml` `mode = "static"` and `mode = "off"`, verifying zero DHCP wire packets after switchroot.

## Verification

### Lease lifecycle, S1 wakeup measurement, and driver pin-rule checks (base `044675303`, drivers `a1e12fc`)

- **Red-before `net-lease` (`e1000`, pre-NW2 `net-test` image `build/nw2-red-base.img` built on `19e5585d1` before the `044675303` rebase, carrying NW2's `net-probe` with `hold`):** FAIL `0/16` green (all 16 cases red, including non-vacuous Boot 2 `case6_spoof_ignored` and Boot 1 `case9_user_netctl_denied`).
- **`net-lease` (`e1000`, base `044675303` + `a1e12fc`, target 10/10):** **10/10 PASS (`16/16` green on every run, zero flakes)**. T1 renew delay `60.00–60.06 s`, ARP conflict decline backoff `10.0 s` in all 10 runs, `case3_expiry_abort` `0` leaked frames from expired IP in all 10 runs, `case6_spoof_ignored` positive-control ACK bound with `["192.0.2.95"]` in all 10 runs, `case9_user_netctl_denied` `uid=1000`, `write_rc=1`, `/usr/bin/bash: line 1: /scheme/netctl/ctl: No such device`, `0` releases, lease kept in all 10 runs.
- **`net-lease` (`virtio`, base `044675303` + `a1e12fc`, target 2/2 including `case8_carrier`):** **2/2 PASS (`16/16` green on both runs)**. T1 renew delay `60.00–60.01 s`, decline backoff `10.0 s`, `case8_carrier` PASS on both runs, same `case6` and `case9` results.
- **S1 idle-`Selecting` measurement (`60 s` in `Selecting` with `dnsmasq` stopped, `3` DISCOVERs sent, `/scheme/sys/context` `10 ms` tick resolution on `044675303` + `a1e12fc`):** Removing the `100 ms` polling cap and registering the UDP fd on `/scheme/event` drops `ferrion-netd`'s timer wakeups in `Selecting` from at least `10/s` (`>= 600/min`) to the `1 Hz` carrier poll (`60/min`) plus exponential DISCOVER backoff (`3` retries in `60 s`). Measured CPU figures sit at the `10 ms` `ACTTIME`/`SCHTIME` tick resolution:
  - Before S1 (pre-S1 `ferrion-netd` `main.rs` + `dhcp.rs` with `100 ms` polling cap cooked into the `044675303` + `a1e12fc` image): `ferrion-netd` `acttime_delta = 4` ticks (`0.04 s`), `schtime_delta = 7` ticks (`0.07 s`), `netstack` `3` ticks (`0.03 s`), `e1000d` `1` tick (`0.01 s`), `total_guest = 1.75 s`, `private_bytes = 835,584`.
  - After S1 (event-driven UDP fd on `044675303` + `a1e12fc`): `ferrion-netd` `acttime_delta = 2` ticks (`0.02 s`), `schtime_delta = 2` ticks (`0.02 s`), `netstack` `2` ticks (`0.02 s`), `e1000d` `0` ticks (`0.00 s`), `total_guest = 4.43 s`, `private_bytes = 831,488` (`0 B` growth).
- **Driver pin-rule checks on `a1e12fc` (base `044675303`, 7/7 PASS):**
  1. Drivers build (`cr.drivers` + `cr.initfs` + `image`, `scripts/drivers-lock.sh check` current): PASS.
  2. `virtio net-hardening`: PASS (`20/20` lookups, `0 KiB` over `40,000` ARP senders, `virtio-netd` PID `25` survived frames up to `9,000 B` and past-end `mac` reads, independent ISNs).
  3. `2x` untimed `virtio net-basics`: Run 1 passed `9/9` other untimed items (`9_arp_cache_bound` covered by `virtio net-hardening`); Run 2 (sharing `virtio-net-hardening` output dir) passed `10/10` untimed items (`red: none`; timed items `2`, `3`, `7` marked `host_limited` outside a quiet window).
  4. `usb-tablet` (default GPU): PASS (`12/12` rounds, `24,276` moves, `276/276` checked moves reached cursor, `0` lost xHCI events).
  5. `display-modes` (`gpu=virtio`): PASS (`4/4` sizes `1280x800` through `3840x2160`, desktop `3.5–3.9 s`, `errors []`).
  6. `gpu-heads` (`gpu=virtio`): PASS (both `1280x800` Orbital heads painted).
  7. `display-connectors` (`gpu=virtio`): PASS (`4/4` hotplug phases stable on `Virtual-1` and `Virtual-2`).
- **Network regressions on `044675303` + `a1e12fc`:** `e1000 net-hardening` PASS (`20/20` lookups, `0 KiB` over `39,992` ARP senders), `net-no-lease` PASS (`unconfigured=True`, `ENETUNREACH`, `netstack` PID `34->34`), `resolver` PASS, `linux-net` PASS.
- **Pre-rebase smoke note (`19e5585d1`, `5793ed1` + link-state patch):** `e1000 net-lease` also passed `3/3` (`16/16` green each) prior to rebasing onto `044675303`.

### Post-rebase pin-rule checks and untimed regressions (base `3fae26417` + drivers `1b8507d29`)

- **Driver pin-rule checks on `1b8507d29` (base `3fae26417`, `98433eaad -> 1b8507d29`, all runs in registered `quiet.py` heavy slots):**
  1. Drivers build (`cr.kernel,drivers,initfs` + `net-test` / `login-namespace` images, `scripts/drivers-lock.sh check` current with no `Cargo.lock` change): PASS.
  2. `virtio net-hardening`: PASS (`20/20` lookups, `0 KiB` over `40,000` ARP senders, `virtio-netd` PID `25` survived frames up to `9,000 B` and past-end `mac` reads, independent ISNs `4103147039` and `3137592579`).
  3. `virtio net-basics` (untimed items; timed items `2`, `3`, `7` blocked outside a quiet window): `12_accept_memory_bound` (`MAX_ACCEPT_MEM_GROWTH_BYTES = 2 MiB`) is **FAIL with and without NW2** on `3fae26417` and current `master` `4c4047125` (all other untimed items `1`, `4`, `6`, `8`, `9`, `10`, `11`, `13` PASS across all seven NW2 runs; `5_dns_reply_match` PASS `20/20` at normal load and dropped replies at `load1 40.04–48.91` on both NW2 and unmodified `3fae26417`). With the `netstack` commit unchanged since `044675303` (`range-diff`) and `ferrion-hardware` carrying the identical network-driver diff (`patch-id 40fe28ce`), `12_accept_memory_bound` passed `2/2` on base `044675303` + `a1e12fc` (`2.94 -> 2.94 MiB`, `+0.00 MiB`, and `2.94 -> 3.31 MiB`, `+0.37 MiB`):
     - NW2 `3fae26417` + `1b8507d29` (`build/nw2-3fae26417/pin/virtio-net-basics-1..7/net-basics-1/net-basics.json`):
       - Run 1 (`load1 18.09 -> 12.07`): item 12 FAIL `2.94 -> 12.90 MiB` (`+9.96 MiB`, `200/200`, `no_ref_warning=false`); item 5 PASS `20/20`.
       - Run 2 (`load1 48.91 -> 48.54`): item 12 PASS `14.20 -> 14.20 MiB` (`+0.00 MiB`, `200/200`, started at `14.20 MiB`); item 5 FAIL `19/20` (`retry=True`, `tcp=True`).
       - Run 3 (`load1 47.62 -> 44.95`): item 12 FAIL `4.36 -> 13.10 MiB` (`+8.74 MiB`, `200/200`); item 5 FAIL `17/20` (`retry=True`, `tcp=False`).
       - Run 4 (`load1 12.19 -> 18.42`): item 12 FAIL `9.19 -> 12.90 MiB` (`+3.71 MiB`, `200/200`); item 5 PASS `20/20`.
       - Run 5 (`load1 18.42 -> 18.46`): item 12 FAIL `10.40 -> 12.90 MiB` (`+2.50 MiB`, `200/200`); item 5 PASS `20/20`.
       - Run 6 (`load1 5.96 -> 10.07`): item 12 FAIL `10.40 -> 12.90 MiB` (`+2.50 MiB`, `200/200`); item 5 PASS `20/20`.
       - Run 7 (`load1 10.07 -> 6.63`): item 12 FAIL `10.50 -> 13.00 MiB` (`+2.50 MiB`, `200/200`); item 5 PASS `20/20`.
     - Unmodified `master` `3fae26417` + `98433eaad`, zero NW2 commits (`build/nw2-3fae26417/master-virtio-net-basics-1..2/net-basics-1/net-basics.json`):
       - Run 1 (`load1 40.04 -> 40.96`): item 12 FAIL `3.02 -> 13.00 MiB` (`+9.98 MiB`, `200/200`, `no_ref_warning=false`); item 5 FAIL `19/20` (`retry=True`, `tcp=True`).
       - Run 2 (`load1 40.96 -> 40.76`): item 12 PASS `3.01 -> 3.01 MiB` (`+0.00 MiB`, `200/200`); item 5 PASS `20/20`.
     - Unmodified `master` `4c4047125` + `98433eaad`, zero NW2 commits (`build/nw2-4c4047125/master-virtio-net-basics-1..2/net-basics-1/net-basics.json`):
       - Run 1 (`load1 19.84 -> 13.01`): item 12 FAIL `9.15 -> 12.90 MiB` (`+3.75 MiB`, `200/200`, `no_ref_warning=false`); item 5 PASS `20/20`.
       - Run 2 (`load1 13.01 -> 19.88`): item 12 PASS `15.40 -> 13.00 MiB` (`-2.40 MiB`, `200/200`); item 5 PASS `20/20`.
  4. `usb-tablet` (default GPU): PASS (`12/12` rounds, `24,276` moves, `276/276` checked moves reached cursor, `0` lost xHCI events).
  5. `display-modes` (`gpu=virtio`): PASS (`4/4` sizes `1280x800` through `3840x2160`, desktop `3.8–5.2 s`, `errors []`).
  6. `gpu-heads` (`gpu=virtio`): PASS (both `1280x800` Orbital heads painted).
  7. `display-connectors` (`gpu=virtio`): PASS (`4/4` hotplug phases stable on `Virtual-1` and `Virtual-2`).
- **Untimed regressions on `3fae26417` + `1b8507d29`:** `boot` PASS (`3.7 s`), `shells` PASS (`5/5`), `exec-handles` PASS, `ferrion-run` PASS, `agent-scope` PASS (also passed prior to adding `netstatus` to `SCOPE_SCHEMES` on `a97b19a9d` + `a1e12fc` because `/etc/ferrion-run/ferrion-run-probe.toml` sets `network = false` and withholds all `NETWORK_SCHEMES`), `ifaddrs` PASS (`root` and `user` see `lo(0x49)+eth0(10.0.2.15 0x43)`; `--no-network` sees `lo(0x8)` and offline status), `login-namespace` (`--config login-namespace`) PASS (`netless` sees no network scheme; `user` sees `['ip', 'icmp', 'tcp', 'udp', 'netstatus']`).
- **Pre-existing `master` failures observed on `3fae26417` (not touched in NW2):**
  - `app-declared`: all `ferrion-run` capability checks PASS (`host saw: ['/declared-net', '/declared-net2']`); fails only at `scripts/vm-test.py:6657` (`mpv.toml` missing after `152448072` moved `mpv` to `wm-session`, plus cached `cosmic-term.toml`). Reproduces identically on unmodified `master` `a97b19a9d` (`desktop` image, zero NW2 code); fixed on `master` by `751e15726`.
  - `app-undeclared`: all capability checks PASS except `homecopy` (`ferrion-run --read-only /home/user/bin /home/user/bin/pfr1-declared` exits `125` with `Permission denied (os error 13)` after PFR2a `PROT_EXEC` gating in `4e2199585`/`8d104d886`, tracked in `.scratch/platform-rules/issues/02-app-undeclared-homecopy-exec.md`). Reproduces identically on unmodified `master` `a97b19a9d` (`desktop` image, zero NW2 code).

## Code locations

- `recipes/core/ferrion-netd/` (`crate/Cargo.toml`, `crate/Cargo.lock`, `crate/src/main.rs`, `dhcp.rs`, `config.rs`, `scheme.rs`, `recipe.toml`)
- `components/base/netstack/src/` (`main.rs`, `link/ethernet.rs`, `link/mod.rs`, `router/mod.rs`, `router/route_table.rs`, `scheme/mod.rs`, `scheme/netcfg/mod.rs`, `scheme/socket.rs`, `scheme/tcp.rs`, `scheme/udp.rs`) and `components/base/vendor/smoltcp/src/` (`iface/interface/mod.rs`, `socket/tcp.rs`)
- `recipes/core/drivers/recipe.toml` (pinning `ferrion-hardware` `1b8507d29dfce5f44470f959a4c4fea742b14fd1`, touching `driver-network`, `e1000d`, and `virtio-netd`)
- `recipes/core/coreutils/ferrion-shutdown-net-release.patch` and `recipes/core/coreutils/recipe.toml`
- `recipes/core/ferrion-cli/crate/src/` (`main.rs`, `network.rs`)
- `recipes/core/ferrion-run/crate/src/lib.rs`, `config/base.toml`, `config/login-namespace.toml`, and `scripts/app-manifest-non-apps.toml`
- `components/relibc/src/header/ifaddrs/mod.rs`
- `recipes/tests/net-probe/crate/src/main.rs`, `scripts/net_lab.py`, and `scripts/vm-test.py`
