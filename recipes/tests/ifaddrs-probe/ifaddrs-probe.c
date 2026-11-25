#include <arpa/inet.h>
#include <errno.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

#define MAX_PRINT_ENTRIES 64

static void format_ipv4(const struct sockaddr *sa, char *buf, size_t len) {
    if (sa == NULL) {
        snprintf(buf, len, "none");
        return;
    }
    if (sa->sa_family != AF_INET) {
        snprintf(buf, len, "family:%d", (int)sa->sa_family);
        return;
    }
    const struct sockaddr_in *sin = (const struct sockaddr_in *)sa;
    if (inet_ntop(AF_INET, &sin->sin_addr, buf, (socklen_t)len) == NULL) {
        snprintf(buf, len, "err");
    }
}

int main(void) {
    struct ifaddrs *list = NULL;
    if (getifaddrs(&list) != 0) {
        int err = errno;
        printf("getifaddrs error errno=%d (%s)\n", err, strerror(err));
        return 1;
    }

    int count = 0;
    for (struct ifaddrs *cur = list; cur != NULL && count < MAX_PRINT_ENTRIES; cur = cur->ifa_next) {
        const char *name = cur->ifa_name != NULL ? cur->ifa_name : "(null)";
        int family = cur->ifa_addr != NULL ? (int)cur->ifa_addr->sa_family : -1;
        char addr_buf[INET_ADDRSTRLEN + 16];
        char mask_buf[INET_ADDRSTRLEN + 16];
        char broad_buf[INET_ADDRSTRLEN + 16];
        format_ipv4(cur->ifa_addr, addr_buf, sizeof(addr_buf));
        format_ipv4(cur->ifa_netmask, mask_buf, sizeof(mask_buf));
        if ((cur->ifa_flags & IFF_BROADCAST) != 0) {
            format_ipv4(cur->ifa_broadaddr, broad_buf, sizeof(broad_buf));
        } else {
            snprintf(broad_buf, sizeof(broad_buf), "none");
        }
        unsigned int idx = cur->ifa_name != NULL ? if_nametoindex(cur->ifa_name) : 0;
        char rev_name[IF_NAMESIZE];
        memset(rev_name, 0, sizeof(rev_name));
        const char *rev = idx > 0 ? if_indextoname(idx, rev_name) : NULL;
        printf(
            "iface name=%s family=%d addr=%s netmask=%s broadcast=%s flags=0x%x ifindex=%u rev=%s\n",
            name,
            family,
            addr_buf,
            mask_buf,
            broad_buf,
            cur->ifa_flags,
            idx,
            rev != NULL ? rev : "none"
        );
        count++;
    }

    struct if_nameindex *ni = if_nameindex();
    if (ni != NULL) {
        for (int i = 0; i < MAX_PRINT_ENTRIES && (ni[i].if_index != 0 || ni[i].if_name != NULL); i++) {
            printf("nameindex idx=%u name=%s\n", ni[i].if_index, ni[i].if_name != NULL ? ni[i].if_name : "(null)");
        }
        if_freenameindex(ni);
    }

    freeifaddrs(list);
    printf("total=%d\n", count);
    return 0;
}
