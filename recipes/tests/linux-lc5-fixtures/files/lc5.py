import os, re, select, socket, sys, urllib.request

mode, curl = sys.argv[1], int(sys.argv[2])


def title(body):
    found = re.search(rb"<title>(.*?)</title>", body, re.S)
    return found.group(1).decode() if found else None


print("curl status", curl)
if curl == 0:
    with open("/tmp/page", "rb") as page:
        print("curl title", title(page.read()))
else:
    with open("/tmp/curl.err") as err:
        print("curl said", err.read().strip())

try:
    with urllib.request.urlopen("https://example.com/", timeout=60) as reply:
        print("urllib status", reply.status, "title", title(reply.read()))
except Exception as e:
    print("urllib error", type(e).__name__, e)

fd = os.eventfd(3, os.EFD_NONBLOCK | os.EFD_CLOEXEC)
os.eventfd_write(fd, 4)
print("eventfd", os.eventfd_read(fd))
try:
    os.eventfd_read(fd)
except BlockingIOError as e:
    print("eventfd empty", e.errno)
os.close(fd)

s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.IPPROTO_TCP, socket.TCP_KEEPIDLE, 30)
s.setsockopt(socket.SOL_SOCKET, socket.SO_KEEPALIVE, 1)
print("TCP_KEEPIDLE", s.getsockopt(socket.IPPROTO_TCP, socket.TCP_KEEPIDLE), "SO_KEEPALIVE", s.getsockopt(socket.SOL_SOCKET, socket.SO_KEEPALIVE) != 0)
try:
    s.setsockopt(socket.SOL_SOCKET, 0x7fff, 1)
except OSError as e:
    print("unknown option", e.errno)
if mode == "online":
    address = socket.getaddrinfo("example.com", 443, socket.AF_INET, socket.SOCK_STREAM)[0][4]
    s.setblocking(False)
    print("connect", s.connect_ex(address) in (0, 115))
    select.select([], [s], [], 30)
    print("SO_ERROR", s.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR))
else:
    try:
        s.connect(("93.184.215.14", 443))
    except OSError as e:
        print("connect error", e.errno, e.strerror)
s.close()
print("done")
