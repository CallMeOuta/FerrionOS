#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

#define TAG "gpu-probe-scanout"
#define MAX_VK_DEVS 16U
#define MAX_VK_EXTS 1024U
#define MAX_VK_QUEUES 32U
#define FLIP_COUNT 600

#define DRM_IOC(dir, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | (0x64UL << 8) | (nr)))
#define DRM_IOWR(nr, type) DRM_IOC(3, nr, sizeof(type))

struct drm_prime_handle {
    uint32_t handle;
    uint32_t flags;
    int32_t fd;
};

struct drm_event {
    uint32_t type;
    uint32_t length;
};

struct drm_event_vblank {
    struct drm_event base;
    uint64_t user_data;
    uint32_t tv_sec;
    uint32_t tv_usec;
    uint32_t sequence;
    uint32_t crtc_id;
};

struct drm_mode_card_res {
    uint64_t fb_id_ptr;
    uint64_t crtc_id_ptr;
    uint64_t connector_id_ptr;
    uint64_t encoder_id_ptr;
    uint32_t count_fbs;
    uint32_t count_crtcs;
    uint32_t count_connectors;
    uint32_t count_encoders;
    uint32_t min_width;
    uint32_t max_width;
    uint32_t min_height;
    uint32_t max_height;
};

struct drm_mode_modeinfo {
    uint32_t clock;
    uint16_t hdisplay, hsync_start, hsync_end, htotal, hskew;
    uint16_t vdisplay, vsync_start, vsync_end, vtotal, vscan;
    uint32_t vrefresh;
    uint32_t flags;
    uint32_t type;
    char name[32];
};

struct drm_mode_crtc {
    uint64_t set_connectors_ptr;
    uint32_t count_connectors;
    uint32_t crtc_id;
    uint32_t fb_id;
    uint32_t x;
    uint32_t y;
    uint32_t gamma_size;
    uint32_t mode_valid;
    struct drm_mode_modeinfo mode;
};

struct drm_mode_get_encoder {
    uint32_t encoder_id;
    uint32_t encoder_type;
    uint32_t crtc_id;
    uint32_t possible_crtcs;
    uint32_t possible_clones;
};

struct drm_mode_get_connector {
    uint64_t encoders_ptr;
    uint64_t modes_ptr;
    uint64_t props_ptr;
    uint64_t prop_values_ptr;
    uint32_t count_modes;
    uint32_t count_props;
    uint32_t count_encoders;
    uint32_t encoder_id;
    uint32_t connector_id;
    uint32_t connector_type;
    uint32_t connector_type_id;
    uint32_t connection;
    uint32_t mm_width;
    uint32_t mm_height;
    uint32_t subpixel;
    uint32_t pad;
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

struct drm_mode_crtc_page_flip {
    uint32_t crtc_id;
    uint32_t fb_id;
    uint32_t flags;
    uint32_t reserved;
    uint64_t user_data;
};

struct inputd_control_event {
    size_t kind;
    size_t data;
};

#define DRM_IOCTL_PRIME_FD_TO_HANDLE DRM_IOWR(0x2e, struct drm_prime_handle)
#define DRM_IOCTL_MODE_GETRESOURCES DRM_IOWR(0xa0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_SETCRTC DRM_IOWR(0xa2, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_GETENCODER DRM_IOWR(0xa6, struct drm_mode_get_encoder)
#define DRM_IOCTL_MODE_GETCONNECTOR DRM_IOWR(0xa7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_RMFB DRM_IOWR(0xaf, unsigned int)
#define DRM_IOCTL_MODE_PAGE_FLIP DRM_IOWR(0xb0, struct drm_mode_crtc_page_flip)
#define DRM_IOCTL_MODE_ADDFB2 DRM_IOWR(0xb8, struct drm_mode_fb_cmd2)

#define DRM_MODE_CONNECTED 1U
#define DRM_MODE_PAGE_FLIP_EVENT 0x01U
#define DRM_MODE_FB_MODIFIERS (1U << 1)
#define DRM_EVENT_FLIP_COMPLETE 0x02U

#define DRM_FORMAT_ARGB8888 0x34325241U
#define DRM_FORMAT_XRGB8888 0x34325258U
#define DRM_FORMAT_ABGR8888 0x34324241U
#define DRM_FORMAT_XBGR8888 0x34324258U
#define DRM_FORMAT_RGB565 0x36314752U

extern ssize_t redox_fpath(int fd, void *buf, size_t count);

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

static int cmp_u64(const void *a, const void *b)
{
    uint64_t lhs = *(const uint64_t *)a;
    uint64_t rhs = *(const uint64_t *)b;
    return (lhs > rhs) - (lhs < rhs);
}

static bool has_ext(const VkExtensionProperties *exts, uint32_t count, const char *name)
{
    for (uint32_t i = 0; i < count; i++) {
        if (strcmp(exts[i].extensionName, name) == 0)
            return true;
    }
    return false;
}

static int activate_vt_from_drm_fd(int drm_fd, size_t *vt_out)
{
    char path[256];
    ssize_t n = redox_fpath(drm_fd, path, sizeof path - 1);
    if (n <= 0)
        return -1;
    path[n] = '\0';
    const char *slash = strrchr(path, '/');
    if (!slash || slash[1] == '\0')
        return -1;
    char *end = NULL;
    unsigned long vt = strtoul(slash + 1, &end, 10);
    if (!end || *end != '\0')
        return -1;
    int ctrl = open("/scheme/input/control", O_WRONLY | O_CLOEXEC);
    if (ctrl < 0)
        return -1;
    struct inputd_control_event cmd = {.kind = 1, .data = (size_t)vt};
    ssize_t written = write(ctrl, &cmd, sizeof cmd);
    int saved = errno;
    close(ctrl);
    errno = saved;
    if (written != (ssize_t)sizeof cmd)
        return -1;
    *vt_out = (size_t)vt;
    printf(TAG ": display %s vt %zu\n", path, *vt_out);
    return 0;
}

static int wait_for_event(int drm_fd, struct drm_event_vblank *ev, int timeout_ms)
{
    struct pollfd pfd = {.fd = drm_fd, .events = POLLIN, .revents = 0};
    int pr = poll(&pfd, 1, timeout_ms);
    if (pr <= 0)
        return pr == 0 ? -ETIMEDOUT : -errno;
    if (!(pfd.revents & POLLIN))
        return -EIO;
    ssize_t n = read(drm_fd, ev, sizeof *ev);
    if (n < 0)
        return -errno;
    if (n != (ssize_t)sizeof *ev)
        return -EIO;
    return 0;
}

static void wait_stdin_line(void)
{
    char line[64];
    (void)fgets(line, sizeof line, stdin);
}

struct vk_ctx {
    VkInstance inst;
    VkPhysicalDevice pdev;
    VkDevice dev;
    VkQueue queue;
    VkCommandPool pool;
    uint32_t qfamily;
    PFN_vkGetMemoryFdKHR get_fd;
};

struct exported_image {
    VkImage img;
    VkDeviceMemory mem;
    uint32_t pitch;
    uint32_t offset;
    uint64_t alloc_size;
    int dmabuf_fd;
};

static int vk_init(struct vk_ctx *ctx)
{
    char detail[256];
    memset(ctx, 0, sizeof *ctx);

    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = TAG,
        .apiVersion = VK_API_VERSION_1_2,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkResult vr = vkCreateInstance(&ici, NULL, &ctx->inst);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkCreateInstance returned %d", (int)vr);
        fail("vk-init", detail);
        return -1;
    }

    uint32_t dev_count = 0;
    vr = vkEnumeratePhysicalDevices(ctx->inst, &dev_count, NULL);
    if (vr != VK_SUCCESS || dev_count == 0) {
        fail("vk-init", "no Vulkan physical devices");
        return -1;
    }
    if (dev_count > MAX_VK_DEVS)
        dev_count = MAX_VK_DEVS;
    VkPhysicalDevice devs[MAX_VK_DEVS];
    vr = vkEnumeratePhysicalDevices(ctx->inst, &dev_count, devs);
    if (vr != VK_SUCCESS) {
        fail("vk-init", "vkEnumeratePhysicalDevices failed");
        return -1;
    }

    for (uint32_t i = 0; i < dev_count; i++) {
        VkPhysicalDeviceProperties props;
        vkGetPhysicalDeviceProperties(devs[i], &props);
        if (props.deviceType != VK_PHYSICAL_DEVICE_TYPE_CPU) {
            ctx->pdev = devs[i];
            break;
        }
    }
    if (ctx->pdev == VK_NULL_HANDLE) {
        fail("vk-init", "no non-CPU Vulkan physical device");
        return -1;
    }

    uint32_t ext_count = 0;
    vr = vkEnumerateDeviceExtensionProperties(ctx->pdev, NULL, &ext_count, NULL);
    if (vr != VK_SUCCESS || ext_count == 0) {
        fail("vk-init", "no device extensions");
        return -1;
    }
    if (ext_count > MAX_VK_EXTS)
        ext_count = MAX_VK_EXTS;
    VkExtensionProperties *exts = calloc(ext_count, sizeof(*exts));
    if (!exts) {
        fail("vk-init", "out of memory");
        return -1;
    }
    vkEnumerateDeviceExtensionProperties(ctx->pdev, NULL, &ext_count, exts);
    bool ok_exts = has_ext(exts, ext_count, VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME) &&
                   has_ext(exts, ext_count, VK_EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION_NAME) &&
                   has_ext(exts, ext_count, VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_EXTENSION_NAME);
    free(exts);
    if (!ok_exts) {
        fail("vk-init", "missing dma-buf or drm format modifier extension");
        return -1;
    }

    uint32_t qcount = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(ctx->pdev, &qcount, NULL);
    if (qcount > MAX_VK_QUEUES)
        qcount = MAX_VK_QUEUES;
    VkQueueFamilyProperties qprops[MAX_VK_QUEUES];
    vkGetPhysicalDeviceQueueFamilyProperties(ctx->pdev, &qcount, qprops);
    ctx->qfamily = UINT32_MAX;
    for (uint32_t i = 0; i < qcount; i++) {
        if (qprops[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) {
            ctx->qfamily = i;
            break;
        }
    }
    if (ctx->qfamily == UINT32_MAX) {
        fail("vk-init", "no graphics queue family");
        return -1;
    }

    const char *enabled_exts[3] = {
        VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
        VK_EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION_NAME,
        VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_EXTENSION_NAME,
    };
    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = ctx->qfamily,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &qci,
        .enabledExtensionCount = 3,
        .ppEnabledExtensionNames = enabled_exts,
    };
    vr = vkCreateDevice(ctx->pdev, &dci, NULL, &ctx->dev);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkCreateDevice returned %d", (int)vr);
        fail("vk-init", detail);
        return -1;
    }
    vkGetDeviceQueue(ctx->dev, ctx->qfamily, 0, &ctx->queue);

    VkCommandPoolCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .flags = VK_COMMAND_POOL_CREATE_TRANSIENT_BIT,
        .queueFamilyIndex = ctx->qfamily,
    };
    vr = vkCreateCommandPool(ctx->dev, &cpci, NULL, &ctx->pool);
    if (vr != VK_SUCCESS) {
        fail("vk-init", "vkCreateCommandPool failed");
        return -1;
    }

    ctx->get_fd = (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(ctx->dev, "vkGetMemoryFdKHR");
    if (!ctx->get_fd) {
        fail("vk-init", "vkGetMemoryFdKHR missing");
        return -1;
    }
    pass("vk-init", "Venus device and dma-buf modifier extensions ready");
    return 0;
}

static uint32_t find_mem_type(VkPhysicalDevice pdev, uint32_t type_bits, VkMemoryPropertyFlags req)
{
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pdev, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if ((type_bits & (1U << i)) && ((mp.memoryTypes[i].propertyFlags & req) == req))
            return i;
    }
    return UINT32_MAX;
}

static void fill_quadrants_bgra(uint8_t *dst, uint32_t width, uint32_t height, int is_xrgb)
{
    static const uint8_t argb_q[4][4] = {
        {0x18, 0x30, 0xE8, 0xFF},
        {0x48, 0xD4, 0x20, 0xFF},
        {0xEC, 0x54, 0x1C, 0xFF},
        {0x24, 0x90, 0xD8, 0xFF},
    };
    static const uint8_t xrgb_q[4][4] = {
        {0xE0, 0xC8, 0x34, 0x00},
        {0xB8, 0x40, 0xE4, 0x00},
        {0x28, 0xE0, 0xA0, 0x00},
        {0xD0, 0x38, 0x78, 0x00},
    };
    const uint8_t (*q)[4] = is_xrgb ? xrgb_q : argb_q;
    uint32_t half_w = width / 2;
    uint32_t half_h = height / 2;
    for (uint32_t y = 0; y < height; y++) {
        uint32_t row_q = (y < half_h) ? 0U : 2U;
        uint8_t *row = dst + (size_t)y * (size_t)width * 4U;
        for (uint32_t x = 0; x < width; x++) {
            uint32_t qi = row_q + ((x < half_w) ? 0U : 1U);
            uint8_t *px = row + (size_t)x * 4U;
            px[0] = q[qi][0];
            px[1] = q[qi][1];
            px[2] = q[qi][2];
            px[3] = q[qi][3];
        }
    }
}

static int create_and_export_image(struct vk_ctx *ctx, uint32_t width, uint32_t height,
                                   int is_xrgb, struct exported_image *out)
{
    char detail[256];
    const char *label = is_xrgb ? "vk-export-xrgb" : "vk-export-argb";
    memset(out, 0, sizeof *out);
    out->dmabuf_fd = -1;

    uint64_t linear_mod = 0ULL;
    VkImageDrmFormatModifierListCreateInfoEXT mod_list = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_LIST_CREATE_INFO_EXT,
        .drmFormatModifierCount = 1,
        .pDrmFormatModifiers = &linear_mod,
    };
    VkExternalMemoryImageCreateInfo emici = {
        .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
        .pNext = &mod_list,
        .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    };
    VkImageCreateInfo img_ci = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        .pNext = &emici,
        .imageType = VK_IMAGE_TYPE_2D,
        .format = VK_FORMAT_B8G8R8A8_UNORM,
        .extent = {width, height, 1},
        .mipLevels = 1,
        .arrayLayers = 1,
        .samples = VK_SAMPLE_COUNT_1_BIT,
        .tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
        .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
                 VK_IMAGE_USAGE_TRANSFER_DST_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
        .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
    };
    VkResult vr = vkCreateImage(ctx->dev, &img_ci, NULL, &out->img);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkCreateImage returned %d", (int)vr);
        fail(label, detail);
        return -1;
    }

    VkMemoryRequirements req;
    vkGetImageMemoryRequirements(ctx->dev, out->img, &req);
    uint32_t mem_type = find_mem_type(ctx->pdev, req.memoryTypeBits, 0);
    if (mem_type == UINT32_MAX) {
        fail(label, "no image memory type");
        return -1;
    }

    VkMemoryDedicatedAllocateInfo ded = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
        .image = out->img,
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
        .memoryTypeIndex = mem_type,
    };
    vr = vkAllocateMemory(ctx->dev, &mai, NULL, &out->mem);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkAllocateMemory returned %d", (int)vr);
        fail(label, detail);
        return -1;
    }
    vr = vkBindImageMemory(ctx->dev, out->img, out->mem, 0);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkBindImageMemory returned %d", (int)vr);
        fail(label, detail);
        return -1;
    }

    VkImageSubresource sub = {
        .aspectMask = VK_IMAGE_ASPECT_MEMORY_PLANE_0_BIT_EXT,
        .mipLevel = 0,
        .arrayLayer = 0,
    };
    VkSubresourceLayout layout;
    memset(&layout, 0, sizeof layout);
    vkGetImageSubresourceLayout(ctx->dev, out->img, &sub, &layout);
    if (layout.rowPitch < (VkDeviceSize)width * 4U || (layout.rowPitch & 3U) != 0 ||
        layout.offset > UINT32_MAX || layout.rowPitch > UINT32_MAX) {
        snprintf(detail, sizeof detail, "bad subresource layout pitch=%llu offset=%llu",
                 (unsigned long long)layout.rowPitch, (unsigned long long)layout.offset);
        fail(label, detail);
        return -1;
    }
    out->pitch = (uint32_t)layout.rowPitch;
    out->offset = (uint32_t)layout.offset;
    out->alloc_size = (uint64_t)req.size;

    VkDeviceSize staging_size = (VkDeviceSize)width * (VkDeviceSize)height * 4U;
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = staging_size,
        .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    VkBuffer staging_buf = VK_NULL_HANDLE;
    VkDeviceMemory staging_mem = VK_NULL_HANDLE;
    vr = vkCreateBuffer(ctx->dev, &bci, NULL, &staging_buf);
    if (vr != VK_SUCCESS) {
        fail(label, "staging vkCreateBuffer failed");
        return -1;
    }
    VkMemoryRequirements breq;
    vkGetBufferMemoryRequirements(ctx->dev, staging_buf, &breq);
    uint32_t btype = find_mem_type(
        ctx->pdev, breq.memoryTypeBits,
        VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    if (btype == UINT32_MAX) {
        vkDestroyBuffer(ctx->dev, staging_buf, NULL);
        fail(label, "no host-visible staging memory type");
        return -1;
    }
    VkMemoryAllocateInfo bmai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = breq.size,
        .memoryTypeIndex = btype,
    };
    vr = vkAllocateMemory(ctx->dev, &bmai, NULL, &staging_mem);
    if (vr != VK_SUCCESS || vkBindBufferMemory(ctx->dev, staging_buf, staging_mem, 0) != VK_SUCCESS) {
        vkDestroyBuffer(ctx->dev, staging_buf, NULL);
        if (staging_mem != VK_NULL_HANDLE)
            vkFreeMemory(ctx->dev, staging_mem, NULL);
        fail(label, "staging alloc/bind failed");
        return -1;
    }
    void *mapped = NULL;
    vr = vkMapMemory(ctx->dev, staging_mem, 0, staging_size, 0, &mapped);
    if (vr != VK_SUCCESS || !mapped) {
        vkFreeMemory(ctx->dev, staging_mem, NULL);
        vkDestroyBuffer(ctx->dev, staging_buf, NULL);
        fail(label, "staging vkMapMemory failed");
        return -1;
    }
    fill_quadrants_bgra((uint8_t *)mapped, width, height, is_xrgb);
    vkUnmapMemory(ctx->dev, staging_mem);

    VkCommandBufferAllocateInfo cbai = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = ctx->pool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    VkCommandBuffer cmd = VK_NULL_HANDLE;
    vr = vkAllocateCommandBuffers(ctx->dev, &cbai, &cmd);
    if (vr != VK_SUCCESS) {
        vkFreeMemory(ctx->dev, staging_mem, NULL);
        vkDestroyBuffer(ctx->dev, staging_buf, NULL);
        fail(label, "vkAllocateCommandBuffers failed");
        return -1;
    }
    VkCommandBufferBeginInfo cbbi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    vkBeginCommandBuffer(cmd, &cbbi);

    VkImageMemoryBarrier to_dst = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
        .srcAccessMask = 0,
        .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
        .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
        .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .image = out->img,
        .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1},
    };
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,
                         0, 0, NULL, 0, NULL, 1, &to_dst);

    VkBufferImageCopy region = {
        .bufferOffset = 0,
        .bufferRowLength = 0,
        .bufferImageHeight = 0,
        .imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
        .imageOffset = {0, 0, 0},
        .imageExtent = {width, height, 1},
    };
    vkCmdCopyBufferToImage(cmd, staging_buf, out->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);

    VkImageMemoryBarrier to_gen = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
        .dstAccessMask = VK_ACCESS_MEMORY_READ_BIT,
        .oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        .newLayout = VK_IMAGE_LAYOUT_GENERAL,
        .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .image = out->img,
        .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1},
    };
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT,
                         0, 0, NULL, 0, NULL, 1, &to_gen);
    vkEndCommandBuffer(cmd);

    VkSubmitInfo si = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers = &cmd,
    };
    vr = vkQueueSubmit(ctx->queue, 1, &si, VK_NULL_HANDLE);
    if (vr == VK_SUCCESS)
        vr = vkQueueWaitIdle(ctx->queue);
    vkFreeCommandBuffers(ctx->dev, ctx->pool, 1, &cmd);
    vkFreeMemory(ctx->dev, staging_mem, NULL);
    vkDestroyBuffer(ctx->dev, staging_buf, NULL);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "upload submit/wait returned %d", (int)vr);
        fail(label, detail);
        return -1;
    }

    VkMemoryGetFdInfoKHR gfi = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
        .memory = out->mem,
        .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    };
    vr = ctx->get_fd(ctx->dev, &gfi, &out->dmabuf_fd);
    if (vr != VK_SUCCESS || out->dmabuf_fd < 0) {
        snprintf(detail, sizeof detail, "vkGetMemoryFdKHR returned %d fd=%d errno=%d (%s)",
                 (int)vr, out->dmabuf_fd, errno, strerror(errno));
        fail(label, detail);
        return -1;
    }

    snprintf(detail, sizeof detail, "fd=%d pitch=%u offset=%u size=%llu",
             out->dmabuf_fd, out->pitch, out->offset, (unsigned long long)out->alloc_size);
    pass(label, detail);
    return 0;
}

static void destroy_vk_all(struct vk_ctx *ctx, struct exported_image *a, struct exported_image *b)
{
    if (a && a->dmabuf_fd >= 0) {
        close(a->dmabuf_fd);
        a->dmabuf_fd = -1;
    }
    if (b && b->dmabuf_fd >= 0) {
        close(b->dmabuf_fd);
        b->dmabuf_fd = -1;
    }
    if (ctx->dev != VK_NULL_HANDLE) {
        if (a && a->img != VK_NULL_HANDLE) {
            vkDestroyImage(ctx->dev, a->img, NULL);
            a->img = VK_NULL_HANDLE;
        }
        if (a && a->mem != VK_NULL_HANDLE) {
            vkFreeMemory(ctx->dev, a->mem, NULL);
            a->mem = VK_NULL_HANDLE;
        }
        if (b && b->img != VK_NULL_HANDLE) {
            vkDestroyImage(ctx->dev, b->img, NULL);
            b->img = VK_NULL_HANDLE;
        }
        if (b && b->mem != VK_NULL_HANDLE) {
            vkFreeMemory(ctx->dev, b->mem, NULL);
            b->mem = VK_NULL_HANDLE;
        }
        if (ctx->pool != VK_NULL_HANDLE) {
            vkDestroyCommandPool(ctx->dev, ctx->pool, NULL);
            ctx->pool = VK_NULL_HANDLE;
        }
        vkDestroyDevice(ctx->dev, NULL);
        ctx->dev = VK_NULL_HANDLE;
    }
    if (ctx->inst != VK_NULL_HANDLE) {
        vkDestroyInstance(ctx->inst, NULL);
        ctx->inst = VK_NULL_HANDLE;
    }
}

int main(int argc, char **argv)
{
    int hold = 0;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--hold") == 0)
            hold = 1;
    }
    setvbuf(stdout, NULL, _IOLBF, 0);

    int input_fd = open("/scheme/input/consumer", O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (input_fd < 0) {
        fail("open-consumer", strerror(errno));
        printf(TAG ": FAIL\n");
        return 1;
    }
    int drm_fd = openat(input_fd, "display", O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if (drm_fd < 0) {
        fail("open-display", strerror(errno));
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    size_t vt = 0;
    if (activate_vt_from_drm_fd(drm_fd, &vt) < 0) {
        fail("activate-vt", strerror(errno));
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    pass("open-vt", "acquired consumer and activated VT");

    uint32_t fbs[32], crtcs[32], conns[32], encs[32];
    struct drm_mode_card_res res = {
        .fb_id_ptr = (uintptr_t)fbs,
        .crtc_id_ptr = (uintptr_t)crtcs,
        .connector_id_ptr = (uintptr_t)conns,
        .encoder_id_ptr = (uintptr_t)encs,
        .count_fbs = 32,
        .count_crtcs = 32,
        .count_connectors = 32,
        .count_encoders = 32,
    };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_GETRESOURCES, &res) < 0 || res.count_connectors == 0 || res.count_crtcs == 0) {
        fail("get-resources", strerror(errno));
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    uint32_t conn_id = 0, crtc_id = 0;
    struct drm_mode_modeinfo chosen_mode;
    memset(&chosen_mode, 0, sizeof chosen_mode);
    int found_conn = 0;
    for (uint32_t i = 0; i < res.count_connectors && i < 32; i++) {
        uint32_t c_encs[16];
        struct drm_mode_modeinfo modes[32];
        uint32_t props[32];
        uint64_t prop_vals[32];
        struct drm_mode_get_connector conn = {
            .encoders_ptr = (uintptr_t)c_encs,
            .modes_ptr = (uintptr_t)modes,
            .props_ptr = (uintptr_t)props,
            .prop_values_ptr = (uintptr_t)prop_vals,
            .count_modes = 32,
            .count_props = 32,
            .count_encoders = 16,
            .connector_id = conns[i],
        };
        if (ioctl(drm_fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn) < 0)
            continue;
        if (conn.connection != DRM_MODE_CONNECTED || conn.count_modes == 0 || conn.count_encoders == 0)
            continue;
        struct drm_mode_get_encoder enc = {.encoder_id = c_encs[0]};
        if (ioctl(drm_fd, DRM_IOCTL_MODE_GETENCODER, &enc) < 0)
            continue;
        for (uint32_t c = 0; c < res.count_crtcs && c < 32; c++) {
            if (enc.possible_crtcs & (1U << c)) {
                conn_id = conns[i];
                crtc_id = crtcs[c];
                chosen_mode = modes[0];
                found_conn = 1;
                break;
            }
        }
        if (found_conn)
            break;
    }
    if (!found_conn) {
        fail("find-output", "no connected output with mode and CRTC");
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    uint32_t width = chosen_mode.hdisplay;
    uint32_t height = chosen_mode.vdisplay;
    char detail[256];
    snprintf(detail, sizeof detail, "crtc=%u conn=%u mode=%ux%u", crtc_id, conn_id, width, height);
    pass("output", detail);
    printf(TAG ": size %u %u\n", width, height);

    struct vk_ctx vk;
    struct exported_image img_argb = {.dmabuf_fd = -1};
    struct exported_image img_xrgb = {.dmabuf_fd = -1};
    if (vk_init(&vk) < 0 ||
        create_and_export_image(&vk, width, height, 0, &img_argb) < 0 ||
        create_and_export_image(&vk, width, height, 1, &img_xrgb) < 0) {
        destroy_vk_all(&vk, &img_argb, &img_xrgb);
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    struct drm_prime_handle imp_argb = {.handle = 0, .flags = 0, .fd = img_argb.dmabuf_fd};
    if (ioctl(drm_fd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp_argb) < 0 || imp_argb.handle == 0) {
        snprintf(detail, sizeof detail, "import argb on display handle failed: %d (%s)", errno, strerror(errno));
        fail("display-import-argb", detail);
        destroy_vk_all(&vk, &img_argb, &img_xrgb);
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    snprintf(detail, sizeof detail, "handle=%u (no EXDEV across gpu.* -> display.*)", imp_argb.handle);
    pass("display-import-argb", detail);

    struct drm_prime_handle imp_argb_dup = {.handle = 0, .flags = 0, .fd = img_argb.dmabuf_fd};
    if (ioctl(drm_fd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp_argb_dup) == 0 && imp_argb_dup.handle == imp_argb.handle)
        pass("display-import-dedup", "re-import on display handle returned same handle");
    else
        fail("display-import-dedup", "re-import failed or returned new handle");

    struct drm_prime_handle imp_xrgb = {.handle = 0, .flags = 0, .fd = img_xrgb.dmabuf_fd};
    if (ioctl(drm_fd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp_xrgb) < 0 || imp_xrgb.handle == 0) {
        snprintf(detail, sizeof detail, "import xrgb on display handle failed: %d (%s)", errno, strerror(errno));
        fail("display-import-xrgb", detail);
        destroy_vk_all(&vk, &img_argb, &img_xrgb);
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    snprintf(detail, sizeof detail, "handle=%u", imp_xrgb.handle);
    pass("display-import-xrgb", detail);

    struct drm_mode_fb_cmd2 bad_fmt = {
        .width = width,
        .height = height,
        .pixel_format = DRM_FORMAT_RGB565,
        .flags = DRM_MODE_FB_MODIFIERS,
        .handles = {imp_argb.handle, 0, 0, 0},
        .pitches = {img_argb.pitch, 0, 0, 0},
        .offsets = {img_argb.offset, 0, 0, 0},
        .modifier = {0, 0, 0, 0},
    };
    expect_errno("addfb2-bad-format", ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB2, &bad_fmt), EINVAL);

    struct drm_mode_fb_cmd2 bad_mod = {
        .width = width,
        .height = height,
        .pixel_format = DRM_FORMAT_ARGB8888,
        .flags = DRM_MODE_FB_MODIFIERS,
        .handles = {imp_argb.handle, 0, 0, 0},
        .pitches = {img_argb.pitch, 0, 0, 0},
        .offsets = {img_argb.offset, 0, 0, 0},
        .modifier = {1ULL, 0, 0, 0},
    };
    expect_errno("addfb2-bad-modifier", ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB2, &bad_mod), EINVAL);

    struct drm_mode_fb_cmd2 bad_pitch = {
        .width = width,
        .height = height,
        .pixel_format = DRM_FORMAT_ARGB8888,
        .flags = DRM_MODE_FB_MODIFIERS,
        .handles = {imp_argb.handle, 0, 0, 0},
        .pitches = {width * 4U - 4U, 0, 0, 0},
        .offsets = {img_argb.offset, 0, 0, 0},
        .modifier = {0, 0, 0, 0},
    };
    expect_errno("addfb2-bad-pitch", ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB2, &bad_pitch), EINVAL);

    struct drm_mode_fb_cmd2 bad_off = {
        .width = width,
        .height = height,
        .pixel_format = DRM_FORMAT_ARGB8888,
        .flags = DRM_MODE_FB_MODIFIERS,
        .handles = {imp_argb.handle, 0, 0, 0},
        .pitches = {img_argb.pitch, 0, 0, 0},
        .offsets = {(uint32_t)img_argb.alloc_size, 0, 0, 0},
        .modifier = {0, 0, 0, 0},
    };
    expect_errno("addfb2-bad-offset", ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB2, &bad_off), EINVAL);

    uint32_t bgr_fmts[2] = {DRM_FORMAT_ABGR8888, DRM_FORMAT_XBGR8888};
    int bgr_ok = 1;
    for (int i = 0; i < 2; i++) {
        struct drm_mode_fb_cmd2 tmp = {
            .width = width,
            .height = height,
            .pixel_format = bgr_fmts[i],
            .flags = DRM_MODE_FB_MODIFIERS,
            .handles = {imp_argb.handle, 0, 0, 0},
            .pitches = {img_argb.pitch, 0, 0, 0},
            .offsets = {img_argb.offset, 0, 0, 0},
            .modifier = {0, 0, 0, 0},
        };
        if (ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB2, &tmp) == 0 && tmp.fb_id != 0) {
            unsigned int id = tmp.fb_id;
            ioctl(drm_fd, DRM_IOCTL_MODE_RMFB, &id);
        } else {
            bgr_ok = 0;
        }
    }
    if (bgr_ok)
        pass("addfb2-bgr-formats", "ABGR8888 and XBGR8888 accepted");
    else
        fail("addfb2-bgr-formats", "ABGR8888 or XBGR8888 rejected");

    struct drm_mode_fb_cmd2 fb_argb = {
        .width = width,
        .height = height,
        .pixel_format = DRM_FORMAT_ARGB8888,
        .flags = DRM_MODE_FB_MODIFIERS,
        .handles = {imp_argb.handle, 0, 0, 0},
        .pitches = {img_argb.pitch, 0, 0, 0},
        .offsets = {img_argb.offset, 0, 0, 0},
        .modifier = {0, 0, 0, 0},
    };
    struct drm_mode_fb_cmd2 fb_xrgb = {
        .width = width,
        .height = height,
        .pixel_format = DRM_FORMAT_XRGB8888,
        .flags = DRM_MODE_FB_MODIFIERS,
        .handles = {imp_xrgb.handle, 0, 0, 0},
        .pitches = {img_xrgb.pitch, 0, 0, 0},
        .offsets = {img_xrgb.offset, 0, 0, 0},
        .modifier = {0, 0, 0, 0},
    };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB2, &fb_argb) < 0 || fb_argb.fb_id == 0 ||
        ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB2, &fb_xrgb) < 0 || fb_xrgb.fb_id == 0) {
        fail("addfb2-blobs", strerror(errno));
        destroy_vk_all(&vk, &img_argb, &img_xrgb);
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    snprintf(detail, sizeof detail, "fb_argb=%u fb_xrgb=%u", fb_argb.fb_id, fb_xrgb.fb_id);
    pass("addfb2-blobs", detail);

    struct drm_mode_crtc set_crtc = {
        .set_connectors_ptr = (uintptr_t)&conn_id,
        .count_connectors = 1,
        .crtc_id = crtc_id,
        .fb_id = fb_argb.fb_id,
        .mode_valid = 1,
        .mode = chosen_mode,
    };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_SETCRTC, &set_crtc) < 0) {
        fail("set-crtc-argb", strerror(errno));
        destroy_vk_all(&vk, &img_argb, &img_xrgb);
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    pass("set-crtc-argb", "ARGB8888 blob scanned out");

    if (hold) {
        printf(TAG ": READY argb\n");
        fflush(stdout);
        wait_stdin_line();
    }

    struct drm_mode_crtc_page_flip flip_xrgb = {
        .crtc_id = crtc_id,
        .fb_id = fb_xrgb.fb_id,
        .flags = DRM_MODE_PAGE_FLIP_EVENT,
        .reserved = 0,
        .user_data = 0x5343414E00000001ULL,
    };
    struct drm_event_vblank ev;
    if (ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &flip_xrgb) < 0 ||
        wait_for_event(drm_fd, &ev, 5000) != 0 ||
        ev.base.type != DRM_EVENT_FLIP_COMPLETE ||
        ev.user_data != 0x5343414E00000001ULL ||
        ev.crtc_id != crtc_id) {
        fail("flip-xrgb", "page flip to XRGB8888 blob failed");
        destroy_vk_all(&vk, &img_argb, &img_xrgb);
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    pass("flip-xrgb", "flipped to XRGB8888 blob");

    if (hold) {
        printf(TAG ": READY xrgb\n");
        fflush(stdout);
        wait_stdin_line();
    }

    destroy_vk_all(&vk, &img_argb, &img_xrgb);
    pass("creator-closed", "closed Vulkan device, memory, and dmabuf fds while scanning out");

    uint64_t intervals[FLIP_COUNT];
    uint64_t prev_ts = (uint64_t)ev.tv_sec * 1000000ULL + (uint64_t)ev.tv_usec;
    uint32_t prev_seq = ev.sequence;
    int events_ok = 0;
    for (int i = 1; i <= FLIP_COUNT; i++) {
        uint64_t cookie = 0x5343414E00010000ULL | (uint64_t)i;
        struct drm_mode_crtc_page_flip flip = {
            .crtc_id = crtc_id,
            .fb_id = (i & 1) ? fb_argb.fb_id : fb_xrgb.fb_id,
            .flags = DRM_MODE_PAGE_FLIP_EVENT,
            .reserved = 0,
            .user_data = cookie,
        };
        if (ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &flip) < 0) {
            snprintf(detail, sizeof detail, "flip %d ioctl failed: %s", i, strerror(errno));
            fail("flip-600", detail);
            break;
        }
        int werr = wait_for_event(drm_fd, &ev, 5000);
        if (werr != 0) {
            snprintf(detail, sizeof detail, "flip %d event failed: %s", i, strerror(-werr));
            fail("flip-600", detail);
            break;
        }
        uint64_t ts = (uint64_t)ev.tv_sec * 1000000ULL + (uint64_t)ev.tv_usec;
        if (ev.base.type != DRM_EVENT_FLIP_COMPLETE || ev.base.length != sizeof ev ||
            ev.user_data != cookie || ev.crtc_id != crtc_id ||
            ev.sequence <= prev_seq || ts < prev_ts) {
            snprintf(detail, sizeof detail, "flip %d bad event seq=%u(prev %u)", i, ev.sequence, prev_seq);
            fail("flip-600", detail);
            break;
        }
        intervals[i - 1] = ts - prev_ts;
        prev_ts = ts;
        prev_seq = ev.sequence;
        events_ok++;
    }
    if (events_ok == FLIP_COUNT) {
        qsort(intervals, FLIP_COUNT, sizeof intervals[0], cmp_u64);
        uint64_t min_us = intervals[0];
        uint64_t median_us = intervals[FLIP_COUNT / 2];
        uint64_t max_us = intervals[FLIP_COUNT - 1];
        printf(TAG ": flips %d events %d median_us %llu min_us %llu max_us %llu\n",
               FLIP_COUNT, events_ok, (unsigned long long)median_us,
               (unsigned long long)min_us, (unsigned long long)max_us);
        snprintf(detail, sizeof detail, "%d events after creator close, median %llu us",
                 events_ok, (unsigned long long)median_us);
        pass("flip-600", detail);
    }

    struct drm_mode_crtc_page_flip mid_flip = {
        .crtc_id = crtc_id,
        .fb_id = fb_argb.fb_id,
        .flags = DRM_MODE_PAGE_FLIP_EVENT,
        .reserved = 0,
        .user_data = 0x5343414EFFFFFFFFULL,
    };
    (void)ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &mid_flip);
    close(drm_fd);
    close(input_fd);

    int in2 = open("/scheme/input/consumer", O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    int d2 = (in2 >= 0) ? openat(in2, "display", O_RDWR | O_NONBLOCK | O_CLOEXEC) : -1;
    if (d2 >= 0) {
        struct drm_mode_card_res res2 = {
            .fb_id_ptr = (uintptr_t)fbs,
            .crtc_id_ptr = (uintptr_t)crtcs,
            .connector_id_ptr = (uintptr_t)conns,
            .encoder_id_ptr = (uintptr_t)encs,
            .count_fbs = 32,
            .count_crtcs = 32,
            .count_connectors = 32,
            .count_encoders = 32,
        };
        if (ioctl(d2, DRM_IOCTL_MODE_GETRESOURCES, &res2) == 0 && res2.count_crtcs > 0)
            pass("mid-flip-close", "display survived compositor close mid-flip and reopened cleanly");
        else
            fail("mid-flip-close", strerror(errno));
        close(d2);
    } else {
        fail("mid-flip-close", strerror(errno));
    }
    if (in2 >= 0)
        close(in2);

    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
