#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <drm.h>
#include <gbm.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <vulkan/vulkan.h>
#include <xf86drm.h>

#define TAG "gpu-probe-gbm"
#define MAX_SCHEME_ENTRIES 4096U
#define MAX_VK_DEVS 16U
#define MAX_VK_EXTS 1024U
#define MAX_VK_QUEUES 32U
#define PROBE_WIDTH 256U
#define PROBE_HEIGHT 256U

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

static bool has_ext(const VkExtensionProperties *exts, uint32_t count, const char *name)
{
    for (uint32_t i = 0; i < count; i++) {
        if (strcmp(exts[i].extensionName, name) == 0)
            return true;
    }
    return false;
}

static void probe_vulkan_dmabuf(void)
{
    char detail[256];
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = TAG,
        .apiVersion = VK_API_VERSION_1_2,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkInstance inst = VK_NULL_HANDLE;
    VkResult vr = vkCreateInstance(&ici, NULL, &inst);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkCreateInstance returned %d", (int)vr);
        fail("vk-instance", detail);
        return;
    }

    uint32_t dev_count = 0;
    vr = vkEnumeratePhysicalDevices(inst, &dev_count, NULL);
    if (vr != VK_SUCCESS || dev_count == 0) {
        snprintf(detail, sizeof detail, "vr=%d count=%u", (int)vr, dev_count);
        fail("vk-devices", detail);
        vkDestroyInstance(inst, NULL);
        return;
    }
    if (dev_count > MAX_VK_DEVS)
        dev_count = MAX_VK_DEVS;
    VkPhysicalDevice devs[MAX_VK_DEVS];
    vr = vkEnumeratePhysicalDevices(inst, &dev_count, devs);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "enumerate returned %d", (int)vr);
        fail("vk-devices", detail);
        vkDestroyInstance(inst, NULL);
        return;
    }

    VkPhysicalDevice pdev = VK_NULL_HANDLE;
    VkPhysicalDeviceProperties chosen_props;
    memset(&chosen_props, 0, sizeof chosen_props);
    for (uint32_t i = 0; i < dev_count; i++) {
        VkPhysicalDeviceProperties props;
        vkGetPhysicalDeviceProperties(devs[i], &props);
        if (props.deviceType != VK_PHYSICAL_DEVICE_TYPE_CPU) {
            pdev = devs[i];
            chosen_props = props;
            break;
        }
    }
    if (pdev == VK_NULL_HANDLE) {
        fail("vk-venus-device", "no non-CPU Vulkan physical device found");
        vkDestroyInstance(inst, NULL);
        return;
    }
    snprintf(detail, sizeof detail, "type=%u api=%u.%u.%u",
             (unsigned)chosen_props.deviceType,
             VK_VERSION_MAJOR(chosen_props.apiVersion),
             VK_VERSION_MINOR(chosen_props.apiVersion),
             VK_VERSION_PATCH(chosen_props.apiVersion));
    pass("vk-venus-device", detail);

    uint32_t ext_count = 0;
    vr = vkEnumerateDeviceExtensionProperties(pdev, NULL, &ext_count, NULL);
    if (vr != VK_SUCCESS || ext_count == 0) {
        snprintf(detail, sizeof detail, "vr=%d count=%u", (int)vr, ext_count);
        fail("vk-exts", detail);
        vkDestroyInstance(inst, NULL);
        return;
    }
    if (ext_count > MAX_VK_EXTS)
        ext_count = MAX_VK_EXTS;
    VkExtensionProperties *exts = calloc(ext_count, sizeof(*exts));
    if (!exts) {
        fail("vk-exts", "out of memory");
        vkDestroyInstance(inst, NULL);
        return;
    }
    vr = vkEnumerateDeviceExtensionProperties(pdev, NULL, &ext_count, exts);
    if (vr != VK_SUCCESS && vr != VK_INCOMPLETE) {
        snprintf(detail, sizeof detail, "enumerate exts returned %d", (int)vr);
        fail("vk-exts", detail);
        free(exts);
        vkDestroyInstance(inst, NULL);
        return;
    }

    bool ext_mem_fd = has_ext(exts, ext_count, VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME);
    bool ext_dmabuf = has_ext(exts, ext_count, VK_EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION_NAME);
    bool ext_mod = has_ext(exts, ext_count, VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_EXTENSION_NAME);
    bool ext_fence_fd = has_ext(exts, ext_count, VK_KHR_EXTERNAL_FENCE_FD_EXTENSION_NAME);
    bool ext_sem_fd = has_ext(exts, ext_count, VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME);
    free(exts);

    snprintf(detail, sizeof detail,
             "ext_mem_fd=%d ext_dmabuf=%d drm_mod=%d fence_fd=%d sem_fd=%d",
             ext_mem_fd ? 1 : 0, ext_dmabuf ? 1 : 0, ext_mod ? 1 : 0,
             ext_fence_fd ? 1 : 0, ext_sem_fd ? 1 : 0);
    if (ext_mem_fd && ext_dmabuf)
        pass("vk-dmabuf-exts", detail);
    else {
        fail("vk-dmabuf-exts", detail);
        vkDestroyInstance(inst, NULL);
        return;
    }

    if (ext_mod) {
        VkFormat fmts[2] = {VK_FORMAT_B8G8R8A8_UNORM, VK_FORMAT_R8G8B8A8_UNORM};
        const char *fmt_names[2] = {"B8G8R8A8_UNORM", "R8G8B8A8_UNORM"};
        for (uint32_t fi = 0; fi < 2; fi++) {
            VkDrmFormatModifierPropertiesListEXT mod_list = {
                .sType = VK_STRUCTURE_TYPE_DRM_FORMAT_MODIFIER_PROPERTIES_LIST_EXT,
            };
            VkFormatProperties2 fp2 = {
                .sType = VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_2,
                .pNext = &mod_list,
            };
            vkGetPhysicalDeviceFormatProperties2(pdev, fmts[fi], &fp2);
            uint32_t mcount = mod_list.drmFormatModifierCount;
            if (mcount > 64)
                mcount = 64;
            VkDrmFormatModifierPropertiesEXT mprops[64];
            bool linear_listed = false;
            uint32_t linear_feats = 0;
            if (mcount > 0) {
                mod_list.drmFormatModifierCount = mcount;
                mod_list.pDrmFormatModifierProperties = mprops;
                vkGetPhysicalDeviceFormatProperties2(pdev, fmts[fi], &fp2);
                for (uint32_t mi = 0; mi < mod_list.drmFormatModifierCount; mi++) {
                    if (mprops[mi].drmFormatModifier == 0ULL) {
                        linear_listed = true;
                        linear_feats = (uint32_t)mprops[mi].drmFormatModifierTilingFeatures;
                    }
                }
            }

            VkPhysicalDeviceImageDrmFormatModifierInfoEXT mod_info = {
                .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_DRM_FORMAT_MODIFIER_INFO_EXT,
                .drmFormatModifier = 0ULL,
                .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
            };
            VkPhysicalDeviceExternalImageFormatInfo ext_img_info = {
                .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO,
                .pNext = &mod_info,
                .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
            };
            VkPhysicalDeviceImageFormatInfo2 fmt_info = {
                .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2,
                .pNext = &ext_img_info,
                .format = fmts[fi],
                .type = VK_IMAGE_TYPE_2D,
                .tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
                .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
            };
            VkExternalImageFormatProperties ext_img_props = {
                .sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES,
            };
            VkImageFormatProperties2 fmt_props = {
                .sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2,
                .pNext = &ext_img_props,
            };
            vr = vkGetPhysicalDeviceImageFormatProperties2(pdev, &fmt_info, &fmt_props);
            char check_name[64];
            snprintf(check_name, sizeof check_name, "vk-dmabuf-linear-%s", fmt_names[fi]);
            if (vr == VK_SUCCESS) {
                VkExternalMemoryFeatureFlags feats =
                    ext_img_props.externalMemoryProperties.externalMemoryFeatures;
                snprintf(detail, sizeof detail,
                         "mods=%u linear_listed=%d lin_feats=0x%x ext_feats=0x%x export=%d import=%d",
                         mcount, linear_listed ? 1 : 0, linear_feats, (unsigned)feats,
                         (feats & VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT) ? 1 : 0,
                         (feats & VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT) ? 1 : 0);
                if (linear_listed && (feats & VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT))
                    pass(check_name, detail);
                else
                    fail(check_name, detail);
            } else {
                snprintf(detail, sizeof detail,
                         "mods=%u linear_listed=%d lin_feats=0x%x query_vr=%d",
                         mcount, linear_listed ? 1 : 0, linear_feats, (int)vr);
                fail(check_name, detail);
            }
        }
    } else {
        fail("vk-dmabuf-linear", "VK_EXT_image_drm_format_modifier missing");
    }

    uint32_t qcount = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(pdev, &qcount, NULL);
    if (qcount > MAX_VK_QUEUES)
        qcount = MAX_VK_QUEUES;
    VkQueueFamilyProperties qprops[MAX_VK_QUEUES];
    vkGetPhysicalDeviceQueueFamilyProperties(pdev, &qcount, qprops);
    uint32_t qfamily = UINT32_MAX;
    for (uint32_t i = 0; i < qcount; i++) {
        if (qprops[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) {
            qfamily = i;
            break;
        }
    }
    if (qfamily == UINT32_MAX) {
        fail("vk-queue", "no graphics queue family");
        vkDestroyInstance(inst, NULL);
        return;
    }

    const char *enabled_exts[3];
    uint32_t enabled_count = 0;
    enabled_exts[enabled_count++] = VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME;
    enabled_exts[enabled_count++] = VK_EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION_NAME;
    if (ext_mod)
        enabled_exts[enabled_count++] = VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_EXTENSION_NAME;

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = qfamily,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &qci,
        .enabledExtensionCount = enabled_count,
        .ppEnabledExtensionNames = enabled_exts,
    };
    VkDevice dev = VK_NULL_HANDLE;
    vr = vkCreateDevice(pdev, &dci, NULL, &dev);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkCreateDevice returned %d", (int)vr);
        fail("vk-device", detail);
        vkDestroyInstance(inst, NULL);
        return;
    }

    VkExternalMemoryImageCreateInfo emici = {
        .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
        .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    };
    VkImageCreateInfo img_ci = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        .pNext = &emici,
        .imageType = VK_IMAGE_TYPE_2D,
        .format = VK_FORMAT_B8G8R8A8_UNORM,
        .extent = {PROBE_WIDTH, PROBE_HEIGHT, 1},
        .mipLevels = 1,
        .arrayLayers = 1,
        .samples = VK_SAMPLE_COUNT_1_BIT,
        .tiling = VK_IMAGE_TILING_LINEAR,
        .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
        .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
    };
    VkImage img = VK_NULL_HANDLE;
    vr = vkCreateImage(dev, &img_ci, NULL, &img);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkCreateImage returned %d", (int)vr);
        fail("vk-dmabuf-image", detail);
        vkDestroyDevice(dev, NULL);
        vkDestroyInstance(inst, NULL);
        return;
    }

    VkMemoryRequirements req;
    vkGetImageMemoryRequirements(dev, img, &req);
    VkPhysicalDeviceMemoryProperties mprops;
    vkGetPhysicalDeviceMemoryProperties(pdev, &mprops);
    uint32_t mem_type = UINT32_MAX;
    for (uint32_t i = 0; i < mprops.memoryTypeCount; i++) {
        if (req.memoryTypeBits & (1U << i)) {
            mem_type = i;
            break;
        }
    }
    if (mem_type == UINT32_MAX) {
        fail("vk-dmabuf-alloc", "no matching memory type");
        vkDestroyImage(dev, img, NULL);
        vkDestroyDevice(dev, NULL);
        vkDestroyInstance(inst, NULL);
        return;
    }

    VkMemoryDedicatedAllocateInfo ded = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
        .image = img,
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
    VkDeviceMemory mem = VK_NULL_HANDLE;
    vr = vkAllocateMemory(dev, &mai, NULL, &mem);
    if (vr != VK_SUCCESS) {
        snprintf(detail, sizeof detail, "vkAllocateMemory returned %d (size=%llu type=%u)",
                 (int)vr, (unsigned long long)req.size, mem_type);
        fail("vk-dmabuf-alloc", detail);
        vkDestroyImage(dev, img, NULL);
        vkDestroyDevice(dev, NULL);
        vkDestroyInstance(inst, NULL);
        return;
    }
    snprintf(detail, sizeof detail, "size=%llu mem_type=%u", (unsigned long long)req.size, mem_type);
    pass("vk-dmabuf-alloc", detail);

    PFN_vkGetMemoryFdKHR get_fd = (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(dev, "vkGetMemoryFdKHR");
    if (!get_fd) {
        fail("vk-dmabuf-export", "vkGetMemoryFdKHR not found");
    } else {
        VkMemoryGetFdInfoKHR get_info = {
            .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
            .memory = mem,
            .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        };
        int dmabuf_fd = -1;
        errno = 0;
        vr = get_fd(dev, &get_info, &dmabuf_fd);
        if (vr == VK_SUCCESS && dmabuf_fd >= 0) {
            snprintf(detail, sizeof detail, "fd=%d", dmabuf_fd);
            pass("vk-dmabuf-export", detail);
            close(dmabuf_fd);
        } else {
            snprintf(detail, sizeof detail, "vkGetMemoryFdKHR returned %d fd=%d errno=%d (%s)",
                     (int)vr, dmabuf_fd, errno, strerror(errno));
            fail("vk-dmabuf-export", detail);
        }
    }

    vkFreeMemory(dev, mem, NULL);
    vkDestroyImage(dev, img, NULL);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
}

static void probe_gbm(const char *path)
{
    char detail[384];
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        snprintf(detail, sizeof detail, "open %s failed: %s", path, strerror(errno));
        fail("gbm-open-render", detail);
        return;
    }

    errno = 0;
    struct gbm_device *gbm = gbm_create_device(fd);
    if (!gbm) {
        snprintf(detail, sizeof detail, "gbm_create_device returned NULL (errno=%d %s)",
                 errno, strerror(errno));
        fail("gbm-create-device", detail);
        close(fd);
        return;
    }
    const char *backend = gbm_device_get_backend_name(gbm);
    snprintf(detail, sizeof detail, "backend=%s", backend ? backend : "(null)");
    pass("gbm-create-device", detail);

    uint64_t linear_mod = 0ULL;
    errno = 0;
    struct gbm_bo *bo = gbm_bo_create_with_modifiers2(
        gbm, PROBE_WIDTH, PROBE_HEIGHT, GBM_FORMAT_XRGB8888,
        &linear_mod, 1, GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
    const char *flags_label = "MOD_LINEAR|SCANOUT|RENDERING";
    if (!bo) {
        int mod_err = errno;
        errno = 0;
        bo = gbm_bo_create(
            gbm, PROBE_WIDTH, PROBE_HEIGHT, GBM_FORMAT_XRGB8888,
            GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR);
        if (!bo) {
            snprintf(detail, sizeof detail,
                     "Zink/GBM could not create BO (mod_err=%d %s, linear_err=%d %s)",
                     mod_err, strerror(mod_err), errno, strerror(errno));
            fail("gbm-bo-create", detail);
            gbm_device_destroy(gbm);
            close(fd);
            return;
        }
        flags_label = "SCANOUT|RENDERING|LINEAR";
    }
    uint32_t stride = gbm_bo_get_stride(bo);
    uint64_t modifier = gbm_bo_get_modifier(bo);
    snprintf(detail, sizeof detail, "flags=%s stride=%u modifier=0x%llx",
             flags_label, stride, (unsigned long long)modifier);
    if (modifier == 0ULL && stride >= PROBE_WIDTH * 4U)
        pass("gbm-bo-create", detail);
    else
        fail("gbm-bo-create", detail);

    errno = 0;
    int dmabuf_fd = gbm_bo_get_fd(bo);
    if (dmabuf_fd < 0) {
        snprintf(detail, sizeof detail, "gbm_bo_get_fd failed (ret=%d errno=%d %s)",
                 dmabuf_fd, errno, strerror(errno));
        fail("gbm-bo-export", detail);
    } else {
        uint32_t imported = 0;
        errno = 0;
        int imp_rc = drmPrimeFDToHandle(fd, dmabuf_fd, &imported);
        if (imp_rc != 0 || imported == 0) {
            snprintf(detail, sizeof detail,
                     "fd=%d stride=%u modifier=0x%llx reimport_rc=%d handle=%u errno=%d (%s)",
                     dmabuf_fd, stride, (unsigned long long)modifier,
                     imp_rc, imported, errno, strerror(errno));
            fail("gbm-bo-export", detail);
        } else {
            struct drm_gem_close gem_close = { .handle = imported };
            drmIoctl(fd, DRM_IOCTL_GEM_CLOSE, &gem_close);
            snprintf(detail, sizeof detail, "fd=%d stride=%u modifier=0x%llx handle=%u",
                     dmabuf_fd, stride, (unsigned long long)modifier, imported);
            pass("gbm-bo-export", detail);
        }
        close(dmabuf_fd);
    }

    gbm_bo_destroy(bo);
    gbm_device_destroy(gbm);
    close(fd);
}

int main(void)
{
    char render[256], path[300];
    setvbuf(stdout, NULL, _IOLBF, 0);

    if (!find_render_name(render, sizeof render)) {
        fail("render-name", "no gpu.* name in /scheme");
        printf(TAG ": FAIL\n");
        return 1;
    }
    pass("render-name", render);
    snprintf(path, sizeof path, "/scheme/%s", render);

    probe_vulkan_dmabuf();
    probe_gbm(path);

    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
