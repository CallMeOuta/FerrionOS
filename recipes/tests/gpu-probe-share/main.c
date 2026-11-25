#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

#define TAG "gpu-probe-share"
#define MAX_SCHEME_ENTRIES 4096U
#define MAX_VK_DEVS 16U
#define MAX_VK_EXTS 1024U
#define MAX_VK_QUEUES 32U
#define COUNTER_ITERS 1000U
#define BLOB_BYTES 65536U
#define PAGE_BYTES 4096U
#define MAX_IPC_FDS 4U

#define DRM_IOC(dir, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | (0x64UL << 8) | (nr)))
#define DRM_IOW(nr, type) DRM_IOC(1, nr, sizeof(type))
#define DRM_IOWR(nr, type) DRM_IOC(3, nr, sizeof(type))

#define DRM_CLOEXEC O_CLOEXEC
#define DRM_RDWR O_RDWR

struct drm_gem_close {
    uint32_t handle;
    uint32_t pad;
};

struct drm_prime_handle {
    uint32_t handle;
    uint32_t flags;
    int32_t fd;
};

struct drm_virtgpu_map {
    uint64_t offset;
    uint32_t handle;
    uint32_t pad;
};

struct drm_virtgpu_resource_info {
    uint32_t bo_handle;
    uint32_t res_handle;
    uint32_t size;
    uint32_t blob_mem;
};

struct drm_virtgpu_resource_create_blob {
    uint32_t blob_mem;
    uint32_t blob_flags;
    uint32_t bo_handle;
    uint32_t res_handle;
    uint64_t size;
    uint32_t pad;
    uint32_t cmd_size;
    uint64_t cmd;
    uint64_t blob_id;
    uint32_t blob_hints;
    uint32_t pad2;
};

struct drm_virtgpu_execbuffer {
    uint32_t flags;
    uint32_t size;
    uint64_t command;
    uint64_t bo_handles;
    uint32_t num_bo_handles;
    int32_t fence_fd;
    uint32_t ring_idx;
    uint32_t syncobj_stride;
    uint32_t num_in_syncobjs;
    uint32_t num_out_syncobjs;
    uint64_t in_syncobjs;
    uint64_t out_syncobjs;
};

struct drm_virtgpu_3d_wait {
    uint32_t handle;
    uint32_t flags;
};

struct drm_virtgpu_context_set_param {
    uint64_t param;
    uint64_t value;
};

struct drm_virtgpu_context_init {
    uint32_t num_params;
    uint32_t pad;
    uint64_t ctx_set_params;
};

struct drm_syncobj_create {
    uint32_t handle;
    uint32_t flags;
};

struct drm_syncobj_destroy {
    uint32_t handle;
    uint32_t pad;
};

struct drm_syncobj_handle {
    uint32_t handle;
    uint32_t flags;
    int32_t fd;
    uint32_t pad;
    uint64_t point;
};

struct drm_syncobj_timeline_array {
    uint64_t handles;
    uint64_t points;
    uint32_t count_handles;
    uint32_t flags;
};

struct drm_syncobj_timeline_wait {
    uint64_t handles;
    uint64_t points;
    int64_t timeout_nsec;
    uint32_t count_handles;
    uint32_t flags;
    uint32_t first_signaled;
    uint32_t pad;
    uint64_t deadline_nsec;
};

struct drm_mode_fb_cmd2 {
    uint32_t fb_id;
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    uint32_t flags;
    uint32_t handles[4];
    uint32_t pitches[4];
    uint32_t offsets[4];
    uint64_t modifier[4];
};

#define DRM_IOCTL_GEM_CLOSE DRM_IOW(0x09, struct drm_gem_close)
#define DRM_IOCTL_PRIME_HANDLE_TO_FD DRM_IOWR(0x2d, struct drm_prime_handle)
#define DRM_IOCTL_PRIME_FD_TO_HANDLE DRM_IOWR(0x2e, struct drm_prime_handle)
#define DRM_IOCTL_VIRTGPU_MAP DRM_IOWR(0x41, struct drm_virtgpu_map)
#define DRM_IOCTL_VIRTGPU_EXECBUFFER DRM_IOWR(0x42, struct drm_virtgpu_execbuffer)
#define DRM_IOCTL_VIRTGPU_RESOURCE_INFO DRM_IOWR(0x45, struct drm_virtgpu_resource_info)
#define DRM_IOCTL_VIRTGPU_WAIT DRM_IOWR(0x48, struct drm_virtgpu_3d_wait)
#define DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB DRM_IOWR(0x4a, struct drm_virtgpu_resource_create_blob)
#define DRM_IOCTL_VIRTGPU_CONTEXT_INIT DRM_IOWR(0x4b, struct drm_virtgpu_context_init)
#define DRM_IOCTL_MODE_ADDFB2 DRM_IOWR(0xb8, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_SYNCOBJ_CREATE DRM_IOWR(0xbf, struct drm_syncobj_create)
#define DRM_IOCTL_SYNCOBJ_DESTROY DRM_IOWR(0xc0, struct drm_syncobj_destroy)
#define DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD DRM_IOWR(0xc1, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE DRM_IOWR(0xc2, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT DRM_IOWR(0xca, struct drm_syncobj_timeline_wait)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL DRM_IOWR(0xcd, struct drm_syncobj_timeline_array)

#define VIRTGPU_BLOB_MEM_HOST3D 0x0002U
#define VIRTGPU_BLOB_FLAG_USE_MAPPABLE 0x0001U
#define VIRTGPU_EXECBUF_FENCE_FD_OUT 0x02U
#define VIRTGPU_EXECBUF_RING_IDX 0x04U
#define VIRTGPU_CONTEXT_PARAM_CAPSET_ID 1U
#define VIRTGPU_CONTEXT_PARAM_NUM_RINGS 2U
#define VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK 3U
#define VIRTGPU_CONTEXT_PARAM_DEBUG_NAME 4U
#define CAPSET_VENUS 4U
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT (1U << 1)
#define DRM_FORMAT_XRGB8888 0x34325258U

static int failures;

static void pass(const char *check, const char *detail)
{
    printf(TAG ": pass %s: %s\n", check, detail);
}

static void fail(const char *check, const char *detail)
{
    printf(TAG ": FAIL %s: %s\n", check, detail);
    failures++;
}

static void expect_errno(const char *check, int ret, int want)
{
    char detail[160];
    int got = errno;
    if (ret == 0) {
        snprintf(detail, sizeof detail, "accepted; expected %s", strerror(want));
        fail(check, detail);
    } else if (got != want) {
        snprintf(detail, sizeof detail, "refused with %d (%s); expected %d (%s)",
                 got, strerror(got), want, strerror(want));
        fail(check, detail);
    } else {
        snprintf(detail, sizeof detail, "refused with %s", strerror(want));
        pass(check, detail);
    }
}

static int find_render_name(char *out, size_t size)
{
    DIR *dir = opendir("/scheme");
    if (!dir)
        return 0;
    struct dirent *entry;
    out[0] = '\0';
    for (unsigned count = 0; count < MAX_SCHEME_ENTRIES && (entry = readdir(dir)) != NULL; count++) {
        if (strncmp(entry->d_name, "gpu.", 4) == 0 && strlen(entry->d_name) < size &&
            (!out[0] || strcmp(entry->d_name, out) < 0)) {
            strcpy(out, entry->d_name);
        }
    }
    closedir(dir);
    return out[0] != '\0';
}

static int init_venus_context(int fd, const char *name)
{
    const struct drm_virtgpu_context_set_param params[] = {
        {VIRTGPU_CONTEXT_PARAM_CAPSET_ID, CAPSET_VENUS},
        {VIRTGPU_CONTEXT_PARAM_NUM_RINGS, 64},
        {VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK, 0},
        {VIRTGPU_CONTEXT_PARAM_DEBUG_NAME, (uintptr_t)name},
    };
    struct drm_virtgpu_context_init req = {
        .num_params = 4,
        .pad = 0,
        .ctx_set_params = (uintptr_t)params,
    };
    return ioctl(fd, DRM_IOCTL_VIRTGPU_CONTEXT_INIT, &req);
}

static int send_msg_fds(int sock, const void *data, size_t len, const int *fds, size_t nfds)
{
    char control[CMSG_SPACE(sizeof(int) * MAX_IPC_FDS)];
    struct iovec iov = {(void *)data, len};
    struct msghdr msg;
    memset(&msg, 0, sizeof msg);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    if (nfds > 0) {
        if (nfds > MAX_IPC_FDS)
            return -1;
        memset(control, 0, sizeof control);
        msg.msg_control = control;
        msg.msg_controllen = CMSG_LEN(sizeof(int) * nfds);
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
        cmsg->cmsg_level = SOL_SOCKET;
        cmsg->cmsg_type = SCM_RIGHTS;
        cmsg->cmsg_len = msg.msg_controllen;
        memcpy(CMSG_DATA(cmsg), fds, sizeof(int) * nfds);
    }
    for (unsigned tries = 0; tries < 200; tries++) {
        ssize_t n = sendmsg(sock, &msg, 0);
        if (n == (ssize_t)len)
            return 0;
        if (n < 0 && errno == EINTR)
            continue;
        return -1;
    }
    return -1;
}

static int recv_msg_fds(int sock, void *data, size_t len, int *fds, size_t *nfds)
{
    char control[CMSG_SPACE(sizeof(int) * MAX_IPC_FDS) + 32];
    struct iovec iov = {data, len};
    struct msghdr msg;
    memset(&msg, 0, sizeof msg);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = sizeof control;
    *nfds = 0;
    for (unsigned tries = 0; tries < 200; tries++) {
        ssize_t n = recvmsg(sock, &msg, MSG_CMSG_CLOEXEC);
        if (n < 0 && errno == EINTR)
            continue;
        if (n != (ssize_t)len)
            return -1;
        if (msg.msg_flags & MSG_CTRUNC)
            return -1;
        for (struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg); cmsg; cmsg = CMSG_NXTHDR(&msg, cmsg)) {
            if (cmsg->cmsg_level == SOL_SOCKET && cmsg->cmsg_type == SCM_RIGHTS) {
                size_t count = (cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int);
                if (*nfds + count > MAX_IPC_FDS)
                    return -1;
                memcpy(fds + *nfds, CMSG_DATA(cmsg), count * sizeof(int));
                *nfds += count;
            }
        }
        return 0;
    }
    return -1;
}

static int64_t monotonic_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + (int64_t)ts.tv_nsec;
}

static bool has_ext(const VkExtensionProperties *exts, uint32_t count, const char *name)
{
    for (uint32_t i = 0; i < count; i++) {
        if (strcmp(exts[i].extensionName, name) == 0)
            return true;
    }
    return false;
}

struct vk_alloc {
    VkInstance inst;
    VkPhysicalDevice pdev;
    VkDevice dev;
    PFN_vkGetMemoryFdKHR get_fd;
};

struct vk_buf_blob {
    VkBuffer buf;
    VkDeviceMemory mem;
    int fd;
    uint32_t size;
};

static int vk_alloc_init(struct vk_alloc *vk)
{
    memset(vk, 0, sizeof *vk);
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = TAG,
        .apiVersion = VK_API_VERSION_1_2,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    if (vkCreateInstance(&ici, NULL, &vk->inst) != VK_SUCCESS)
        return -1;

    uint32_t count = 0;
    if (vkEnumeratePhysicalDevices(vk->inst, &count, NULL) != VK_SUCCESS || count == 0)
        return -1;
    if (count > MAX_VK_DEVS)
        count = MAX_VK_DEVS;
    VkPhysicalDevice devs[MAX_VK_DEVS];
    if (vkEnumeratePhysicalDevices(vk->inst, &count, devs) != VK_SUCCESS)
        return -1;
    for (uint32_t i = 0; i < count; i++) {
        VkPhysicalDeviceProperties props;
        vkGetPhysicalDeviceProperties(devs[i], &props);
        if (props.deviceType != VK_PHYSICAL_DEVICE_TYPE_CPU) {
            vk->pdev = devs[i];
            break;
        }
    }
    if (vk->pdev == VK_NULL_HANDLE)
        return -1;

    uint32_t ecount = 0;
    vkEnumerateDeviceExtensionProperties(vk->pdev, NULL, &ecount, NULL);
    if (ecount == 0)
        return -1;
    if (ecount > MAX_VK_EXTS)
        ecount = MAX_VK_EXTS;
    VkExtensionProperties *exts = calloc(ecount, sizeof(*exts));
    if (!exts)
        return -1;
    vkEnumerateDeviceExtensionProperties(vk->pdev, NULL, &ecount, exts);
    bool ok = has_ext(exts, ecount, VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME) &&
              has_ext(exts, ecount, VK_EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION_NAME);
    free(exts);
    if (!ok)
        return -1;

    uint32_t qcount = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(vk->pdev, &qcount, NULL);
    if (qcount > MAX_VK_QUEUES)
        qcount = MAX_VK_QUEUES;
    VkQueueFamilyProperties qprops[MAX_VK_QUEUES];
    vkGetPhysicalDeviceQueueFamilyProperties(vk->pdev, &qcount, qprops);
    uint32_t qfam = UINT32_MAX;
    for (uint32_t i = 0; i < qcount; i++) {
        if (qprops[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) {
            qfam = i;
            break;
        }
    }
    if (qfam == UINT32_MAX)
        return -1;

    const char *en[2] = {
        VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
        VK_EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION_NAME,
    };
    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = qfam,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &qci,
        .enabledExtensionCount = 2,
        .ppEnabledExtensionNames = en,
    };
    if (vkCreateDevice(vk->pdev, &dci, NULL, &vk->dev) != VK_SUCCESS)
        return -1;
    vk->get_fd = (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(vk->dev, "vkGetMemoryFdKHR");
    return vk->get_fd ? 0 : -1;
}

static int vk_create_shareable_blob(struct vk_alloc *vk, uint32_t bytes, struct vk_buf_blob *out)
{
    memset(out, 0, sizeof *out);
    out->fd = -1;
    VkExternalMemoryBufferCreateInfo embci = {
        .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO,
        .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    };
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .pNext = &embci,
        .size = bytes,
        .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    if (vkCreateBuffer(vk->dev, &bci, NULL, &out->buf) != VK_SUCCESS)
        return -1;
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(vk->dev, out->buf, &req);
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(vk->pdev, &mp);
    VkMemoryPropertyFlags want = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    uint32_t mtype = UINT32_MAX;
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if ((req.memoryTypeBits & (1U << i)) && ((mp.memoryTypes[i].propertyFlags & want) == want)) {
            mtype = i;
            break;
        }
    }
    if (mtype == UINT32_MAX)
        return -1;
    VkMemoryDedicatedAllocateInfo ded = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
        .buffer = out->buf,
    };
    VkExportMemoryAllocateInfo exp_ai = {
        .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
        .pNext = &ded,
        .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    };
    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .pNext = &exp_ai,
        .allocationSize = req.size,
        .memoryTypeIndex = mtype,
    };
    if (vkAllocateMemory(vk->dev, &mai, NULL, &out->mem) != VK_SUCCESS)
        return -1;
    if (vkBindBufferMemory(vk->dev, out->buf, out->mem, 0) != VK_SUCCESS)
        return -1;
    VkMemoryGetFdInfoKHR gfi = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
        .memory = out->mem,
        .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    };
    if (vk->get_fd(vk->dev, &gfi, &out->fd) != VK_SUCCESS || out->fd < 0)
        return -1;
    out->size = (uint32_t)req.size;
    return 0;
}

struct init_payload {
    uint32_t rw_bo;
    uint32_t rw_res;
    uint32_t rw_size;
    uint32_t ro_bo;
    uint32_t ro_res;
    uint32_t kill_bo;
    uint32_t kill_res;
};

static int run_guesser(const char *render_path, uint32_t guess_bo, uint32_t guess_res)
{
    int rfd = open(render_path, O_RDWR | O_CLOEXEC);
    if (rfd < 0)
        return 2;
    if (init_venus_context(rfd, TAG "-guesser") != 0) {
        close(rfd);
        return 3;
    }

    uint32_t candidates[2] = {guess_bo, guess_res};
    for (int i = 0; i < 2; i++) {
        uint32_t h = candidates[i];
        struct drm_virtgpu_map m = {.handle = h};
        if (ioctl(rfd, DRM_IOCTL_VIRTGPU_MAP, &m) == 0)
            return 10 + i;
        struct drm_virtgpu_resource_info info = {.bo_handle = h};
        if (ioctl(rfd, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, &info) == 0)
            return 20 + i;
        struct drm_prime_handle exp = {.handle = h, .flags = DRM_CLOEXEC, .fd = -1};
        if (ioctl(rfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &exp) == 0) {
            close(exp.fd);
            return 30 + i;
        }
        struct drm_virtgpu_execbuffer eb = {
            .flags = VIRTGPU_EXECBUF_RING_IDX,
            .ring_idx = 0,
            .bo_handles = (uintptr_t)&h,
            .num_bo_handles = 1,
            .fence_fd = -1,
        };
        if (ioctl(rfd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &eb) == 0)
            return 40 + i;
        struct drm_virtgpu_3d_wait w = {.handle = h};
        if (ioctl(rfd, DRM_IOCTL_VIRTGPU_WAIT, &w) == 0)
            return 50 + i;
        struct drm_gem_close cl = {.handle = h};
        if (ioctl(rfd, DRM_IOCTL_GEM_CLOSE, &cl) == 0)
            return 60 + i;
    }
    close(rfd);

    int input_fd = open("/scheme/input/consumer", O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (input_fd >= 0) {
        int dfd = openat(input_fd, "display", O_RDWR | O_NONBLOCK | O_CLOEXEC);
        if (dfd >= 0) {
            for (int i = 0; i < 2; i++) {
                uint32_t h = candidates[i];
                struct drm_mode_fb_cmd2 fb = {
                    .width = 64,
                    .height = 64,
                    .pixel_format = DRM_FORMAT_XRGB8888,
                    .handles = {h, 0, 0, 0},
                    .pitches = {256, 0, 0, 0},
                };
                if (ioctl(dfd, DRM_IOCTL_MODE_ADDFB2, &fb) == 0)
                    return 70 + i;
                struct drm_prime_handle exp = {.handle = h, .flags = DRM_CLOEXEC, .fd = -1};
                if (ioctl(dfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &exp) == 0) {
                    close(exp.fd);
                    return 80 + i;
                }
            }
            close(dfd);
        }
        close(input_fd);
    }
    return 0;
}

static int run_producer(const char *render_path, int sock)
{
    int rfd = open(render_path, O_RDWR | O_CLOEXEC);
    if (rfd < 0)
        return 2;
    if (init_venus_context(rfd, TAG "-producer") != 0)
        return 3;

    struct drm_virtgpu_resource_create_blob private_blob = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = PAGE_BYTES,
    };
    if (ioctl(rfd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &private_blob) != 0)
        return 4;
    struct drm_prime_handle bad_priv = {
        .handle = private_blob.bo_handle,
        .flags = DRM_CLOEXEC,
        .fd = -1,
    };
    if (ioctl(rfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &bad_priv) == 0 || errno != EINVAL)
        return 5;
    struct drm_gem_close cl_priv = {.handle = private_blob.bo_handle};
    ioctl(rfd, DRM_IOCTL_GEM_CLOSE, &cl_priv);

    struct vk_alloc vk;
    if (vk_alloc_init(&vk) != 0)
        return 6;
    struct vk_buf_blob vb_rw, vb_ro, vb_kill;
    if (vk_create_shareable_blob(&vk, BLOB_BYTES, &vb_rw) != 0)
        return 7;
    if (vk_create_shareable_blob(&vk, PAGE_BYTES, &vb_ro) != 0)
        return 8;
    if (vk_create_shareable_blob(&vk, PAGE_BYTES, &vb_kill) != 0)
        return 9;

    struct drm_prime_handle p_rw = {.handle = 0, .flags = 0, .fd = vb_rw.fd};
    struct drm_prime_handle p_ro = {.handle = 0, .flags = 0, .fd = vb_ro.fd};
    struct drm_prime_handle p_kill = {.handle = 0, .flags = 0, .fd = vb_kill.fd};
    if (ioctl(rfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &p_rw) != 0 || p_rw.handle == 0)
        return 10;
    if (ioctl(rfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &p_ro) != 0 || p_ro.handle == 0)
        return 11;
    if (ioctl(rfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &p_kill) != 0 || p_kill.handle == 0)
        return 12;
    close(vb_rw.fd);
    close(vb_ro.fd);
    close(vb_kill.fd);

    struct drm_virtgpu_resource_info rw_info = {.bo_handle = p_rw.handle};
    struct drm_virtgpu_resource_info ro_info = {.bo_handle = p_ro.handle};
    struct drm_virtgpu_resource_info kill_info = {.bo_handle = p_kill.handle};
    if (ioctl(rfd, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, &rw_info) != 0 ||
        ioctl(rfd, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, &ro_info) != 0 ||
        ioctl(rfd, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, &kill_info) != 0)
        return 13;

    struct drm_prime_handle bad_flags[3] = {
        {.handle = p_rw.handle, .flags = 1U, .fd = -1},
        {.handle = p_rw.handle, .flags = (uint32_t)O_WRONLY, .fd = -1},
        {.handle = p_rw.handle, .flags = DRM_CLOEXEC | (uint32_t)O_WRONLY, .fd = -1},
    };
    for (int i = 0; i < 3; i++) {
        if (ioctl(rfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &bad_flags[i]) == 0 || errno != EINVAL)
            return 14 + i;
    }

    struct drm_prime_handle exp_rdonly = {
        .handle = p_rw.handle,
        .flags = DRM_CLOEXEC | (uint32_t)O_RDONLY,
        .fd = -1,
    };
    if (ioctl(rfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &exp_rdonly) != 0 || exp_rdonly.fd < 0)
        return 37;
    void *rdonly_bad = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, exp_rdonly.fd, 0);
    if (rdonly_bad != MAP_FAILED || errno != EACCES) {
        if (rdonly_bad != MAP_FAILED)
            munmap(rdonly_bad, PAGE_BYTES);
        close(exp_rdonly.fd);
        return 38;
    }
    close(exp_rdonly.fd);

    struct drm_virtgpu_map rw_map = {.handle = p_rw.handle};
    if (ioctl(rfd, DRM_IOCTL_VIRTGPU_MAP, &rw_map) != 0)
        return 17;
    volatile uint32_t *rw_ptr = mmap(NULL, rw_info.size, PROT_READ | PROT_WRITE, MAP_SHARED, rfd, (off_t)rw_map.offset);
    if (rw_ptr == MAP_FAILED)
        return 18;
    rw_ptr[0] = 0;

    struct drm_virtgpu_map ro_map = {.handle = p_ro.handle};
    if (ioctl(rfd, DRM_IOCTL_VIRTGPU_MAP, &ro_map) != 0)
        return 19;
    volatile uint32_t *ro_ptr = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, rfd, (off_t)ro_map.offset);
    if (ro_ptr == MAP_FAILED)
        return 20;
    ro_ptr[0] = 0x13579BDFU;
    ro_ptr[1] = 0x2468ACE0U;
    __sync_synchronize();
    munmap((void *)ro_ptr, PAGE_BYTES);

    struct drm_virtgpu_map kill_map = {.handle = p_kill.handle};
    if (ioctl(rfd, DRM_IOCTL_VIRTGPU_MAP, &kill_map) != 0)
        return 21;
    volatile uint32_t *kill_ptr = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, rfd, (off_t)kill_map.offset);
    if (kill_ptr == MAP_FAILED)
        return 22;
    kill_ptr[0] = 0x5A5AC3C3U;
    kill_ptr[1023] = 0xDEADC0DEU;
    __sync_synchronize();

    struct drm_syncobj_create sc = {.handle = 0, .flags = 0};
    if (ioctl(rfd, DRM_IOCTL_SYNCOBJ_CREATE, &sc) != 0)
        return 23;
    struct drm_syncobj_handle sh = {.handle = sc.handle, .flags = 0, .fd = -1};
    if (ioctl(rfd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &sh) != 0)
        return 24;

    struct drm_prime_handle exp_rw = {
        .handle = p_rw.handle,
        .flags = DRM_CLOEXEC | DRM_RDWR,
        .fd = -1,
    };
    if (ioctl(rfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &exp_rw) != 0)
        return 25;

    struct drm_prime_handle exp_ro = {
        .handle = p_ro.handle,
        .flags = DRM_CLOEXEC,
        .fd = -1,
    };
    if (ioctl(rfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &exp_ro) != 0)
        return 26;

    struct drm_prime_handle exp_kill = {
        .handle = p_kill.handle,
        .flags = DRM_CLOEXEC | DRM_RDWR,
        .fd = -1,
    };
    if (ioctl(rfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &exp_kill) != 0)
        return 27;

    struct init_payload init = {
        .rw_bo = p_rw.handle,
        .rw_res = rw_info.res_handle,
        .rw_size = rw_info.size,
        .ro_bo = p_ro.handle,
        .ro_res = ro_info.res_handle,
        .kill_bo = p_kill.handle,
        .kill_res = kill_info.res_handle,
    };
    int init_fds[4] = {exp_rw.fd, exp_ro.fd, exp_kill.fd, sh.fd};
    if (send_msg_fds(sock, &init, sizeof init, init_fds, 4) != 0)
        return 28;
    close(exp_rw.fd);
    close(exp_ro.fd);
    close(exp_kill.fd);
    close(sh.fd);

    uint32_t ack = 0;
    size_t nfds = 0;
    int dummy_fds[MAX_IPC_FDS];
    if (recv_msg_fds(sock, &ack, sizeof ack, dummy_fds, &nfds) != 0 || ack != 1U)
        return 29;

    uint32_t bo = p_rw.handle;
    for (uint32_t iter = 1; iter <= COUNTER_ITERS; iter++) {
        rw_ptr[0] = iter;
        rw_ptr[1] = iter ^ 0xA5A55A5AU;
        __sync_synchronize();

        if (iter == 1) {
            if (send_msg_fds(sock, &iter, sizeof iter, NULL, 0) != 0)
                return 30;
            if (recv_msg_fds(sock, &ack, sizeof ack, dummy_fds, &nfds) != 0 || ack != 0xBEEFU)
                return 31;
            uint32_t s_handle = sc.handle;
            uint64_t s_point = 1;
            struct drm_syncobj_timeline_array sig = {
                .handles = (uintptr_t)&s_handle,
                .points = (uintptr_t)&s_point,
                .count_handles = 1,
                .flags = 0,
            };
            if (ioctl(rfd, DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &sig) != 0)
                return 32;
            if (recv_msg_fds(sock, &ack, sizeof ack, dummy_fds, &nfds) != 0 || ack != iter)
                return 33;
            continue;
        }

        struct drm_virtgpu_execbuffer eb = {
            .flags = VIRTGPU_EXECBUF_RING_IDX | VIRTGPU_EXECBUF_FENCE_FD_OUT,
            .ring_idx = 0,
            .bo_handles = (uintptr_t)&bo,
            .num_bo_handles = 1,
            .fence_fd = -1,
        };
        if (ioctl(rfd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &eb) != 0 || eb.fence_fd < 0)
            return 34;
        if (send_msg_fds(sock, &iter, sizeof iter, &eb.fence_fd, 1) != 0) {
            close(eb.fence_fd);
            return 35;
        }
        close(eb.fence_fd);
        if (recv_msg_fds(sock, &ack, sizeof ack, dummy_fds, &nfds) != 0 || ack != iter)
            return 36;
    }

    for (unsigned wait = 0; wait < 600; wait++) {
        struct timespec ts = {.tv_sec = 0, .tv_nsec = 50000000};
        nanosleep(&ts, NULL);
    }
    return 0;
}

int main(void)
{
    char render[256], path[300], detail[256];
    setvbuf(stdout, NULL, _IOLBF, 0);

    if (!find_render_name(render, sizeof render)) {
        fail("render-name", "no gpu.* name in /scheme");
        printf(TAG ": FAIL\n");
        return 1;
    }
    printf(TAG ": render %s\n", render);
    pass("render-name", render);
    snprintf(path, sizeof path, "/scheme/%s", render);

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        fail("socketpair", strerror(errno));
        printf(TAG ": FAIL\n");
        return 1;
    }

    pid_t prod = fork();
    if (prod < 0) {
        fail("fork-producer", strerror(errno));
        printf(TAG ": FAIL\n");
        return 1;
    }
    if (prod == 0) {
        close(sv[0]);
        _exit(run_producer(path, sv[1]));
    }
    close(sv[1]);

    struct init_payload init;
    int init_fds[MAX_IPC_FDS];
    size_t nfds = 0;
    if (recv_msg_fds(sv[0], &init, sizeof init, init_fds, &nfds) != 0 || nfds != 4) {
        int st = 0;
        kill(prod, SIGKILL);
        waitpid(prod, &st, 0);
        snprintf(detail, sizeof detail, "init recv failed (nfds=%zu prod_status=%d)", nfds, st);
        fail("producer-init", detail);
        printf(TAG ": FAIL\n");
        return 1;
    }
    int rw_fd = init_fds[0];
    int ro_fd = init_fds[1];
    int kill_fd = init_fds[2];
    int sync_fd = init_fds[3];
    pass("producer-init", "received rw/ro/kill dmabufs and syncobj fd; non-shareable and bad flags refused");

    int cfd = open(path, O_RDWR | O_CLOEXEC);
    if (cfd < 0 || init_venus_context(cfd, TAG "-consumer") != 0) {
        fail("consumer-open", strerror(errno));
        kill(prod, SIGKILL);
        waitpid(prod, NULL, 0);
        printf(TAG ": FAIL\n");
        return 1;
    }

    struct drm_prime_handle bad_import_flags = {.handle = 0, .flags = 1U, .fd = rw_fd};
    expect_errno("import-bad-flags", ioctl(cfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &bad_import_flags), EINVAL);

    struct drm_prime_handle imp_rw = {.handle = 0, .flags = 0, .fd = rw_fd};
    if (ioctl(cfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp_rw) != 0 || imp_rw.handle == 0) {
        fail("import-rw", strerror(errno));
    } else {
        struct drm_prime_handle imp_rw2 = {.handle = 0, .flags = 0, .fd = rw_fd};
        if (ioctl(cfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp_rw2) == 0 && imp_rw2.handle == imp_rw.handle)
            pass("import-rw-dedup", "second import returned existing handle");
        else
            fail("import-rw-dedup", "re-import returned different handle or failed");

        struct drm_virtgpu_resource_info info = {.bo_handle = imp_rw.handle};
        if (ioctl(cfd, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, &info) == 0 &&
            info.res_handle == init.rw_res && info.size == init.rw_size &&
            info.blob_mem == VIRTGPU_BLOB_MEM_HOST3D) {
            pass("import-rw-info", "RESOURCE_INFO matched res_id, size, and blob_mem");
        } else {
            fail("import-rw-info", "RESOURCE_INFO mismatch on imported blob");
        }
    }

    void *ro_bad_fd = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, ro_fd, 0);
    if (ro_bad_fd == MAP_FAILED && errno == EACCES) {
        pass("ro-fd-mmap-write", "refused with EACCES");
    } else {
        if (ro_bad_fd != MAP_FAILED)
            munmap(ro_bad_fd, PAGE_BYTES);
        fail("ro-fd-mmap-write", "expected EACCES on writable mmap of read-only dmabuf fd");
    }

    void *ro_ok_fd = mmap(NULL, PAGE_BYTES, PROT_READ, MAP_SHARED, ro_fd, 0);
    if (ro_ok_fd != MAP_FAILED) {
        const volatile uint32_t *p = (const volatile uint32_t *)ro_ok_fd;
        if (p[0] == 0x13579BDFU && p[1] == 0x2468ACE0U)
            pass("ro-fd-mmap-read", "read-only mmap of dmabuf fd matched producer pattern");
        else
            fail("ro-fd-mmap-read", "data mismatch");
        munmap(ro_ok_fd, PAGE_BYTES);
    } else {
        fail("ro-fd-mmap-read", strerror(errno));
    }

    struct drm_prime_handle imp_ro = {.handle = 0, .flags = 0, .fd = ro_fd};
    if (ioctl(cfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp_ro) != 0 || imp_ro.handle == 0) {
        fail("import-ro", strerror(errno));
    } else {
        struct drm_virtgpu_map ro_map = {.handle = imp_ro.handle};
        if (ioctl(cfd, DRM_IOCTL_VIRTGPU_MAP, &ro_map) != 0) {
            fail("map-ro-import", strerror(errno));
        } else {
            void *ro_bad_imp = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, cfd, (off_t)ro_map.offset);
            if (ro_bad_imp == MAP_FAILED && errno == EACCES) {
                pass("ro-import-mmap-write", "refused with EACCES");
            } else {
                if (ro_bad_imp != MAP_FAILED)
                    munmap(ro_bad_imp, PAGE_BYTES);
                fail("ro-import-mmap-write", "expected EACCES on writable mmap of read-only import");
            }

            void *ro_ok_imp = mmap(NULL, PAGE_BYTES, PROT_READ, MAP_SHARED, cfd, (off_t)ro_map.offset);
            if (ro_ok_imp != MAP_FAILED) {
                const volatile uint32_t *p = (const volatile uint32_t *)ro_ok_imp;
                if (p[0] == 0x13579BDFU && p[1] == 0x2468ACE0U)
                    pass("ro-import-mmap-read", "read-only mmap of imported handle matched pattern");
                else
                    fail("ro-import-mmap-read", "data mismatch");
                munmap(ro_ok_imp, PAGE_BYTES);
            } else {
                fail("ro-import-mmap-read", strerror(errno));
            }

            struct drm_prime_handle escalate = {
                .handle = imp_ro.handle,
                .flags = DRM_CLOEXEC | DRM_RDWR,
                .fd = -1,
            };
            expect_errno("ro-import-reexport-rw", ioctl(cfd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &escalate), EACCES);
        }
    }
    close(ro_fd);

    struct drm_prime_handle imp_kill = {.handle = 0, .flags = 0, .fd = kill_fd};
    if (ioctl(cfd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp_kill) != 0 || imp_kill.handle == 0)
        fail("import-kill-blob", strerror(errno));
    close(kill_fd);

    pid_t guesser = fork();
    if (guesser == 0) {
        _exit(run_guesser(path, init.rw_bo, init.rw_res));
    }
    int gstatus = 0;
    waitpid(guesser, &gstatus, 0);
    if (WIFEXITED(gstatus) && WEXITSTATUS(gstatus) == 0)
        pass("third-process-isolation", "guessed bo_handle and res_id refused on render and display handles");
    else {
        snprintf(detail, sizeof detail, "guesser exited with status 0x%x", gstatus);
        fail("third-process-isolation", detail);
    }

    struct drm_syncobj_handle imp_sync = {.handle = 0, .flags = 0, .fd = sync_fd};
    if (ioctl(cfd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &imp_sync) != 0)
        fail("import-syncobj", strerror(errno));
    close(sync_fd);

    struct drm_virtgpu_map c_rw_map = {.handle = imp_rw.handle};
    volatile uint32_t *c_rw_ptr = MAP_FAILED;
    if (ioctl(cfd, DRM_IOCTL_VIRTGPU_MAP, &c_rw_map) == 0)
        c_rw_ptr = mmap(NULL, init.rw_size, PROT_READ, MAP_SHARED, cfd, (off_t)c_rw_map.offset);
    if (c_rw_ptr == MAP_FAILED)
        fail("consumer-mmap-rw", strerror(errno));
    close(rw_fd);

    uint32_t ready_ack = 1U;
    send_msg_fds(sv[0], &ready_ack, sizeof ready_ack, NULL, 0);

    uint32_t unsignaled_on_arrival = 0;
    uint32_t counters_ok = 0;
    for (uint32_t iter = 1; iter <= COUNTER_ITERS; iter++) {
        uint32_t got_iter = 0;
        int ffd[MAX_IPC_FDS];
        size_t got_nfds = 0;
        if (recv_msg_fds(sv[0], &got_iter, sizeof got_iter, ffd, &got_nfds) != 0 || got_iter != iter) {
            snprintf(detail, sizeof detail, "counter %u ipc recv failed", iter);
            fail("counters-1000", detail);
            break;
        }
        if (iter == 1) {
            uint32_t sh = imp_sync.handle;
            uint64_t pt = 1;
            struct drm_syncobj_timeline_wait w0 = {
                .handles = (uintptr_t)&sh,
                .points = (uintptr_t)&pt,
                .timeout_nsec = 0,
                .count_handles = 1,
                .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
            };
            if (ioctl(cfd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w0) < 0 && errno == ETIME) {
                unsignaled_on_arrival++;
            }
            uint32_t go = 0xBEEFU;
            send_msg_fds(sv[0], &go, sizeof go, NULL, 0);
            struct drm_syncobj_timeline_wait w1 = {
                .handles = (uintptr_t)&sh,
                .points = (uintptr_t)&pt,
                .timeout_nsec = monotonic_ns() + 5000000000LL,
                .count_handles = 1,
                .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
            };
            if (ioctl(cfd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w1) != 0) {
                fail("counters-1000", "timeline wait for point 1 failed");
                break;
            }
        } else {
            if (got_nfds != 1) {
                fail("counters-1000", "missing fence fd");
                break;
            }
            struct pollfd pfd = {.fd = ffd[0], .events = POLLIN, .revents = 0};
            if (poll(&pfd, 1, 0) == 0)
                unsignaled_on_arrival++;
            int pr = poll(&pfd, 1, 5000);
            close(ffd[0]);
            if (pr <= 0 || !(pfd.revents & POLLIN)) {
                fail("counters-1000", "fence poll timed out");
                break;
            }
        }

        if (c_rw_ptr != MAP_FAILED && c_rw_ptr[0] == iter && c_rw_ptr[1] == (iter ^ 0xA5A55A5AU)) {
            counters_ok++;
        } else {
            snprintf(detail, sizeof detail, "counter mismatch at %u", iter);
            fail("counters-1000", detail);
            break;
        }
        send_msg_fds(sv[0], &iter, sizeof iter, NULL, 0);
    }

    printf(TAG ": counters %u unsignaled %u\n", counters_ok, unsignaled_on_arrival);
    if (counters_ok == COUNTER_ITERS && unsignaled_on_arrival >= 1) {
        snprintf(detail, sizeof detail, "%u/%u counters matched, %u fence(s) unsignaled on arrival",
                 counters_ok, COUNTER_ITERS, unsignaled_on_arrival);
        pass("counters-1000", detail);
    } else {
        snprintf(detail, sizeof detail, "counters=%u unsignaled=%u", counters_ok, unsignaled_on_arrival);
        fail("counters-1000", detail);
    }

    kill(prod, SIGKILL);
    waitpid(prod, NULL, 0);
    close(sv[0]);

    struct drm_virtgpu_map kmap = {.handle = imp_kill.handle};
    if (ioctl(cfd, DRM_IOCTL_VIRTGPU_MAP, &kmap) != 0) {
        fail("producer-kill-survival", strerror(errno));
    } else {
        void *kptr = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, cfd, (off_t)kmap.offset);
        if (kptr == MAP_FAILED) {
            fail("producer-kill-survival", strerror(errno));
        } else {
            volatile uint32_t *kw = (volatile uint32_t *)kptr;
            uint32_t w0 = kw[0];
            uint32_t wlast = kw[1023];
            uint32_t kbo = imp_kill.handle;
            struct drm_virtgpu_execbuffer keb = {
                .flags = VIRTGPU_EXECBUF_RING_IDX,
                .ring_idx = 0,
                .bo_handles = (uintptr_t)&kbo,
                .num_bo_handles = 1,
                .fence_fd = -1,
            };
            int eb_ok = ioctl(cfd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &keb) == 0;
            munmap(kptr, PAGE_BYTES);
            if (w0 == 0x5A5AC3C3U && wlast == 0xDEADC0DEU && eb_ok)
                pass("producer-kill-survival", "imported blob readable and usable in EXECBUFFER after SIGKILL");
            else
                fail("producer-kill-survival", "blob data or EXECBUFFER failed after producer SIGKILL");
        }
    }

    if (c_rw_ptr != MAP_FAILED)
        munmap((void *)c_rw_ptr, init.rw_size);
    close(cfd);

    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
