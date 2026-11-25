import http.server
import json
import os
import re
import shlex
import shutil
import socket
import statistics
import struct
import subprocess
import sys
import threading
import time
import tomllib
from datetime import datetime, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAB_HOST = "10.77.0.1"
LAB_ALT_HOST = "10.77.0.54"
LAB_INET_HOST = "198.51.100.1"
LAB_GUEST_MAC = "52:54:00:77:00:02"
LAB_GUEST_MAC_BYTES = bytes.fromhex("525400770002")
LAB_DHCP_RANGE = "10.77.0.50,10.77.0.99,255.255.255.0,2m"
HTTP_PORT = 8080
TCP_ECHO_PORT = 9000
UDP_ECHO_PORT = 9001
TCP_FIN_PORT = 9002
IPERF_PORT = 5201
ACCEPT_PROBE_PORT = 5210
FIXED_ISN_SPORT = 45000
CLOSED_PORT = 9
LOAD_LIMIT = 12.0
NOISE_REL_LIMIT = 0.10
MAX_PCAP_PACKETS = 500_000
MAX_ECHO_CONNS = 256
MAX_ECHO_STEPS = 200_000
MAX_DHCP_POLLS = 60
MAX_IPERF_ATTEMPTS = 5
MAX_ACCEPT_PROBES = 200
MAX_ACCEPT_MEM_GROWTH_BYTES = 2 * 1024 * 1024
MAX_FIN_PROBES = 20
MAX_PROC_ENTRIES = 65_536
MAX_ANCESTOR_HOPS = 64
MAX_TASK_ENTRIES = 1024
SCHED_POLICIES = {
    0: "SCHED_OTHER",
    1: "SCHED_FIFO",
    2: "SCHED_RR",
    3: "SCHED_BATCH",
    5: "SCHED_IDLE",
}


class _ForeignQemuDetected(Exception):
    def __init__(self, entries):
        self.entries = entries
        super().__init__(format_foreign_qemus(entries))


def format_foreign_qemus(entries):
    parts = []
    for entry in entries:
        parts.append(
            f"pid={entry.get('pid')} name={entry.get('name')} "
            f"started_at={entry.get('started_at')} cwd={entry.get('cwd')}"
        )
    return "; ".join(parts)

LAB_BASE_TOOLS = ("unshare", "ip", "tc", "dnsmasq", "ping")
LAB_PERF_TOOLS = ("unshare", "ip", "tc", "dnsmasq", "ping", "iperf3")
LAB_TESTS = ("net-basics", "net-perf", "net-no-lease")
MAX_TIMEOUT_SUBS_MARGIN = 2


def read_timeout_subscribers(vm, run_status):
    _, text = run_status(vm, "cat /scheme/sys/stat", 20)
    m = re.search(r"timeout_subscribers:\s*(\d+)", text)
    return int(m.group(1)) if m else None


def bring_up_loopback(blocked_cls):
    res = subprocess.run(["ip", "link", "set", "lo", "up"], capture_output=True, text=True, timeout=15)
    if res.returncode != 0:
        detail = (res.stderr or res.stdout).strip()
        raise blocked_cls(f"cannot bring up loopback in lab namespace: {detail}")


def require_tools(tools, blocked_cls):
    for tool in tools:
        if shutil.which(tool) is None:
            raise blocked_cls(f"missing host tool: {tool}")


def check_userns(blocked_cls):
    require_tools(("unshare",), blocked_cls)
    try:
        res = subprocess.run(
            ["unshare", "--user", "--map-root-user", "--net", "--kill-child", "true"],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.SubprocessError) as err:
        raise blocked_cls(f"unprivileged user namespace unavailable: {err}") from None
    if res.returncode != 0:
        detail = (res.stderr or res.stdout).strip()
        raise blocked_cls(f"unprivileged user namespace unavailable: {detail}")


def required_tools_for_tests(tests):
    tools = list(LAB_BASE_TOOLS)
    if "net-perf" in tests:
        tools.append("iperf3")
    return tuple(dict.fromkeys(tools))


def reexec_in_lab(argv, blocked_cls, tests):
    require_tools(required_tools_for_tests(tests), blocked_cls)
    check_userns(blocked_cls)
    cmd = [
        "unshare",
        "--user",
        "--map-root-user",
        "--net",
        "--kill-child",
        sys.executable,
        "-I",
        *argv,
        "--in-lab",
    ]
    proc = subprocess.run(cmd, cwd=ROOT)
    sys.exit(proc.returncode)


def run_cmd(cmd, check=True, timeout=30):
    res = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if check and res.returncode != 0:
        raise RuntimeError(f"{shlex.join(cmd)}: {(res.stderr or res.stdout).strip()}")
    return res


def parse_acttime(text):
    parts = text.split(":")
    if len(parts) != 3:
        return 0.0
    return int(parts[0]) * 3600.0 + int(parts[1]) * 60.0 + float(parts[2])


def context_snapshot(vm, run_status):
    _, text = run_status(vm, "cat /scheme/sys/context > /tmp/nw-ctx.txt && cat /tmp/nw-ctx.txt", 60)
    units = {"B": 1, "KB": 1 << 10, "MB": 1 << 20, "GB": 1 << 30}
    rows = []
    pattern = re.compile(
        r"(?m)^(\d+)\s+\d+\s+\d+\s+\S+\s+\S+\s+\S+\s+(\d+:\d+:\d+\.\d+)\s+"
        r"(\d+:\d+:\d+\.\d+)\s+([\d.]+) ([KMG]?B)\s+[\d.]+ [KMG]?B\s+(\S.*?)\s*$"
    )
    for m in pattern.finditer(text):
        rows.append(
            {
                "pid": int(m.group(1)),
                "acttime_s": parse_acttime(m.group(2)),
                "schtime_s": parse_acttime(m.group(3)),
                "private_bytes": int(float(m.group(4)) * units[m.group(5)]),
                "name": m.group(6).rsplit("/", 1)[-1],
            }
        )
    return rows


def cpu_delta(before, after, names):
    def sum_for(rows, name):
        return sum(r["acttime_s"] for r in rows if r["name"] == name)

    out = {}
    for name in names:
        delta = sum_for(after, name) - sum_for(before, name)
        out[name] = round(max(0.0, delta), 3)
    total_before = sum(r["acttime_s"] for r in before)
    total_after = sum(r["acttime_s"] for r in after)
    out["total_guest"] = round(max(0.0, total_after - total_before), 3)
    return out


def repo_component_commits(arch):
    target = f"{arch}-unknown-redox"
    out = {}
    for pkg in ("kernel", "base", "relibc"):
        meta = ROOT / "repo" / target / f"{pkg}.toml"
        if not meta.exists():
            out[pkg] = None
            continue
        data = tomllib.loads(meta.read_text())
        out[pkg] = {
            "source_identifier": data.get("source_identifier"),
            "commit_identifier": data.get("commit_identifier"),
            "time_identifier": data.get("time_identifier"),
        }
    return out


def parse_json_line(text):
    for line in reversed(text.splitlines()):
        stripped = line.strip()
        if stripped.startswith("{") and stripped.endswith("}"):
            try:
                return json.loads(stripped)
            except json.JSONDecodeError:
                continue
    return None


def parse_pcap_syns_all(pcap_path, dst_ip, dst_port, sport_filter=None):
    raw = pcap_path.read_bytes()
    if len(raw) < 24:
        return []
    magic = struct.unpack("<I", raw[:4])[0]
    if magic == 0xA1B2C3D4:
        sub_scale = 1e-6
    elif magic == 0xA1B23C4D:
        sub_scale = 1e-9
    else:
        return []
    syns = []
    pos = 24
    for _ in range(MAX_PCAP_PACKETS):
        if pos + 16 > len(raw):
            break
        ts_sec, ts_sub, caplen, _ = struct.unpack("<IIII", raw[pos : pos + 16])
        pos += 16
        frame = raw[pos : pos + caplen]
        pos += caplen
        if len(frame) < 54:
            continue
        if frame[6:12] != LAB_GUEST_MAC_BYTES or frame[12:14] != b"\x08\x00":
            continue
        ip = frame[14:]
        ihl = (ip[0] & 0x0F) * 4
        if (ip[0] >> 4) != 4 or ihl < 20 or ip[9] != 6 or len(ip) < ihl + 20:
            continue
        dip = socket.inet_ntoa(ip[16:20])
        if dip != dst_ip:
            continue
        tcp = ip[ihl:]
        sport, dport, seq = struct.unpack("!HHI", tcp[:8])
        flags = tcp[13]
        if dport != dst_port or (flags & 0x12) != 0x02:
            continue
        if sport_filter is not None and sport != sport_filter:
            continue
        syns.append({"ts": ts_sec + ts_sub * sub_scale, "sport": sport, "isn": seq})
    return syns


def parse_pcap_syns(pcap_path, dst_ip, dst_port, exclude_sport=None):
    syns = []
    seen = set()
    for entry in parse_pcap_syns_all(pcap_path, dst_ip, dst_port):
        sport = entry["sport"]
        isn = entry["isn"]
        if exclude_sport is not None and sport == exclude_sport:
            continue
        key = (sport, isn)
        if key not in seen:
            seen.add(key)
            syns.append({"sport": sport, "isn": isn})
    return syns


class _LabHttpHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        m = re.match(r"^/blob/(\d+)$", self.path)
        if m:
            size = min(int(m.group(1)), 128 << 20)
            self.send_response(200)
            self.send_header("Content-Length", str(size))
            self.send_header("Connection", "close")
            self.end_headers()
            chunk = b"\0" * 65536
            left = size
            try:
                while left > 0:
                    take = min(left, len(chunk))
                    self.wfile.write(chunk[:take])
                    left -= take
            except OSError:
                return
            return
        body = b"ferrion-lab-ok\n"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            self.wfile.write(body)
        except OSError:
            return

    def do_PUT(self):
        total = min(int(self.headers.get("Content-Length", "0")), 128 << 20)
        got = 0
        t0 = time.monotonic()
        while got < total:
            data = self.rfile.read(min(65536, total - got))
            if not data:
                break
            got += len(data)
        dt = time.monotonic() - t0
        body = f"{got} {dt:.3f}\n".encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            self.wfile.write(body)
        except OSError:
            return

    def log_message(self, *args):
        return


class _TcpEchoServer:
    def __init__(self, host, port):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((host, port))
        self.sock.listen(32)
        self.sock.settimeout(0.5)
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._accept_loop, daemon=True)
        self.thread.start()

    def _accept_loop(self):
        accepted = 0
        for _ in range(MAX_ECHO_STEPS):
            if self.stop.is_set() or accepted >= MAX_ECHO_CONNS:
                return
            try:
                conn, _ = self.sock.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            accepted += 1
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn):
        try:
            conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            conn.settimeout(10.0)
            for _ in range(MAX_ECHO_STEPS):
                data = conn.recv(65536)
                if not data:
                    break
                conn.sendall(data)
        except OSError:
            pass
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def close(self):
        self.stop.set()
        try:
            self.sock.close()
        except OSError:
            pass
        self.thread.join(timeout=2)


class _UdpEchoServer:
    def __init__(self, host, port):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((host, port))
        self.sock.settimeout(0.5)
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.thread.start()

    def _loop(self):
        for _ in range(MAX_ECHO_STEPS):
            if self.stop.is_set():
                return
            try:
                data, addr = self.sock.recvfrom(65536)
            except TimeoutError:
                continue
            except OSError:
                return
            try:
                self.sock.sendto(data, addr)
            except OSError:
                pass

    def close(self):
        self.stop.set()
        try:
            self.sock.close()
        except OSError:
            pass
        self.thread.join(timeout=2)


class _TcpFinCheckServer:
    def __init__(self, host, port):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((host, port))
        self.sock.listen(32)
        self.sock.settimeout(0.5)
        self.stop = threading.Event()
        self.lock = threading.Lock()
        self.outcomes = []
        self.thread = threading.Thread(target=self._accept_loop, daemon=True)
        self.thread.start()

    def _accept_loop(self):
        accepted = 0
        for _ in range(MAX_ECHO_STEPS):
            if self.stop.is_set() or accepted >= MAX_ECHO_CONNS:
                return
            try:
                conn, _ = self.sock.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            accepted += 1
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn):
        outcome = "error"
        try:
            conn.settimeout(3.0)
            got = 0
            for _ in range(64):
                data = conn.recv(64)
                if not data:
                    outcome = "eof" if got > 0 else "empty_eof"
                    break
                got += len(data)
        except ConnectionResetError:
            outcome = "reset"
        except TimeoutError:
            outcome = "timeout"
        except OSError:
            outcome = "error"
        finally:
            with self.lock:
                self.outcomes.append(outcome)
            try:
                conn.close()
            except OSError:
                pass

    def close(self):
        self.stop.set()
        try:
            self.sock.close()
        except OSError:
            pass
        self.thread.join(timeout=2)


class _ScriptedDnsServer:
    def __init__(self, host, alt_host, dns_reply_fn):
        self.host = host
        self.dns_reply = dns_reply_fn
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((host, 53))
        self.sock.settimeout(0.5)
        self.tcp_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.tcp_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.tcp_sock.bind((host, 53))
        self.tcp_sock.listen(16)
        self.tcp_sock.settimeout(0.5)
        self.wrong_port_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.wrong_port_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.wrong_port_sock.bind((host, 5353))
        self.wrong_src_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.wrong_src_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.wrong_src_sock.bind((alt_host, 53))
        self.stop = threading.Event()
        self.queries = []
        self.retry_queries = []
        self.tcp_queries = []
        self.dropped_retry = False
        self.lock = threading.Lock()
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.tcp_thread = threading.Thread(target=self._tcp_loop, daemon=True)
        self.thread.start()
        self.tcp_thread.start()

    @staticmethod
    def _parse_query(data):
        if len(data) < 17:
            return None
        qid = int.from_bytes(data[:2], "big")
        cursor = 12
        labels = []
        while cursor < len(data) and data[cursor]:
            ln = data[cursor]
            labels.append(data[cursor + 1 : cursor + 1 + ln].decode("ascii", "replace"))
            cursor += 1 + ln
        qend = cursor + 5
        if qend > len(data):
            return None
        return qid, ".".join(labels), data[12:qend]

    def _loop(self):
        for _ in range(MAX_ECHO_STEPS):
            if self.stop.is_set():
                return
            try:
                data, addr = self.sock.recvfrom(4096)
            except TimeoutError:
                continue
            except OSError:
                return
            parsed = self._parse_query(data)
            if parsed is None:
                continue
            qid, name, question = parsed
            other = b"\x05other\x07invalid\x00" + question[-4:]
            drop_now = False
            with self.lock:
                entry = {"id": qid, "sport": addr[1], "name": name}
                if name == "retry.lab.invalid":
                    self.retry_queries.append(entry)
                    if not self.dropped_retry:
                        self.dropped_retry = True
                        drop_now = True
                elif name != "tcp.lab.invalid":
                    self.queries.append(entry)
            if drop_now:
                continue
            try:
                if name == "tcp.lab.invalid":
                    tc_reply = struct.pack("!HHHHHH", qid, 0x8380, 1, 0, 0, 0) + question
                    self.sock.sendto(tc_reply, addr)
                    continue
                self.sock.sendto(self.dns_reply(qid ^ 0x5A5A, question, "192.0.2.66"), addr)
                self.sock.sendto(self.dns_reply(qid, other, "192.0.2.67"), addr)
                self.wrong_port_sock.sendto(self.dns_reply(qid, question, "192.0.2.68"), addr)
                self.wrong_src_sock.sendto(self.dns_reply(qid, question, "192.0.2.69"), addr)
                self.sock.sendto(self.dns_reply(qid, question, self.host), addr)
            except OSError:
                pass

    def _tcp_loop(self):
        for _ in range(MAX_ECHO_STEPS):
            if self.stop.is_set():
                return
            try:
                conn, _ = self.tcp_sock.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            try:
                conn.settimeout(5.0)
                hdr = b""
                while len(hdr) < 2:
                    chunk = conn.recv(2 - len(hdr))
                    if not chunk:
                        break
                    hdr += chunk
                if len(hdr) < 2:
                    continue
                msg_len = int.from_bytes(hdr, "big")
                if msg_len == 0 or msg_len > 4096:
                    continue
                body = b""
                while len(body) < msg_len:
                    chunk = conn.recv(msg_len - len(body))
                    if not chunk:
                        break
                    body += chunk
                if len(body) < msg_len:
                    continue
                parsed = self._parse_query(body)
                if parsed is None:
                    continue
                qid, name, question = parsed
                with self.lock:
                    self.tcp_queries.append({"id": qid, "name": name})
                reply = self.dns_reply(qid, question, self.host)
                conn.sendall(struct.pack("!H", len(reply)) + reply)
            except OSError:
                pass
            finally:
                try:
                    conn.close()
                except OSError:
                    pass

    def close(self):
        self.stop.set()
        for s in (self.sock, self.tcp_sock, self.wrong_port_sock, self.wrong_src_sock):
            try:
                s.close()
            except OSError:
                pass
        self.thread.join(timeout=2)
        self.tcp_thread.join(timeout=2)


class NetLab:
    def __init__(
        self,
        out,
        blocked_cls,
        dns_reply_fn,
        *,
        enable_dnsmasq=True,
        enable_dns=True,
        enable_iperf=False,
        enable_lo_helpers=False,
        enable_fin_check=False,
    ):
        self.out = Path(out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.blocked_cls = blocked_cls
        self.dns_reply_fn = dns_reply_fn
        self.enable_dnsmasq = enable_dnsmasq
        self.enable_dns = enable_dns
        self.enable_iperf = enable_iperf
        self.enable_lo_helpers = enable_lo_helpers
        self.enable_fin_check = enable_fin_check
        self.procs = []
        self.iperf_servers = {}
        self.servers = []
        self.httpds = []
        self.dns = None
        self.fin_server = None
        self.tap_ready = False

    def start(self):
        run_cmd(["ip", "link", "set", "lo", "up"])
        run_cmd(["ip", "addr", "replace", f"{LAB_INET_HOST}/32", "dev", "lo"])
        run_cmd(["ip", "link", "del", "tap0"], check=False)
        res = run_cmd(["ip", "tuntap", "add", "dev", "tap0", "mode", "tap"], check=False)
        if res.returncode != 0:
            raise self.blocked_cls(f"tap device unavailable (tun module): {(res.stderr or res.stdout).strip()}")
        self.tap_ready = True
        run_cmd(["ip", "addr", "add", f"{LAB_HOST}/24", "dev", "tap0"])
        run_cmd(["ip", "addr", "add", f"{LAB_ALT_HOST}/24", "dev", "tap0"])
        run_cmd(["ip", "link", "set", "tap0", "up"])
        netem = run_cmd(["tc", "qdisc", "add", "dev", "tap0", "root", "netem", "delay", "1ms"], check=False)
        if netem.returncode != 0:
            raise self.blocked_cls(f"tc netem unavailable (sch_netem module): {(netem.stderr or netem.stdout).strip()}")
        run_cmd(["tc", "qdisc", "del", "dev", "tap0", "root"], check=False)

        if self.enable_dnsmasq:
            dnsmasq_log = open(self.out / "dnsmasq.log", "w")
            dnsmasq = subprocess.Popen(
                [
                    "dnsmasq",
                    "--no-daemon",
                    "--conf-file=/dev/null",
                    "--user=root",
                    "--group=root",
                    "--port=0",
                    "--bind-interfaces",
                    "--interface=tap0",
                    f"--dhcp-range={LAB_DHCP_RANGE}",
                    f"--dhcp-option=3,{LAB_HOST}",
                    f"--dhcp-option=6,{LAB_HOST}",
                    f"--dhcp-leasefile={self.out / 'leases'}",
                    "--pid-file=",
                    "--log-dhcp",
                ],
                stdout=dnsmasq_log,
                stderr=subprocess.STDOUT,
            )
            self.procs.append((dnsmasq, dnsmasq_log))
            time.sleep(0.2)
            if dnsmasq.poll() is not None:
                raise RuntimeError(f"dnsmasq exited early with {dnsmasq.returncode}; see {self.out / 'dnsmasq.log'}")

        hosts = [LAB_HOST]
        if self.enable_lo_helpers:
            hosts.append("127.0.0.1")
        for host in hosts:
            httpd = http.server.ThreadingHTTPServer((host, HTTP_PORT), _LabHttpHandler)
            httpd.daemon_threads = True
            threading.Thread(target=httpd.serve_forever, daemon=True).start()
            self.httpds.append(httpd)
            self.servers.append(_TcpEchoServer(host, TCP_ECHO_PORT))
            self.servers.append(_UdpEchoServer(host, UDP_ECHO_PORT))
            if self.enable_iperf:
                self._spawn_iperf(host)

        if self.enable_fin_check:
            self.fin_server = _TcpFinCheckServer(LAB_HOST, TCP_FIN_PORT)
            self.servers.append(self.fin_server)

        if self.enable_dns:
            self.dns = _ScriptedDnsServer(LAB_HOST, LAB_ALT_HOST, self.dns_reply_fn)

    def _spawn_iperf(self, host):
        logf = open(self.out / f"iperf3-server-{host}.log", "a")
        proc = subprocess.Popen(
            ["iperf3", "-s", "-B", host, "-p", str(IPERF_PORT)],
            stdout=logf,
            stderr=subprocess.STDOUT,
        )
        self.procs.append((proc, logf))
        self.iperf_servers[host] = proc

    def ensure_iperf(self):
        if not self.enable_iperf:
            return
        for host, proc in list(self.iperf_servers.items()):
            if proc.poll() is not None:
                self._spawn_iperf(host)
                time.sleep(0.2)

    def qemu_flags(self, card_device, pcap_path=None):
        flags = (
            "-netdev tap,id=net0,ifname=tap0,script=no,downscript=no "
            f"-device {card_device},netdev=net0,mac={LAB_GUEST_MAC}"
        )
        if pcap_path is not None:
            flags += f" -object filter-dump,id=f1,netdev=net0,file={Path(pcap_path).resolve()}"
        return flags

    def set_netem(self, delay_ms=None, loss_pct=None):
        run_cmd(["tc", "qdisc", "del", "dev", "tap0", "root"], check=False)
        if delay_ms is None and loss_pct is None:
            return
        cmd = ["tc", "qdisc", "add", "dev", "tap0", "root", "netem"]
        if delay_ms is not None:
            cmd.extend(["delay", f"{delay_ms}ms"])
        if loss_pct is not None:
            cmd.extend(["loss", f"{loss_pct}%"])
        cmd.extend(["limit", "10000"])
        run_cmd(cmd)

    def close(self):
        if self.dns is not None:
            self.dns.close()
        for srv in self.servers:
            srv.close()
        for httpd in self.httpds:
            try:
                httpd.shutdown()
                httpd.server_close()
            except OSError:
                pass
        for proc, logf in self.procs:
            try:
                if proc.poll() is None:
                    proc.terminate()
                    proc.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                try:
                    proc.kill()
                    proc.wait(timeout=5)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            try:
                logf.close()
            except OSError:
                pass
        if self.tap_ready:
            run_cmd(["tc", "qdisc", "del", "dev", "tap0", "root"], check=False)
            run_cmd(["ip", "link", "del", "tap0"], check=False)


def wait_dhcp_address(vm, run_status, prefix="10.77.0."):
    for _ in range(MAX_DHCP_POLLS):
        _, out = run_status(
            vm,
            "cat /scheme/netcfg/ifaces/eth0/addr/list; echo ---; cat /etc/net/dns",
            30,
        )
        m = re.search(rf"({re.escape(prefix)}\d+)/\d+", out)
        if m:
            return m.group(1), out
        time.sleep(1.0)
    return None, ""


def host_ping(addr, count, interval=0.05, size=None, df=False, timeout=30):
    cmd = ["ping", "-c", str(count), "-i", str(interval), "-W", "1", "-q"]
    if df:
        cmd.extend(["-M", "do"])
    if size is not None:
        cmd.extend(["-s", str(size)])
    cmd.append(addr)
    res = run_cmd(cmd, check=False, timeout=timeout)
    out = res.stdout + res.stderr
    m = re.search(r"(\d+) packets transmitted, (\d+) received", out)
    sent, recv = (int(m.group(1)), int(m.group(2))) if m else (count, 0)
    rtt = re.search(r"rtt min/avg/max/mdev = ([\d.]+)/([\d.]+)/([\d.]+)/([\d.]+) ms", out)
    stats = (
        {
            "min_ms": float(rtt.group(1)),
            "avg_ms": float(rtt.group(2)),
            "max_ms": float(rtt.group(3)),
            "mdev_ms": float(rtt.group(4)),
        }
        if rtt
        else None
    )
    return {"sent": sent, "received": recv, "rtt": stats, "raw": out.strip()}


def run_net_basics(args, out, ctx):
    card = ctx["net_card"](args)
    if card not in ctx["NET_CARDS"]:
        return False, f"net-basics runs with net=e1000 or net=virtio, not net={card}"
    driver = "virtio-netd" if card == "virtio" else "e1000d"
    require_tools(LAB_BASE_TOOLS, ctx["Blocked"])
    deadline_str, deadline_ts = parse_window_deadline(getattr(args, "window_deadline", None))
    load_start = [round(v, 2) for v in os.getloadavg()]
    items = {}
    boots_syns = []

    def timed_guard_start():
        return (
            os.getloadavg()[0],
            list_foreign_qemus(),
            sample_own_qemu_priority(),
        )

    def timed_guard_end(start_state):
        load_before, f_before, q_before = start_state
        load_after = os.getloadavg()[0]
        f_after = list_foreign_qemus()
        q_after = sample_own_qemu_priority()
        max_l = round(max(load_before, load_after), 2)
        seen = {}
        for entry in [*f_before, *f_after]:
            seen[(entry["pid"], entry["started_at"])] = entry
        foreign = [seen[k] for k in sorted(seen)]
        q_summary = summarize_priority_samples([*q_before, *q_after])
        after_deadline = deadline_ts is not None and time.time() > deadline_ts
        reasons = []
        if max_l > LOAD_LIMIT:
            reasons.append("load")
        if foreign:
            reasons.append("foreign_qemu")
        if after_deadline:
            reasons.append("window_deadline")
        if q_summary is None:
            reasons.append("priority_unrecorded")
        elif not q_summary["constant"]:
            reasons.append("nice_changed")
        return {
            "max_load1": max_l,
            "foreign_qemu": foreign,
            "qemu_priority": q_summary,
            "ended_after_deadline": after_deadline,
            "host_limited_reasons": reasons,
            "host_limited": bool(reasons),
        }

    boot1_pcap = None
    for boot in (1, 2):
        boot_out = out / f"boot-{boot}"
        boot_out.mkdir(parents=True, exist_ok=True)
        pcap_path = boot_out / "net.pcap"
        if boot == 1:
            boot1_pcap = pcap_path
        lab = NetLab(
            boot_out,
            ctx["Blocked"],
            ctx["dns_reply"],
            enable_dns=(boot == 1),
            enable_fin_check=(boot == 1),
        )
        vm = None
        try:
            lab.start()
            vm = ctx["VM"](
                args,
                boot_out,
                qemu_flags=lab.qemu_flags(ctx["NET_CARDS"][card], pcap_path),
                make=("net=no",),
            )
            if not ctx["root_shell"](vm, args.timeout):
                return False, f"boot {boot}: no root shell on serial console; see {boot_out}"
            guest_ip, netcfg_out = wait_dhcp_address(vm, ctx["run_status"])
            (boot_out / "dhcp.txt").write_text(netcfg_out)
            if not guest_ip:
                return False, f"boot {boot}: guest did not obtain 10.77.0.x DHCP lease; see {boot_out}"

            ctx["run_status"](
                vm,
                f"bash -c 'for i in $(seq 20); do net-probe connect {LAB_HOST}:{CLOSED_PORT} --timeout-ms 500 >/dev/null; done'",
                120,
            )

            if boot == 1:
                rows_before = context_snapshot(vm, ctx["run_status"])
                drv_before = next((r for r in rows_before if r["name"] == driver), None)
                ping_1472 = host_ping(guest_ip, count=3, interval=0.2, size=1472, df=True, timeout=20)
                _, udp_out = ctx["run_status"](
                    vm,
                    f"net-probe udp-send {LAB_HOST}:{UDP_ECHO_PORT} --bytes 1472 --count 3 --expect-echo --timeout-ms 3000",
                    45,
                )
                udp_json = parse_json_line(udp_out) or {"ok": False, "raw": udp_out.strip()}
                rows_after = context_snapshot(vm, ctx["run_status"])
                drv_after = next((r for r in rows_after if r["name"] == driver), None)
                pid_same = drv_before is not None and drv_after is not None and drv_before["pid"] == drv_after["pid"]
                items["1_full_size_frames"] = {
                    "pass": ping_1472["received"] == 3 and bool(udp_json.get("ok")) and pid_same,
                    "echo_1472_received": ping_1472["received"],
                    "udp_1472": udp_json,
                    "driver": driver,
                    "driver_pid_before": drv_before["pid"] if drv_before else None,
                    "driver_pid_after": drv_after["pid"] if drv_after else None,
                    "driver_survived": pid_same,
                }

                g2 = timed_guard_start()
                _, http_out = ctx["run_status"](
                    vm,
                    f"net-probe http-bench {LAB_HOST}:{HTTP_PORT} /ping --count 20 --interval-ms 1000 --timeout-ms 5000",
                    90,
                )
                g2_res = timed_guard_end(g2)
                http_json = parse_json_line(http_out) or {"ok": False, "raw": http_out.strip()}
                items["2_idle_http_rtt"] = {
                    "pass": (not g2_res["host_limited"])
                    and bool(http_json.get("ok"))
                    and http_json.get("total_p50_ms", 9999.0) < 10.0
                    and http_json.get("total_max_ms", 9999.0) <= 100.0,
                    "blocked": g2_res["host_limited"],
                    "result": http_json,
                    **g2_res,
                }

                g3 = timed_guard_start()
                lab.set_netem(delay_ms=50)
                try:
                    blob_bytes = 16 * 1024 * 1024
                    _, dl_out = ctx["run_status"](
                        vm,
                        f'curl -s -o /dev/null --max-time 240 -w "%{{size_download}} %{{time_total}} %{{speed_download}}\\n" http://{LAB_HOST}:{HTTP_PORT}/blob/{blob_bytes}',
                        260,
                    )
                finally:
                    lab.set_netem(None)
                g3_res = timed_guard_end(g3)
                m = re.search(r"(\d+)\s+([\d.]+)\s+([\d.]+)", dl_out)
                if m:
                    dl_size = int(m.group(1))
                    dl_time = float(m.group(2))
                    dl_mbps = float(m.group(3)) / 1e6
                else:
                    dl_size, dl_time, dl_mbps = 0, 0.0, 0.0
                items["3_delayed_download_50ms"] = {
                    "pass": (not g3_res["host_limited"])
                    and dl_size == 16 * 1024 * 1024
                    and dl_mbps >= 4.0,
                    "blocked": g3_res["host_limited"],
                    "size_bytes": dl_size,
                    "time_s": round(dl_time, 3),
                    "mb_per_s": round(dl_mbps, 4),
                    "target_mb_per_s": 4.0,
                    **g3_res,
                }

                lookup_answers = []
                for idx in range(20):
                    _, lout = ctx["run_status"](vm, f"net-probe lookup q{idx}.lab.invalid", 30)
                    ljson = parse_json_line(lout) or {"ok": False, "addrs": []}
                    lookup_answers.append(ljson.get("addrs", []))
                good_lookups = sum(1 for a in lookup_answers if a == [LAB_HOST])
                _, retry_out = ctx["run_status"](vm, "net-probe lookup retry.lab.invalid", 30)
                retry_json = parse_json_line(retry_out) or {"ok": False, "addrs": []}
                _, tcp_out = ctx["run_status"](vm, "net-probe lookup tcp.lab.invalid", 30)
                tcp_json = parse_json_line(tcp_out) or {"ok": False, "addrs": []}
                with lab.dns.lock:
                    dns_queries = list(lab.dns.queries)
                    retry_queries = list(lab.dns.retry_queries)
                    tcp_queries = list(lab.dns.tcp_queries)
                retry_ok = (
                    retry_json.get("addrs") == [LAB_HOST]
                    and len(retry_queries) >= 2
                    and retry_queries[0]["id"] != retry_queries[1]["id"]
                    and retry_queries[0]["sport"] != retry_queries[1]["sport"]
                )
                tcp_ok = tcp_json.get("addrs") == [LAB_HOST] and any(
                    q["name"] == "tcp.lab.invalid" for q in tcp_queries
                )
                items["5_dns_reply_match"] = {
                    "pass": good_lookups == 20
                    and ctx["consecutive_run"]([q["sport"] for q in dns_queries]) < 3
                    and len({q["id"] for q in dns_queries}) == len(dns_queries)
                    and retry_ok
                    and tcp_ok,
                    "matched": good_lookups,
                    "total": 20,
                    "queries_seen": len(dns_queries),
                    "retry_ok": retry_ok,
                    "retry_queries": retry_queries,
                    "tcp_ok": tcp_ok,
                    "tcp_queries": tcp_queries,
                }

                ping_100 = host_ping(guest_ip, count=100, interval=0.05, timeout=30)
                items["6_ping_100"] = {
                    "pass": ping_100["received"] == 100,
                    "sent": ping_100["sent"],
                    "received": ping_100["received"],
                    "rtt": ping_100["rtt"],
                }

                g7 = timed_guard_start()
                _, opt_out = ctx["run_status"](
                    vm,
                    "net-probe sockopts tcp --nodelay 1 --keepalive 1 --rcvbuf 262144 --sndbuf 262144 --rcvtimeo-ms 1500 --sndtimeo-ms 1500",
                    30,
                )
                opt_json = parse_json_line(opt_out) or {"ok": False, "raw": opt_out.strip()}
                _, echo_out = ctx["run_status"](
                    vm,
                    f"net-probe echo-bench tcp {LAB_HOST}:{TCP_ECHO_PORT} --count 200 --bytes 1 --nodelay --timeout-ms 5000",
                    120,
                )
                g7_res = timed_guard_end(g7)
                echo_json = parse_json_line(echo_out) or {"ok": False, "raw": echo_out.strip()}
                nodelay_val = opt_json.get("get", {}).get("nodelay", {}).get("value")
                items["7_tcp_nodelay_1b_echo"] = {
                    "pass": (not g7_res["host_limited"])
                    and bool(echo_json.get("ok"))
                    and echo_json.get("p99_ms", 9999.0) < 5.0
                    and nodelay_val == 1,
                    "blocked": g7_res["host_limited"],
                    "echo": echo_json,
                    "sockopts": opt_json,
                    **g7_res,
                }

                _, so_out = ctx["run_status"](
                    vm,
                    f"net-probe connect {LAB_HOST}:{CLOSED_PORT} --nonblock --timeout-ms 3000",
                    30,
                )
                so_json = parse_json_line(so_out) or {"ok": False, "raw": so_out.strip()}
                items["8_so_error_closed_port"] = {
                    "pass": so_json.get("so_error_name") == "ECONNREFUSED",
                    "result": so_json,
                }

                install_iperf_watchdog(vm, ctx["run_status"])
                ts_before = read_timeout_subscribers(vm, ctx["run_status"])
                ctx["run_status"](
                    vm,
                    "bash -c 'iperf3 -s -4 -B 127.0.0.1 -p 5202 -1 </dev/null >/tmp/nb-lo-srv.log 2>&1 & echo $! > /tmp/nb-lo.pid'; sleep 1",
                    30,
                )
                lo_rc, lo_out = ctx["run_status"](
                    vm,
                    "bash /tmp/ip.sh 20 iperf3 -4 -c 127.0.0.1 -p 5202 -t 2 -J",
                    45,
                )
                ctx["run_status"](vm, "bash -c 'kill $(cat /tmp/nb-lo.pid) 2>/dev/null || true'", 15)
                ts_imm = read_timeout_subscribers(vm, ctx["run_status"])
                time.sleep(5)
                ts_after_5s = read_timeout_subscribers(vm, ctx["run_status"])
                ts_delta = (
                    (ts_after_5s - ts_before)
                    if (ts_before is not None and ts_after_5s is not None)
                    else None
                )
                ts_ok = ts_delta is not None and ts_delta <= MAX_TIMEOUT_SUBS_MARGIN
                lo_data = parse_iperf_json(lo_out)
                lo_bps = (
                    (lo_data or {}).get("end", {}).get("sum_received", {}).get("bits_per_second")
                    or (lo_data or {}).get("end", {}).get("sum_sent", {}).get("bits_per_second")
                )
                lo_mbps = round(lo_bps / 8e6, 4) if lo_bps is not None else None
                items["10_tcp_loopback_2s"] = {
                    "pass": lo_rc == 0
                    and lo_data is not None
                    and not lo_data.get("error")
                    and lo_mbps is not None
                    and lo_mbps > 0.0
                    and ts_ok,
                    "rc": lo_rc,
                    "mb_per_s": lo_mbps,
                    "timeout_subs_before": ts_before,
                    "timeout_subs_imm": ts_imm,
                    "timeout_subs_after_5s": ts_after_5s,
                    "timeout_subs_delta": ts_delta,
                    "timeout_subs_margin": MAX_TIMEOUT_SUBS_MARGIN,
                    "error": (lo_data or {}).get("error") if isinstance(lo_data, dict) else lo_out.strip()[-160:],
                }

                ctx["run_status"](
                    vm,
                    f"bash -c 'exec 3<>/scheme/tcp/{LAB_HOST}:{HTTP_PORT}/0.0.0.0:{FIXED_ISN_SPORT}; printf \"GET /ping HTTP/1.1\\r\\nHost: {LAB_HOST}\\r\\nConnection: close\\r\\n\\r\\n\" >&3; sleep 0.3; exec 3<&-; sleep 2.5; exec 3<>/scheme/tcp/{LAB_HOST}:{HTTP_PORT}/0.0.0.0:{FIXED_ISN_SPORT}; sleep 0.3; exec 3<&-'",
                    30,
                )

                serial_path = boot_out / "serial.log"
                serial_before_len = serial_path.stat().st_size if serial_path.exists() else 0
                ns_before_rows = context_snapshot(vm, ctx["run_status"])
                ns_before = next((r for r in ns_before_rows if r["name"] == "netstack"), None)
                ctx["run_status"](
                    vm,
                    f"bash -c 'rm -f /tmp/nw-acc-ready /tmp/nw-acc.log; net-probe bind tcp 0.0.0.0:{ACCEPT_PROBE_PORT} --echo --max-conns {MAX_ACCEPT_PROBES} --timeout-ms 15000 --ready-file /tmp/nw-acc-ready >/tmp/nw-acc.log 2>&1 & echo $! > /tmp/nw-acc.pid; for i in $(seq 50); do [ -f /tmp/nw-acc-ready ] && break; sleep 0.1; done; cat /tmp/nw-acc-ready'",
                    30,
                )
                acc_ok = 0
                acc_errs = []
                for _ in range(MAX_ACCEPT_PROBES):
                    try:
                        with socket.create_connection((guest_ip, ACCEPT_PROBE_PORT), timeout=5.0) as sock:
                            sock.settimeout(5.0)
                            sock.sendall(b"a")
                            got = sock.recv(1)
                            if got == b"a":
                                acc_ok += 1
                            else:
                                acc_errs.append(f"short recv {got!r}")
                                break
                    except OSError as exc:
                        acc_errs.append(str(exc))
                        break
                _, acc_wait_out = ctx["run_status"](
                    vm,
                    "bash -c 'p=$(cat /tmp/nw-acc.pid 2>/dev/null); for i in $(seq 50); do kill -0 \"$p\" 2>/dev/null || break; sleep 0.1; done; kill \"$p\" 2>/dev/null || true; cat /tmp/nw-acc.log'",
                    30,
                )
                ns_after_rows = context_snapshot(vm, ctx["run_status"])
                ns_after = next((r for r in ns_after_rows if r["name"] == "netstack"), None)
                serial_tail = serial_path.read_bytes()[serial_before_len:] if serial_path.exists() else b""
                no_ref_warn = b"Closing a socket_handle with no ref" in serial_tail
                mem_growth = (
                    (ns_after["private_bytes"] - ns_before["private_bytes"])
                    if (ns_before is not None and ns_after is not None)
                    else None
                )
                ns_pid_same = (
                    ns_before is not None
                    and ns_after is not None
                    and ns_before["pid"] == ns_after["pid"]
                )
                items["12_accept_memory_bound"] = {
                    "pass": acc_ok == MAX_ACCEPT_PROBES
                    and ns_pid_same
                    and mem_growth is not None
                    and mem_growth < MAX_ACCEPT_MEM_GROWTH_BYTES
                    and not no_ref_warn,
                    "conns_ok": acc_ok,
                    "conns_target": MAX_ACCEPT_PROBES,
                    "mem_before_bytes": ns_before["private_bytes"] if ns_before else None,
                    "mem_after_bytes": ns_after["private_bytes"] if ns_after else None,
                    "mem_growth_bytes": mem_growth,
                    "mem_bound_bytes": MAX_ACCEPT_MEM_GROWTH_BYTES,
                    "no_ref_warning": no_ref_warn,
                    "netstack_pid_before": ns_before["pid"] if ns_before else None,
                    "netstack_pid_after": ns_after["pid"] if ns_after else None,
                    "errors": acc_errs,
                    "guest_log": acc_wait_out.strip()[-200:],
                }

                ctx["run_status"](
                    vm,
                    f"bash -c 'for i in $(seq {MAX_FIN_PROBES}); do exec 3<>/scheme/tcp/{LAB_HOST}:{TCP_FIN_PORT}; printf fin >&3; sleep 0.05; exec 3<&-; done'",
                    60,
                )
                time.sleep(3.5)
                with lab.fin_server.lock:
                    fin_outcomes = list(lab.fin_server.outcomes)
                fin_eof = sum(1 for o in fin_outcomes if o == "eof")
                items["13_tcp_close_fin"] = {
                    "pass": len(fin_outcomes) == MAX_FIN_PROBES and fin_eof == MAX_FIN_PROBES,
                    "eof": fin_eof,
                    "total": len(fin_outcomes),
                    "target": MAX_FIN_PROBES,
                    "outcomes": fin_outcomes,
                }
            vm.kill()
        finally:
            if vm is not None:
                vm.close()
            lab.close()

        syns = parse_pcap_syns(pcap_path, LAB_HOST, CLOSED_PORT, exclude_sport=FIXED_ISN_SPORT)
        boots_syns.append(syns)

    fixed_syns = (
        parse_pcap_syns_all(boot1_pcap, LAB_HOST, HTTP_PORT, sport_filter=FIXED_ISN_SPORT)
        if boot1_pcap is not None and boot1_pcap.exists()
        else []
    )
    if len(fixed_syns) >= 2:
        dt = fixed_syns[-1]["ts"] - fixed_syns[0]["ts"]
        delta = (fixed_syns[-1]["isn"] - fixed_syns[0]["isn"]) & 0xFFFFFFFF
        expected_ticks = dt * 1_000_000.0 / 4.0
        isn_clock_ok = dt >= 1.5 and 0.5 * expected_ticks <= delta <= 2.0 * expected_ticks
    else:
        dt, delta, expected_ticks, isn_clock_ok = 0.0, None, None, False
    items["11_isn_clock_advance"] = {
        "pass": isn_clock_ok,
        "syns_seen": len(fixed_syns),
        "elapsed_s": round(dt, 3),
        "isn_delta": delta,
        "expected_ticks": round(expected_ticks, 1) if expected_ticks is not None else None,
        "syns": fixed_syns,
    }

    isn_ok = True
    isn_notes = []
    for idx, syns in enumerate(boots_syns, 1):
        if len(syns) < 20:
            isn_ok = False
            isn_notes.append(f"boot {idx}: saw {len(syns)}/20 SYNs")
            continue
        run_len = ctx["consecutive_run"]([s["sport"] for s in syns])
        if run_len >= 3:
            isn_ok = False
            isn_notes.append(f"boot {idx}: consecutive source port run {run_len}")
        isns = [s["isn"] for s in syns]
        if ctx["pcg_prediction"](isns) is not None:
            isn_ok = False
            isn_notes.append(f"boot {idx}: ISNs predicted by PCG")
    if len(boots_syns) == 2 and boots_syns[0] and boots_syns[1]:
        set0 = {s["isn"] for s in boots_syns[0]}
        set1 = {s["isn"] for s in boots_syns[1]}
        if boots_syns[0][0]["isn"] == boots_syns[1][0]["isn"] or (set0 & set1):
            isn_ok = False
            isn_notes.append("ISNs repeated across boots")
    items["4_isn_and_ports"] = {
        "pass": isn_ok,
        "boot1_first_isn": boots_syns[0][0]["isn"] if boots_syns and boots_syns[0] else None,
        "boot2_first_isn": boots_syns[1][0]["isn"] if len(boots_syns) > 1 and boots_syns[1] else None,
        "boot1_syn_count": len(boots_syns[0]) if boots_syns else 0,
        "boot2_syn_count": len(boots_syns[1]) if len(boots_syns) > 1 else 0,
        "notes": isn_notes,
    }

    hardening_result = out.parent / "net-hardening-1" / "result.json"
    img_path = ROOT / "build" / args.arch / args.config / "harddrive.img"
    arp_verdict = None
    arp_pass = False
    arp_reason = "net-hardening not run on this image and card"
    if hardening_result.exists() and img_path.exists():
        try:
            hdata = json.loads(hardening_result.read_text())
            hcard = "e1000"
            for entry in hdata.get("make", []):
                if isinstance(entry, str) and entry.startswith("net="):
                    hcard = entry.split("=", 1)[1]
            fresh = hardening_result.stat().st_mtime > img_path.stat().st_mtime
            arp_verdict = {
                "verdict": hdata.get("verdict"),
                "note": hdata.get("note"),
                "card": hcard,
                "newer_than_image": fresh,
            }
            if fresh and hcard == card and hdata.get("verdict") == "PASS":
                arp_pass = True
                arp_reason = None
        except (OSError, json.JSONDecodeError):
            arp_verdict = None
    items["9_arp_cache_bound"] = {
        "pass": arp_pass,
        "covered_by": "net-hardening",
        "reason": arp_reason,
        "net_hardening": arp_verdict,
    }

    ordered_keys = sorted(items.keys(), key=lambda k: int(k.split("_", 1)[0]))
    blocked_items = [k for k in ordered_keys if items[k].get("blocked")]
    report = {
        "card": card,
        "driver": driver,
        "vhost": "off",
        "window_deadline": deadline_str,
        "loadavg_start": load_start,
        "loadavg_end": [round(v, 2) for v in os.getloadavg()],
        "items": {k: items[k] for k in ordered_keys},
    }
    (out / "net-basics.json").write_text(json.dumps(report, indent=2) + "\n")
    lines = []
    for key in ordered_keys:
        status = "BLOCKED" if items[key].get("blocked") else ("PASS" if items[key]["pass"] else "FAIL")
        lines.append(f"{status} {key}")
    (out / "net-basics.txt").write_text("\n".join(lines) + "\n")
    passed = [k for k in ordered_keys if items[k]["pass"]]
    failed = [k for k in ordered_keys if not items[k]["pass"] and not items[k].get("blocked")]
    summary = (
        f"card {card}: {len(passed)}/{len(ordered_keys)} green "
        f"(green: {', '.join(passed) or 'none'}; red: {', '.join(failed) or 'none'}"
        + (f"; blocked: {', '.join(blocked_items)}" if blocked_items else "")
        + f"); see {out / 'net-basics.json'}"
    )
    if blocked_items and not failed:
        raise ctx["Blocked"](summary)
    return not failed and not blocked_items, summary


def run_net_no_lease(args, out, ctx):
    card = ctx["net_card"](args)
    if card not in ctx["NET_CARDS"]:
        return False, f"net-no-lease runs with net=e1000 or net=virtio, not net={card}"
    require_tools(LAB_BASE_TOOLS, ctx["Blocked"])
    boot_out = out / "boot-1"
    boot_out.mkdir(parents=True, exist_ok=True)
    lab = NetLab(boot_out, ctx["Blocked"], ctx["dns_reply"], enable_dnsmasq=False, enable_dns=False)
    vm = None
    try:
        lab.start()
        vm = ctx["VM"](
            args,
            boot_out,
            qemu_flags=lab.qemu_flags(ctx["NET_CARDS"][card], pcap_path=None),
            make=("net=no",),
        )
        if not ctx["root_shell"](vm, args.timeout):
            return False, f"no root shell on serial console; see {boot_out}"
        _, addr_out = ctx["run_status"](
            vm,
            "bash -c 'for i in $(seq 80); do grep -q dhcpd /scheme/sys/context || break; sleep 0.2; done; cat /scheme/netcfg/ifaces/eth0/addr/list'",
            45,
        )
        rows_before = context_snapshot(vm, ctx["run_status"])
        ns_before = next((r for r in rows_before if r["name"] == "netstack"), None)
        _, conn_out = ctx["run_status"](
            vm,
            f"net-probe connect {LAB_HOST}:{HTTP_PORT} --timeout-ms 3000",
            30,
        )
        conn_json = parse_json_line(conn_out) or {"ok": False, "raw": conn_out.strip()}
        rows_after = context_snapshot(vm, ctx["run_status"])
        ns_after = next((r for r in rows_after if r["name"] == "netstack"), None)
        vm.kill()
    finally:
        if vm is not None:
            vm.close()
        lab.close()

    unconfigured = "Not configured" in addr_out
    pid_same = ns_before is not None and ns_after is not None and ns_before["pid"] == ns_after["pid"]
    conn_errored = not bool(conn_json.get("ok"))
    report = {
        "card": card,
        "addr_list": addr_out.strip(),
        "unconfigured": unconfigured,
        "connect": conn_json,
        "netstack_before": ns_before,
        "netstack_after": ns_after,
        "netstack_survived": pid_same,
        "pass": unconfigured and conn_errored and pid_same,
    }
    (out / "net-no-lease.json").write_text(json.dumps(report, indent=2) + "\n")
    summary = (
        f"card {card}: unconfigured={unconfigured} conn_err={conn_json.get('error')} "
        f"netstack_pid={ns_before['pid'] if ns_before else None}->{ns_after['pid'] if ns_after else None}; "
        f"see {out / 'net-no-lease.json'}"
    )
    return report["pass"], summary


def parse_iperf_json(text):
    start = text.find("{")
    end = text.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        return json.loads(text[start : end + 1])
    except json.JSONDecodeError:
        return None


def install_iperf_watchdog(vm, run_status):
    lines = [
        "rm -f /tmp/ip.sh",
        "printf '%s\\n' '#!/usr/bin/env bash' > /tmp/ip.sh",
        "printf '%s\\n' 't=\"$1\"; shift' >> /tmp/ip.sh",
        "printf '%s\\n' 'rm -f /tmp/ip.out' >> /tmp/ip.sh",
        "printf '%s\\n' '\"$@\" >/tmp/ip.out 2>&1 &' >> /tmp/ip.sh",
        "printf '%s\\n' 'p=$!; i=0' >> /tmp/ip.sh",
        "printf '%s\\n' 'while kill -0 \"$p\" 2>/dev/null; do' >> /tmp/ip.sh",
        "printf '%s\\n' '  if [ \"$i\" -ge \"$t\" ]; then' >> /tmp/ip.sh",
        "printf '%s\\n' '    kill -9 \"$p\" 2>/dev/null' >> /tmp/ip.sh",
        "printf '%s\\n' '    cat /tmp/ip.out; exit 124' >> /tmp/ip.sh",
        "printf '%s\\n' '  fi' >> /tmp/ip.sh",
        "printf '%s\\n' '  sleep 1; i=$((i+1))' >> /tmp/ip.sh",
        "printf '%s\\n' 'done' >> /tmp/ip.sh",
        "printf '%s\\n' 'wait \"$p\"; rc=$?' >> /tmp/ip.sh",
        "printf '%s\\n' 'cat /tmp/ip.out; exit \"$rc\"' >> /tmp/ip.sh",
    ]
    for line in lines:
        run_status(vm, line, 15)


def run_iperf_series(
    vm,
    run_status,
    cmd_base,
    driver,
    runs=3,
    is_udp=False,
    watchdog_s=35,
    fail_fast_timeout=False,
    lab=None,
    expected=None,
    fail_status="fail",
):
    samples = []
    errors = []
    cpu_samples = []
    iperf_cpu_samples = []
    load_peaks = []
    qemu_prios = []
    iperf_prios = []
    wrapped = f"bash /tmp/ip.sh {watchdog_s} {cmd_base}"
    timeout = watchdog_s + 25
    max_attempts = max(runs, MAX_IPERF_ATTEMPTS)
    attempts = 0
    for _ in range(max_attempts):
        if len(samples) >= runs:
            break
        attempts += 1
        if lab is not None:
            lab.ensure_iperf()
        check_foreign_qemus()
        qemu_prios.extend(sample_own_qemu_priority())
        if lab is not None:
            iperf_prios.extend(sample_iperf_priority(lab))
        load_before = os.getloadavg()[0]
        ctx_before = context_snapshot(vm, run_status)
        status, out = run_status(vm, wrapped, timeout)
        if status is None:
            vm.type("\x03\n")
            time.sleep(0.5)
        ctx_after = context_snapshot(vm, run_status)
        load_after = os.getloadavg()[0]
        load_peaks.append(round(max(load_before, load_after), 2))
        check_foreign_qemus()
        qemu_prios.extend(sample_own_qemu_priority())
        if lab is not None:
            iperf_prios.extend(sample_iperf_priority(lab))
        cpu_samples.append(cpu_delta(ctx_before, ctx_after, ("netstack", driver, "iperf3")))
        data = parse_iperf_json(out)
        if status == 124:
            errors.append(f"timed out after {watchdog_s}s")
            if fail_fast_timeout and not samples:
                break
            continue
        if status != 0 or not data or data.get("error"):
            err = (data or {}).get("error") if isinstance(data, dict) else None
            errors.append(err or f"exit {status}: {out.strip()[-200:]}")
            if fail_fast_timeout and not samples:
                break
            continue
        test_start = data.get("start", {}).get("test_start", {})
        param_errors = []
        for key, want in (expected or {}).items():
            got = test_start.get(key)
            if got != want:
                param_errors.append(f"test_start.{key}={got!r} != {want!r}")
        if param_errors:
            errors.append("; ".join(param_errors))
            continue
        end = data.get("end", {})
        cpu_pct = end.get("cpu_utilization_percent")
        if isinstance(cpu_pct, dict):
            iperf_cpu_samples.append(
                {
                    "host_total": round(float(cpu_pct.get("host_total", 0.0)), 2),
                    "host_user": round(float(cpu_pct.get("host_user", 0.0)), 2),
                    "host_system": round(float(cpu_pct.get("host_system", 0.0)), 2),
                    "remote_total": round(float(cpu_pct.get("remote_total", 0.0)), 2),
                }
            )
        if is_udp:
            summary = end.get("sum", {})
            bps = summary.get("bits_per_second")
            if bps is not None:
                samples.append(
                    {
                        "mb_per_s": round(bps / 8e6, 4),
                        "lost_percent": round(float(summary.get("lost_percent", 0.0)), 3),
                        "jitter_ms": round(float(summary.get("jitter_ms", 0.0)), 4),
                    }
                )
            else:
                errors.append("missing UDP sum")
        else:
            summary = end.get("sum_received") or end.get("sum_sent") or {}
            bps = summary.get("bits_per_second")
            if bps is not None:
                samples.append(round(bps / 8e6, 4))
            else:
                errors.append("missing TCP sum")

    q_prio = summarize_priority_samples(qemu_prios)
    i_prio = summarize_priority_samples(iperf_prios)
    if not samples:
        return {
            "status": fail_status,
            "runs": [],
            "runs_completed": 0,
            "runs_requested": runs,
            "attempts": attempts,
            "errors": errors,
            "cpu_s": cpu_samples,
            "iperf_cpu_percent": iperf_cpu_samples,
            "max_load1": max(load_peaks) if load_peaks else None,
            "qemu_priority": q_prio,
            "iperf_server_priority": i_prio,
        }
    if is_udp:
        rates = [s["mb_per_s"] for s in samples]
        losses = [s["lost_percent"] for s in samples]
        jitters = [s["jitter_ms"] for s in samples]
        return {
            "status": "ok",
            "unit": "MB/s",
            "runs": rates,
            "runs_completed": len(rates),
            "runs_requested": runs,
            "attempts": attempts,
            "median": round(statistics.median(rates), 4),
            "spread": round(max(rates) - min(rates), 4),
            "lost_percent_median": round(statistics.median(losses), 3),
            "jitter_ms_median": round(statistics.median(jitters), 4),
            "errors": errors,
            "cpu_s": cpu_samples,
            "iperf_cpu_percent": iperf_cpu_samples,
            "max_load1": max(load_peaks),
            "qemu_priority": q_prio,
            "iperf_server_priority": i_prio,
        }
    return {
        "status": "ok",
        "unit": "MB/s",
        "runs": samples,
        "runs_completed": len(samples),
        "runs_requested": runs,
        "attempts": attempts,
        "median": round(statistics.median(samples), 4),
        "spread": round(max(samples) - min(samples), 4),
        "errors": errors,
        "cpu_s": cpu_samples,
        "iperf_cpu_percent": iperf_cpu_samples,
        "max_load1": max(load_peaks),
        "qemu_priority": q_prio,
        "iperf_server_priority": i_prio,
    }


def run_latency_series(vm, run_status, cmd, metric_key, driver, runs=3, timeout=90):
    samples = []
    extra = []
    cpu_samples = []
    load_peaks = []
    qemu_prios = []
    errors = []
    max_attempts = max(runs, MAX_IPERF_ATTEMPTS)
    attempts = 0
    for _ in range(max_attempts):
        if len(samples) >= runs:
            break
        attempts += 1
        check_foreign_qemus()
        qemu_prios.extend(sample_own_qemu_priority())
        load_before = os.getloadavg()[0]
        ctx_before = context_snapshot(vm, run_status)
        status, out = run_status(vm, cmd, timeout)
        ctx_after = context_snapshot(vm, run_status)
        load_after = os.getloadavg()[0]
        load_peaks.append(round(max(load_before, load_after), 2))
        check_foreign_qemus()
        qemu_prios.extend(sample_own_qemu_priority())
        cpu_samples.append(cpu_delta(ctx_before, ctx_after, ("netstack", driver, "net-probe")))
        data = parse_json_line(out)
        if status == 0 and data and data.get("ok") and data.get(metric_key) is not None:
            samples.append(round(float(data[metric_key]), 3))
            extra.append(data)
        else:
            errors.append(f"exit {status}: {out.strip()[-160:]}")
    q_prio = summarize_priority_samples(qemu_prios)
    if not samples:
        return {
            "status": "error",
            "runs": [],
            "runs_completed": 0,
            "runs_requested": runs,
            "attempts": attempts,
            "errors": errors,
            "cpu_s": cpu_samples,
            "max_load1": max(load_peaks) if load_peaks else None,
            "qemu_priority": q_prio,
        }
    return {
        "status": "ok",
        "unit": "ms",
        "runs": samples,
        "runs_completed": len(samples),
        "runs_requested": runs,
        "attempts": attempts,
        "median": round(statistics.median(samples), 3),
        "spread": round(max(samples) - min(samples), 3),
        "p99_median": round(statistics.median([e["p99_ms"] for e in extra if "p99_ms" in e]), 3)
        if any("p99_ms" in e for e in extra)
        else None,
        "max_worst": round(max(e["max_ms"] for e in extra if "max_ms" in e), 3)
        if any("max_ms" in e for e in extra)
        else round(max(e["total_max_ms"] for e in extra if "total_max_ms" in e), 3)
        if any("total_max_ms" in e for e in extra)
        else None,
        "errors": errors,
        "cpu_s": cpu_samples,
        "max_load1": max(load_peaks),
        "qemu_priority": q_prio,
    }


def parse_window_deadline(raw, now=None):
    if not raw:
        return None, None
    text = str(raw).strip()
    if not text:
        return None, None
    now_ts = time.time() if now is None else float(now)
    if re.match(r"^\d{2}:\d{2}:\d{2}$", text):
        base = datetime.fromtimestamp(now_ts)
        h, m, s = (int(part) for part in text.split(":"))
        target = base.replace(hour=h, minute=m, second=s, microsecond=0)
        if target.timestamp() < now_ts - 43_200:
            target = target + timedelta(days=1)
        return text, target.timestamp()
    try:
        return text, datetime.fromisoformat(text).timestamp()
    except ValueError:
        pass
    try:
        return text, float(text)
    except ValueError:
        raise ValueError(f"invalid --window-deadline {raw!r}; expected HH:MM:SS, ISO-8601 or epoch") from None


def _proc_boot_time():
    try:
        for line in Path("/proc/stat").read_text().splitlines():
            if line.startswith("btime "):
                return int(line.split()[1])
    except (OSError, ValueError, IndexError):
        return None
    return None


def _parse_proc_stat(text):
    rparen = text.rfind(")")
    lparen = text.find("(")
    if lparen < 0 or rparen <= lparen:
        return None, None, None, None, None
    comm = text[lparen + 1 : rparen]
    fields = text[rparen + 2 :].split()
    if len(fields) < 20:
        return comm, None, None, None, None
    try:
        ppid = int(fields[1])
        nice = int(fields[16])
        start_ticks = int(fields[19])
        policy = int(fields[38]) if len(fields) > 38 else None
    except ValueError:
        return comm, None, None, None, None
    return comm, ppid, nice, start_ticks, policy


def _policy_name(policy_id):
    if policy_id is None:
        return None
    return SCHED_POLICIES.get(policy_id, f"POLICY_{policy_id}")


def read_proc_priority(pid):
    pairs = set()
    task_dir = Path(f"/proc/{pid}/task")
    try:
        task_entries = list(task_dir.iterdir())[:MAX_TASK_ENTRIES]
    except OSError:
        task_entries = []
    for entry in task_entries:
        if not entry.name.isdigit():
            continue
        try:
            stat_txt = (entry / "stat").read_text()
        except OSError:
            continue
        _, _, nice, _, policy = _parse_proc_stat(stat_txt)
        if nice is not None and policy is not None:
            pairs.add((nice, policy))
    if not pairs:
        try:
            stat_txt = Path(f"/proc/{pid}/stat").read_text()
            _, _, nice, _, policy = _parse_proc_stat(stat_txt)
            if nice is not None and policy is not None:
                pairs.add((nice, policy))
        except OSError:
            return None
    if not pairs:
        return None
    return sorted(pairs)


def summarize_priority_samples(samples):
    unique_sets = []
    seen_sets = set()
    for sample in samples:
        if not sample:
            continue
        norm = tuple(sorted({(int(pair[0]), int(pair[1])) for pair in sample}))
        if norm and norm not in seen_sets:
            seen_sets.add(norm)
            unique_sets.append(norm)
    if not unique_sets:
        return None
    all_pairs = sorted({pair for s in unique_sets for pair in s})
    observed = [{"nice": n, "policy": _policy_name(p), "policy_id": p} for n, p in all_pairs]
    constant = len(unique_sets) == 1
    single = constant and len(unique_sets[0]) == 1
    return {
        "nice": unique_sets[0][0][0] if single else None,
        "policy": _policy_name(unique_sets[0][0][1]) if single else None,
        "policy_id": unique_sets[0][0][1] if single else None,
        "constant": constant,
        "observed": observed,
        "sample_sets": [
            [{"nice": n, "policy": _policy_name(p), "policy_id": p} for n, p in s]
            for s in unique_sets
        ],
    }


def scan_qemus(self_pid=None):
    owner_pid = os.getpid() if self_pid is None else int(self_pid)
    btime = _proc_boot_time()
    clk_tck = os.sysconf("SC_CLK_TCK")
    ppids = {}
    qemus = []
    proc_root = Path("/proc")
    try:
        entries = list(proc_root.iterdir())[:MAX_PROC_ENTRIES]
    except OSError:
        return [], []
    for entry in entries:
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        try:
            stat_txt = (entry / "stat").read_text()
        except OSError:
            continue
        comm, ppid, _, start_ticks, _ = _parse_proc_stat(stat_txt)
        if ppid is not None:
            ppids[pid] = ppid
        argv0 = ""
        try:
            raw_cmd = (entry / "cmdline").read_bytes()
            if raw_cmd:
                argv0 = os.path.basename(raw_cmd.split(b"\0", 1)[0].decode("utf-8", "replace"))
        except OSError:
            pass
        name = argv0 if argv0.startswith("qemu-system-") else (comm or "")
        if not name.startswith("qemu-system-"):
            continue
        started_at = None
        if btime is not None and start_ticks is not None and clk_tck > 0:
            started_at = datetime.fromtimestamp(btime + start_ticks / clk_tck).astimezone().isoformat(timespec="seconds")
        try:
            cwd = os.readlink(str(entry / "cwd"))
        except OSError:
            cwd = None
        qemus.append((pid, name, started_at, cwd))
    own_pids = []
    foreign = []
    for pid, name, started_at, cwd in sorted(qemus):
        cur = pid
        is_own = False
        for _ in range(MAX_ANCESTOR_HOPS):
            if cur == owner_pid:
                is_own = True
                break
            nxt = ppids.get(cur)
            if nxt is None or nxt <= 1 or nxt == cur:
                break
            cur = nxt
        if is_own:
            own_pids.append(pid)
        else:
            foreign.append({"pid": pid, "name": name, "started_at": started_at, "cwd": cwd})
    return own_pids, foreign


def list_foreign_qemus(self_pid=None):
    return scan_qemus(self_pid)[1]


def check_foreign_qemus(self_pid=None):
    foreign = list_foreign_qemus(self_pid)
    if foreign:
        raise _ForeignQemuDetected(foreign)


def sample_own_qemu_priority(self_pid=None):
    own_pids, _ = scan_qemus(self_pid)
    out = []
    for pid in own_pids:
        pairs = read_proc_priority(pid)
        if pairs:
            out.append(pairs)
    return out


def sample_iperf_priority(lab):
    out = []
    for proc in lab.iperf_servers.values():
        if proc.poll() is None:
            pairs = read_proc_priority(proc.pid)
            if pairs:
                out.append(pairs)
    return out


def is_legacy_pre_guard_pass(pass_data):
    cond = pass_data.get("conditions", {})
    return (
        "qemu_priority" not in cond
        and "foreign_qemu" not in cond
        and "window_deadline" not in cond
    )


def _priority_signature(qp):
    if not isinstance(qp, dict) or not qp.get("constant"):
        return None
    obs = qp.get("observed") or []
    sig = tuple(
        sorted(
            (int(o["nice"]), str(o["policy"]))
            for o in obs
            if o.get("nice") is not None and o.get("policy") is not None
        )
    )
    if sig:
        return sig
    if qp.get("nice") is not None and qp.get("policy") is not None:
        return ((int(qp["nice"]), str(qp["policy"])),)
    return None


def _format_priority_sig(sig, qp):
    if sig is None:
        n = qp.get("nice") if isinstance(qp, dict) else None
        p = qp.get("policy") if isinstance(qp, dict) else None
        return f"{n} ({p})"
    if len(sig) == 1:
        return f"{sig[0][0]} ({sig[0][1]})"
    return "[" + ", ".join(f"{n} ({p})" for n, p in sig) + "]"


def compare_perf_passes(pass1, pass2, allow_legacy_unrecorded_nice=False):
    c1 = pass1.get("conditions", {})
    c2 = pass2.get("conditions", {})
    if c1.get("host_limited") or c2.get("host_limited"):
        r1 = c1.get("host_limited_reasons") or (["load"] if c1.get("host_limited") else [])
        r2 = c2.get("host_limited_reasons") or (["load"] if c2.get("host_limited") else [])
        return (
            None,
            0,
            [
                f"host_limited pass cannot be compared "
                f"(pass1 max_load1={c1.get('max_load1')} reasons={r1}, "
                f"pass2 max_load1={c2.get('max_load1')} reasons={r2})"
            ],
        )
    legacy_ok = (
        allow_legacy_unrecorded_nice
        and is_legacy_pre_guard_pass(pass1)
        and is_legacy_pre_guard_pass(pass2)
    )
    if not legacy_ok:
        qp1 = c1.get("qemu_priority")
        qp2 = c2.get("qemu_priority")
        sig1 = _priority_signature(qp1)
        sig2 = _priority_signature(qp2)
        if sig1 is None or sig2 is None or sig1 != sig2:
            return (
                None,
                0,
                [
                    f"not comparable: nice {_format_priority_sig(sig1, qp1)} "
                    f"against {_format_priority_sig(sig2, qp2)}"
                ],
            )
    mismatches = []
    checked = 0
    for topo in ("lab_tap", "qemu_user"):
        m1 = pass1.get("topologies", {}).get(topo, {}).get("measures", {})
        m2 = pass2.get("topologies", {}).get(topo, {}).get("measures", {})
        for name in sorted(set(m1) & set(m2)):
            a, b = m1[name], m2[name]
            if a.get("status") != "ok" or b.get("status") != "ok":
                continue
            if "median" not in a or "median" not in b:
                continue
            req1 = int(a.get("runs_requested", 3))
            req2 = int(b.get("runs_requested", 3))
            if len(a.get("runs", [])) < req1 or len(b.get("runs", [])) < req2:
                continue
            med1, med2 = float(a["median"]), float(b["median"])
            sp1, sp2 = float(a.get("spread", 0.0)), float(b.get("spread", 0.0))
            bound = max(NOISE_REL_LIMIT * max(abs(med1), abs(med2)), sp1, sp2)
            diff = abs(med1 - med2)
            checked += 1
            if diff > bound:
                mismatches.append(
                    f"{topo}.{name}: {med1} vs {med2} (diff {diff:.4f} > bound {bound:.4f})"
                )
    return len(mismatches) == 0 and checked > 0, checked, mismatches


def run_net_perf(args, out, ctx):
    card = ctx["net_card"](args)
    if card not in ctx["NET_CARDS"]:
        return False, f"net-perf runs with net=e1000 or net=virtio, not net={card}"
    driver = "virtio-netd" if card == "virtio" else "e1000d"
    require_tools(LAB_PERF_TOOLS, ctx["Blocked"])
    deadline_str, deadline_ts = parse_window_deadline(getattr(args, "window_deadline", None))
    host_iperf = ctx["command_output"](["iperf3", "--version"])
    host_iperf_line = host_iperf.splitlines()[0] if host_iperf else None
    load_start = [round(v, 2) for v in os.getloadavg()]
    max_load1 = load_start[0]

    report = {
        "conditions": {
            "card": card,
            "driver": driver,
            "vhost": "off",
            "cpu_pinned": False,
            "load_limit": LOAD_LIMIT,
            "noise_rule": "abs(med1 - med2) <= max(0.10 * max(abs(med1), abs(med2)), spread1, spread2)",
            "host_iperf3": host_iperf_line,
            "guest_iperf3": None,
            "components": repo_component_commits(args.arch),
            "loadavg_start": load_start,
            "loadavg_end": None,
            "max_load1": None,
            "qemu_priority": None,
            "iperf_server_priority": None,
            "foreign_qemu": [],
            "window_deadline": deadline_str,
            "ended_after_deadline": False,
            "host_limited_reasons": [],
            "host_limited": False,
        },
        "topologies": {},
    }

    foreign_start = list_foreign_qemus()
    if foreign_start:
        report["conditions"]["loadavg_end"] = [round(v, 2) for v in os.getloadavg()]
        report["conditions"]["max_load1"] = round(max(max_load1, report["conditions"]["loadavg_end"][0]), 2)
        report["conditions"]["foreign_qemu"] = foreign_start
        report["conditions"]["host_limited_reasons"] = ["foreign_qemu"]
        report["conditions"]["host_limited"] = True
        (out / "net-perf.json").write_text(json.dumps(report, indent=2) + "\n")
        raise ctx["Blocked"](
            f"card {card} vhost=off HOST_LIMITED reasons=foreign_qemu "
            f"({format_foreign_qemus(foreign_start)}); see {out / 'net-perf.json'}"
        )

    topo_a_out = out / "topology-a-lab-tap"
    topo_a_out.mkdir(parents=True, exist_ok=True)
    lab = NetLab(
        topo_a_out,
        ctx["Blocked"],
        ctx["dns_reply"],
        enable_dns=False,
        enable_iperf=True,
        enable_lo_helpers=True,
    )
    vm_a = None
    foreign_abort = None
    try:
        lab.start()
        vm_a = ctx["VM"](
            args,
            topo_a_out,
            qemu_flags=lab.qemu_flags(ctx["NET_CARDS"][card], pcap_path=None),
            make=("net=no",),
        )
        if not ctx["root_shell"](vm_a, args.timeout):
            return False, f"topology A: no root shell; see {topo_a_out}"
        guest_ip, dhcp_txt = wait_dhcp_address(vm_a, ctx["run_status"])
        (topo_a_out / "dhcp.txt").write_text(dhcp_txt)
        if not guest_ip:
            return False, f"topology A: no DHCP lease; see {topo_a_out}"
        _, iv_out = ctx["run_status"](vm_a, "iperf3 --version | head -n 1", 30)
        m_iv = re.search(r"iperf\s+\d+\.\d+\S*", iv_out)
        report["conditions"]["guest_iperf3"] = m_iv.group(0).strip() if m_iv else None
        install_iperf_watchdog(vm_a, ctx["run_status"])

        measures_a = {}
        measures_a["tcp_tx_1"] = run_iperf_series(
            vm_a,
            ctx["run_status"],
            f"iperf3 -4 -c {LAB_HOST} -p {IPERF_PORT} -O 2 -t 10 -J",
            driver,
            lab=lab,
            expected={"num_streams": 1, "omit": 2, "duration": 10},
        )
        measures_a["tcp_rx_1"] = run_iperf_series(
            vm_a,
            ctx["run_status"],
            f"iperf3 -4 -c {LAB_HOST} -p {IPERF_PORT} -R -O 2 -t 10 -J",
            driver,
            lab=lab,
            expected={"num_streams": 1, "omit": 2, "duration": 10},
        )
        measures_a["tcp_tx_4"] = run_iperf_series(
            vm_a,
            ctx["run_status"],
            f"iperf3 -4 -c {LAB_HOST} -p {IPERF_PORT} -P 4 -O 2 -t 10 -J",
            driver,
            lab=lab,
            expected={"num_streams": 4, "omit": 2, "duration": 10},
        )
        measures_a["tcp_rx_4"] = run_iperf_series(
            vm_a,
            ctx["run_status"],
            f"iperf3 -4 -c {LAB_HOST} -p {IPERF_PORT} -R -P 4 -O 2 -t 10 -J",
            driver,
            lab=lab,
            expected={"num_streams": 4, "omit": 2, "duration": 10},
        )
        measures_a["udp_1200"] = run_iperf_series(
            vm_a,
            ctx["run_status"],
            f"iperf3 -4 -c {LAB_HOST} -p {IPERF_PORT} -u -l 1200 -b 10M -O 2 -t 10 -J",
            driver,
            is_udp=True,
            lab=lab,
            expected={"blksize": 1200, "target_bitrate": 10_000_000, "num_streams": 1, "omit": 2, "duration": 10},
        )

        lab.set_netem(delay_ms=50)
        try:
            measures_a["tcp_rx_rtt50ms"] = run_iperf_series(
                vm_a,
                ctx["run_status"],
                f"iperf3 -4 -c {LAB_HOST} -p {IPERF_PORT} -R -O 2 -t 10 -J",
                driver,
                watchdog_s=40,
                lab=lab,
                expected={"num_streams": 1, "omit": 2, "duration": 10},
            )
            lab.set_netem(delay_ms=50, loss_pct=1)
            measures_a["tcp_rx_rtt50ms_loss1pct"] = run_iperf_series(
                vm_a,
                ctx["run_status"],
                f"iperf3 -4 -c {LAB_HOST} -p {IPERF_PORT} -R -O 2 -t 10 -J",
                driver,
                watchdog_s=40,
                lab=lab,
                expected={"num_streams": 1, "omit": 2, "duration": 10},
            )
        finally:
            lab.set_netem(None)

        ctx["run_status"](
            vm_a,
            "bash -c 'iperf3 -s -4 -B 127.0.0.1 -p 5202 </dev/null >/tmp/iperf-lo.log 2>&1 & echo $! > /tmp/iperf-lo.pid'; sleep 1",
            30,
        )
        measures_a["tcp_loopback"] = run_iperf_series(
            vm_a,
            ctx["run_status"],
            "iperf3 -4 -c 127.0.0.1 -p 5202 -O 2 -t 10 -J",
            driver,
            watchdog_s=25,
            fail_fast_timeout=True,
            expected={"num_streams": 1, "omit": 2, "duration": 10},
            fail_status="red",
        )
        ctx["run_status"](vm_a, "bash -c 'kill $(cat /tmp/iperf-lo.pid) 2>/dev/null || true'", 15)

        measures_a["idle_http_rtt"] = run_latency_series(
            vm_a,
            ctx["run_status"],
            f"net-probe http-bench {LAB_HOST}:{HTTP_PORT} /ping --count 20 --interval-ms 1000 --timeout-ms 5000",
            "total_p50_ms",
            driver,
            runs=3,
            timeout=90,
        )
        measures_a["tcp_1b_echo"] = run_latency_series(
            vm_a,
            ctx["run_status"],
            f"net-probe echo-bench tcp {LAB_HOST}:{TCP_ECHO_PORT} --count 200 --bytes 1 --nodelay --timeout-ms 5000",
            "p50_ms",
            driver,
            runs=3,
            timeout=120,
        )
        measures_a["udp_1b_echo"] = run_latency_series(
            vm_a,
            ctx["run_status"],
            f"net-probe echo-bench udp {LAB_HOST}:{UDP_ECHO_PORT} --count 200 --bytes 1 --timeout-ms 5000",
            "p50_ms",
            driver,
            runs=3,
            timeout=120,
        )
        ping_res = host_ping(guest_ip, count=100, interval=0.05, timeout=30)
        measures_a["ping_100"] = {
            "status": "ok" if ping_res["received"] > 0 else "no_reply",
            "sent": ping_res["sent"],
            "received": ping_res["received"],
            "rtt": ping_res["rtt"],
        }
        report["topologies"]["lab_tap"] = {"guest_ip": guest_ip, "measures": measures_a}
        vm_a.kill()
        vm_a.close()
        vm_a = None

        topo_b_out = out / "topology-b-qemu-user"
        topo_b_out.mkdir(parents=True, exist_ok=True)
        user_flags = (
            "-netdev user,id=net0 "
            f"-device {ctx['NET_CARDS'][card]},netdev=net0,mac=52:54:00:77:00:03"
        )
        vm_b = ctx["VM"](args, topo_b_out, qemu_flags=user_flags, make=("net=no",))
        try:
            if not ctx["root_shell"](vm_b, args.timeout):
                return False, f"topology B: no root shell; see {topo_b_out}"
            guest_ip_b, dhcp_b = wait_dhcp_address(vm_b, ctx["run_status"], prefix="10.0.2.")
            (topo_b_out / "dhcp.txt").write_text(dhcp_b)
            if not guest_ip_b:
                return False, f"topology B: no DHCP lease; see {topo_b_out}"
            host_b = "10.0.2.2"
            install_iperf_watchdog(vm_b, ctx["run_status"])
            measures_b = {}
            measures_b["tcp_tx_1"] = run_iperf_series(
                vm_b,
                ctx["run_status"],
                f"iperf3 -4 -c {host_b} -p {IPERF_PORT} -O 2 -t 10 -J",
                driver,
                lab=lab,
                expected={"num_streams": 1, "omit": 2, "duration": 10},
            )
            measures_b["tcp_rx_1"] = run_iperf_series(
                vm_b,
                ctx["run_status"],
                f"iperf3 -4 -c {host_b} -p {IPERF_PORT} -R -O 2 -t 10 -J",
                driver,
                lab=lab,
                expected={"num_streams": 1, "omit": 2, "duration": 10},
            )
            measures_b["tcp_tx_4"] = run_iperf_series(
                vm_b,
                ctx["run_status"],
                f"iperf3 -4 -c {host_b} -p {IPERF_PORT} -P 4 -O 2 -t 10 -J",
                driver,
                lab=lab,
                expected={"num_streams": 4, "omit": 2, "duration": 10},
            )
            measures_b["tcp_rx_4"] = run_iperf_series(
                vm_b,
                ctx["run_status"],
                f"iperf3 -4 -c {host_b} -p {IPERF_PORT} -R -P 4 -O 2 -t 10 -J",
                driver,
                lab=lab,
                expected={"num_streams": 4, "omit": 2, "duration": 10},
            )
            measures_b["udp_1200"] = run_iperf_series(
                vm_b,
                ctx["run_status"],
                f"iperf3 -4 -c {host_b} -p {IPERF_PORT} -u -l 1200 -b 10M -O 2 -t 10 -J",
                driver,
                is_udp=True,
                lab=lab,
                expected={"blksize": 1200, "target_bitrate": 10_000_000, "num_streams": 1, "omit": 2, "duration": 10},
            )
            measures_b["idle_http_rtt"] = run_latency_series(
                vm_b,
                ctx["run_status"],
                f"net-probe http-bench {host_b}:{HTTP_PORT} /ping --count 20 --interval-ms 1000 --timeout-ms 5000",
                "total_p50_ms",
                driver,
                runs=3,
                timeout=90,
            )
            measures_b["tcp_1b_echo"] = run_latency_series(
                vm_b,
                ctx["run_status"],
                f"net-probe echo-bench tcp {host_b}:{TCP_ECHO_PORT} --count 200 --bytes 1 --nodelay --timeout-ms 5000",
                "p50_ms",
                driver,
                runs=3,
                timeout=120,
            )
            measures_b["udp_1b_echo"] = run_latency_series(
                vm_b,
                ctx["run_status"],
                f"net-probe echo-bench udp {host_b}:{UDP_ECHO_PORT} --count 200 --bytes 1 --timeout-ms 5000",
                "p50_ms",
                driver,
                runs=3,
                timeout=120,
            )
            report["topologies"]["qemu_user"] = {"guest_ip": guest_ip_b, "measures": measures_b}
            vm_b.kill()
        finally:
            vm_b.close()
    except _ForeignQemuDetected as exc:
        foreign_abort = exc.entries
    finally:
        if vm_a is not None:
            vm_a.close()
        lab.close()

    foreign_end = list_foreign_qemus()
    seen_foreign = {}
    for entry in [*foreign_start, *(foreign_abort or ()), *foreign_end]:
        seen_foreign[(entry["pid"], entry["started_at"])] = entry
    pass_qemu_pairs = []
    pass_iperf_pairs = []
    for topo in report["topologies"].values():
        for m in topo["measures"].values():
            ml = m.get("max_load1")
            if ml is not None:
                max_load1 = max(max_load1, ml)
            qp = m.get("qemu_priority")
            if isinstance(qp, dict):
                for s in qp.get("sample_sets") or [qp.get("observed", [])]:
                    pass_qemu_pairs.append([(o["nice"], o["policy_id"]) for o in s])
            ip = m.get("iperf_server_priority")
            if isinstance(ip, dict):
                for s in ip.get("sample_sets") or [ip.get("observed", [])]:
                    pass_iperf_pairs.append([(o["nice"], o["policy_id"]) for o in s])
    load_end = [round(v, 2) for v in os.getloadavg()]
    max_load1 = round(max(max_load1, load_end[0]), 2)
    foreign_qemu = [seen_foreign[k] for k in sorted(seen_foreign)]
    qemu_prio_summary = summarize_priority_samples(pass_qemu_pairs)
    iperf_prio_summary = summarize_priority_samples(pass_iperf_pairs)
    ended_after_deadline = deadline_ts is not None and time.time() > deadline_ts
    reasons = []
    if max_load1 > LOAD_LIMIT:
        reasons.append("load")
    if foreign_qemu:
        reasons.append("foreign_qemu")
    if ended_after_deadline:
        reasons.append("window_deadline")
    if qemu_prio_summary is None:
        reasons.append("priority_unrecorded")
    elif not qemu_prio_summary["constant"]:
        reasons.append("nice_changed")
    report["conditions"]["loadavg_end"] = load_end
    report["conditions"]["max_load1"] = max_load1
    report["conditions"]["qemu_priority"] = qemu_prio_summary
    report["conditions"]["iperf_server_priority"] = iperf_prio_summary
    report["conditions"]["foreign_qemu"] = foreign_qemu
    report["conditions"]["ended_after_deadline"] = ended_after_deadline
    report["conditions"]["host_limited_reasons"] = reasons
    report["conditions"]["host_limited"] = bool(reasons)

    (out / "net-perf.json").write_text(json.dumps(report, indent=2) + "\n")
    if foreign_abort:
        raise ctx["Blocked"](
            f"card {card} vhost=off HOST_LIMITED reasons={','.join(reasons)} "
            f"({format_foreign_qemus(foreign_qemu)}); see {out / 'net-perf.json'}"
        )
    table_lines = []
    for topo_name, topo in report["topologies"].items():
        for mname, m in topo["measures"].items():
            if m.get("status") == "ok":
                done_n = m.get("runs_completed", len(m.get("runs", [])))
                req_n = m.get("runs_requested", 3)
                table_lines.append(
                    f"{topo_name} {card} {mname}: median={m.get('median')} {m.get('unit', '')} "
                    f"spread={m.get('spread')} runs={done_n}/{req_n} max_load1={m.get('max_load1')}"
                )
            else:
                table_lines.append(f"{topo_name} {card} {mname}: {m.get('status')} {m.get('errors', '')}")
    (out / "net-perf.txt").write_text("\n".join(table_lines) + "\n")
    for line in table_lines:
        print(line, flush=True)

    ok_count = 0
    non_ok = []
    for topo_name, topo in report["topologies"].items():
        for mname, m in topo["measures"].items():
            if m.get("status") == "ok":
                ok_count += 1
            else:
                non_ok.append(f"{topo_name}.{mname}({m.get('status')})")
    status_tag = "HOST_LIMITED" if report["conditions"]["host_limited"] else "QUALIFIED"
    reason_str = f" reasons={','.join(reasons)}" if reasons else ""
    summary = (
        f"card {card} vhost=off {status_tag} max_load1={max_load1}{reason_str}; "
        f"{ok_count}/{len(table_lines)} ok"
        + (f" (non-ok: {', '.join(non_ok)})" if non_ok else "")
        + f"; see {out / 'net-perf.json'}"
    )
    if report["conditions"]["host_limited"]:
        raise ctx["Blocked"](summary)
    return True, summary
