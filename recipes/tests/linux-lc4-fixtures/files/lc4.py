import ctypes, errno, fcntl, hashlib, json, mmap, os, select, socket, sqlite3, subprocess, sys, tempfile, threading


def say(*args):
    print(*args, flush=True)


srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
say("SO_REUSEADDR", srv.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR) != 0)
say("SO_TYPE", srv.getsockopt(socket.SOL_SOCKET, socket.SO_TYPE) == socket.SOCK_STREAM)
srv.bind(("127.0.0.1", 0))
srv.listen(4)
host, port = srv.getsockname()
say("bound", host, 1024 <= port < 65536)
seen = {}


SERVE_TIMEOUT_S = 10


def recv_exactly(conn, n):
    got = b""
    while len(got) < n:
        part = conn.recv(n - len(got))
        if not part:
            raise EOFError("short recv")
        got += part
    return got


def recvmsg_exactly(conn, n):
    data, ancillary, flags = b"", [], 0
    while len(data) < n:
        part, ancillary, flags, _ = conn.recvmsg(n - len(data))
        if not part:
            raise EOFError("short recvmsg")
        data += part
    return data, ancillary, flags


def serve():
    conn = None
    try:
        srv.settimeout(SERVE_TIMEOUT_S)
        conn, peer = srv.accept()
        conn.settimeout(SERVE_TIMEOUT_S)
        seen["peer"] = peer
        seen["name"] = conn.getsockname()
        seen["getpeername"] = conn.getpeername()
        conn.sendall(recv_exactly(conn, 4).upper())
        data, ancillary, flags = recvmsg_exactly(conn, 11)
        seen["recvmsg"] = (data, len(ancillary), flags)
        conn.sendall(b"ok")
        seen["sendmmsg"] = recv_exactly(conn, 9)
    except (TimeoutError, EOFError, OSError) as err:
        seen["error"] = err
    finally:
        if conn is not None:
            conn.close()


thread = threading.Thread(target=serve)
thread.start()
cli = socket.create_connection((host, port))
cli.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
say("TCP_NODELAY", cli.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY) != 0)
cli.sendall(b"ping")
say("reply", cli.recv(64))
cli.sendmsg([b"via ", b"sendmsg"])
say("acknowledged", cli.recv(2))


class Iovec(ctypes.Structure):
    _fields_ = [("base", ctypes.c_void_p), ("len", ctypes.c_size_t)]


class Msghdr(ctypes.Structure):
    _fields_ = [("name", ctypes.c_void_p), ("namelen", ctypes.c_uint32), ("iov", ctypes.POINTER(Iovec)),
                ("iovlen", ctypes.c_size_t), ("control", ctypes.c_void_p), ("controllen", ctypes.c_size_t),
                ("flags", ctypes.c_int)]


class Mmsghdr(ctypes.Structure):
    _fields_ = [("hdr", Msghdr), ("len", ctypes.c_uint)]


libc = ctypes.CDLL(None, use_errno=True)
pieces = [ctypes.create_string_buffer(b"one", 3), ctypes.create_string_buffer(b"two", 3),
          ctypes.create_string_buffer(b"six", 3)]
iovs = (Iovec * 3)(*[Iovec(ctypes.cast(p, ctypes.c_void_p), 3) for p in pieces])
msgs = (Mmsghdr * 3)()
for i in range(3):
    msgs[i].hdr.iov = ctypes.cast(ctypes.byref(iovs, i * ctypes.sizeof(Iovec)), ctypes.POINTER(Iovec))
    msgs[i].hdr.iovlen = 1
sent = libc.sendmmsg(cli.fileno(), msgs, 3, 0)
say("sendmmsg", sent, [msgs[i].len for i in range(3)])
thread.join(SERVE_TIMEOUT_S)
if thread.is_alive() or "error" in seen:
    say("serve timed out")
    cli.close()
    srv.close()
    sys.exit(1)
say("server's peer is the client", seen["peer"] == cli.getsockname(), seen["getpeername"] == seen["peer"])
say("server's own name", seen["name"] == (host, port))
say("client's peer", cli.getpeername() == (host, port))
say("recvmsg", seen["recvmsg"])
say("sendmmsg arrived", seen["sendmmsg"])
cli.close()
srv.close()
try:
    socket.create_connection((host, port))
except ConnectionRefusedError as e:
    say("closed port", errno.errorcode[e.errno])
try:
    socket.create_connection(("192.0.2.1", 9))
except OSError as e:
    say("outside loopback", errno.errorcode[e.errno])

srv2 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv2.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv2.bind(("127.0.0.1", 0))
srv2.listen(4)
h2, p2 = srv2.getsockname()
cli2 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
cli2.setblocking(False)
rc = cli2.connect_ex((h2, p2))
_, writable, _ = select.select([], [cli2], [], 2)
so_err = cli2.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
peer_ok = cli2.getpeername() == (h2, p2)
srv2.settimeout(2)
conn2, _ = srv2.accept()
srv_peer_ok = conn2.getpeername() == cli2.getsockname()
say("nb connect", rc in (0, errno.EINPROGRESS), len(writable) == 1, so_err == 0, peer_ok, srv_peer_ok)
conn2.close()
cli2.close()
srv2.close()

r, w = os.pipe()
ep = select.epoll()
ep.register(r, select.EPOLLIN)
say("epoll empty", ep.poll(0))
os.write(w, b"x")
say("epoll ready", [(fd == r, ev) for fd, ev in ep.poll(1)])
say("epoll still ready", [(fd == r, ev) for fd, ev in ep.poll(0)])
os.read(r, 1)
say("epoll drained", ep.poll(0))
ep.modify(r, select.EPOLLIN | select.EPOLLONESHOT)
os.write(w, b"y")
say("oneshot", len(ep.poll(1)), len(ep.poll(0)))
ep.close()

r_et, w_et = os.pipe()
os.set_blocking(r_et, False)
ep_et = select.epoll()
ep_et.register(r_et, select.EPOLLIN | select.EPOLLET)
os.write(w_et, b"a")
et_first = [(fd == r_et, ev) for fd, ev in ep_et.poll(0.2)]
et_drained = False
for _ in range(16):
    try:
        os.read(r_et, 64)
    except BlockingIOError:
        et_drained = True
        break
os.write(w_et, b"b")
et_second = [(fd == r_et, ev) for fd, ev in ep_et.poll(0.2)]
say("epoll et rearm", et_drained, et_first, et_second)
ep_et.close()
os.close(r_et)
os.close(w_et)

r2, w2 = os.pipe()
ep2 = select.epoll()
ep2.register(r2, select.EPOLLIN)
old_r2 = r2
os.close(r2)
r3, w3 = os.pipe()
os.write(w3, b"z")
after_reuse = ep2.poll(0)
ep2.register(r3, select.EPOLLIN)
after_rereg = [(fd == r3, ev) for fd, ev in ep2.poll(0)]
say("epoll close reuse", r3 == old_r2, after_reuse, after_rereg)
ep2.close()
os.close(w2)
os.close(r3)
os.close(w3)
p = select.poll()
p.register(w, select.POLLOUT)
p.register(r, select.POLLIN)
say("poll", sorted((fd == r, ev) for fd, ev in p.poll(0)))
os.close(w)
say("poll hangup", sorted((fd == r, ev & (select.POLLIN | select.POLLHUP)) for fd, ev in p.poll(0)))
os.close(r)

m = mmap.mmap(-1, 4096)
m.write(b"kept bytes")
m.resize(1 << 20)
say("mremap", m[:10], len(m))
m.close()

with tempfile.TemporaryDirectory() as d:
    path = os.path.join(d, "f")
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o644)
    os.pwrite(fd, b"written at ten", 10)
    os.fsync(fd)
    os.fdatasync(fd)
    say("pwrite", os.pread(fd, 32, 0))
    copy = os.dup(fd)
    say("dup", os.fstat(copy).st_size)
    extra = [os.dup(fd) for _ in range(3)]
    os.closerange(min(extra), max(extra) + 1)
    closed = 0
    for e in extra:
        try:
            os.fstat(e)
        except OSError as err:
            closed += err.errno == errno.EBADF
    say("close_range closed", closed)
    os.close(copy)
    os.symlink("f", os.path.join(d, "link"))
    say("symlink", os.readlink(os.path.join(d, "link")), open(os.path.join(d, "link"), "rb").read(4))
    say("listxattr", os.listxattr(path))

    fcntl.lockf(fd, fcntl.LOCK_EX)
    probe = ("import fcntl, os, sys, errno\nfd = os.open(sys.argv[1], os.O_RDWR)\n"
             "try:\n    fcntl.lockf(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)\n    print('child locked')\n"
             "except OSError as e:\n    print('child refused', errno.errorcode[e.errno])\n")
    say(subprocess.run([sys.executable, "-c", probe, path], capture_output=True, text=True).stdout.strip())
    fcntl.lockf(fd, fcntl.LOCK_UN)
    say(subprocess.run([sys.executable, "-c", probe, path], capture_output=True, text=True).stdout.strip())
    os.close(fd)

    p = os.path.join(d, "x.json")
    with open(p, "w") as f:
        json.dump({"a": [1, 2, 3]}, f)
    say("json", hashlib.sha256(open(p, "rb").read()).hexdigest()[:16])

    db_path = os.path.join(d, "t.db")
    db = sqlite3.connect(db_path, timeout=0, isolation_level=None)
    db.execute("create table t(x)")
    db.execute("insert into t values (42)")
    writer = ("import sqlite3, sys\nc = sqlite3.connect(sys.argv[1], timeout=0)\n"
              "try:\n    c.execute('insert into t values (7)'); c.commit(); print('child inserted')\n"
              "except sqlite3.OperationalError as e:\n    print('child:', e)\n")
    db.execute("begin exclusive")
    db.execute("insert into t values (43)")
    say(subprocess.run([sys.executable, "-c", writer, db_path], capture_output=True, text=True).stdout.strip())
    db.execute("commit")
    say(subprocess.run([sys.executable, "-c", writer, db_path], capture_output=True, text=True).stdout.strip())
    say("sqlite", db.execute("select group_concat(x) from t order by rowid").fetchone()[0])
    db.close()

child = subprocess.run(["/bin/echo", "child"], capture_output=True, text=True)
say("child", child.stdout.strip(), child.returncode)
say("python done", sys.version_info[:2])
