/* SPDX-License-Identifier: MIT
 * Query the Linux reference guest through EGL, Vulkan and virtio-gpu DRM.
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <vulkan/vulkan.h>
#include <libdrm/virtgpu_drm.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

static int gl_info(void) {
    EGLDisplay dpy = eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
    if (dpy == EGL_NO_DISPLAY || !eglInitialize(dpy, NULL, NULL)) return 1;
    for (int es = 0; es < 2; es++) {
        if (!eglBindAPI(es ? EGL_OPENGL_ES_API : EGL_OPENGL_API)) return 1;
        EGLContext ctx = EGL_NO_CONTEXT;
        if (es) {
            EGLint attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
            ctx = eglCreateContext(dpy, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, attrs);
        } else {
            for (int minor = 6; minor >= 0 && ctx == EGL_NO_CONTEXT; minor--) {
                EGLint attrs[] = {EGL_CONTEXT_MAJOR_VERSION, 4, EGL_CONTEXT_MINOR_VERSION, minor,
                    EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
                ctx = eglCreateContext(dpy, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, attrs);
            }
        }
        if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) return 1;
        const char *version = (const char *)glGetString(GL_VERSION);
        const char *renderer = (const char *)glGetString(GL_RENDERER);
        if (!version || !renderer) return 1;
        printf("%s\t%s\t%s\n", es ? "gles" : "gl", version, renderer);
        eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        eglDestroyContext(dpy, ctx);
    }
    eglTerminate(dpy);
    return 0;
}

static int vulkan_info(void) {
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "gpu-reference", .apiVersion = VK_API_VERSION_1_3};
    VkInstanceCreateInfo create = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance instance;
    VkResult rc = vkCreateInstance(&create, NULL, &instance);
    if (rc != VK_SUCCESS) { fprintf(stderr, "vkCreateInstance: %d\n", rc); return 1; }
    uint32_t count = 0;
    rc = vkEnumeratePhysicalDevices(instance, &count, NULL);
    if (rc != VK_SUCCESS || count > 64) return 1;
    VkPhysicalDevice devices[64];
    rc = vkEnumeratePhysicalDevices(instance, &count, devices);
    if (rc != VK_SUCCESS) return 1;
    for (uint32_t i = 0; i < count; i++) {
        VkPhysicalDeviceDriverProperties driver = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
        VkPhysicalDeviceProperties2 props = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, .pNext = &driver};
        vkGetPhysicalDeviceProperties2(devices[i], &props);
        VkPhysicalDeviceProperties *p = &props.properties;
        uint32_t extensions = 0;
        rc = vkEnumerateDeviceExtensionProperties(devices[i], NULL, &extensions, NULL);
        if (rc != VK_SUCCESS || extensions > 4096) return 1;
        VkExtensionProperties *list = calloc(extensions, sizeof(*list));
        if (!list) return 1;
        rc = vkEnumerateDeviceExtensionProperties(devices[i], NULL, &extensions, list);
        if (rc != VK_SUCCESS) return 1;
        printf("device\t%s\t%u.%u.%u\t%u.%u.%u.%u\t%u\t%04x\t%04x\t%s\t%s\t%u\n",
            p->deviceName, VK_API_VERSION_MAJOR(p->apiVersion), VK_API_VERSION_MINOR(p->apiVersion),
            VK_API_VERSION_PATCH(p->apiVersion), driver.conformanceVersion.major, driver.conformanceVersion.minor,
            driver.conformanceVersion.subminor, driver.conformanceVersion.patch, extensions, p->vendorID,
            p->deviceID, driver.driverName, driver.driverInfo, p->driverVersion);
        for (uint32_t j = 0; j < extensions; j++) printf("extension\t%s\n", list[j].extensionName);
        free(list);
    }
    vkDestroyInstance(instance, NULL);
    return count ? 0 : 1;
}

static int caps(const char *size_arg) {
    int fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    if (fd < 0) { perror("render node"); return 1; }
    for (unsigned param = 1; param <= 7; param++) {
        uint64_t value = 0;
        struct drm_virtgpu_getparam request = {.param = param, .value = (uintptr_t)&value};
        if (ioctl(fd, DRM_IOCTL_VIRTGPU_GETPARAM, &request) < 0) { perror("getparam"); return 1; }
        printf("param\t%u\t%llu\n", param, (unsigned long long)value);
    }
    if (size_arg) {
        char *end;
        unsigned long size = strtoul(size_arg, &end, 10);
        if (*end || !size || size > 65536) return 1;
        unsigned char *data = calloc(size, 1);
        if (!data) return 1;
        struct drm_virtgpu_get_caps request = {.cap_set_id = 4, .cap_set_ver = 0,
            .addr = (uintptr_t)data, .size = size};
        if (ioctl(fd, DRM_IOCTL_VIRTGPU_GET_CAPS, &request) < 0) { perror("getcaps"); return 1; }
        printf("venus_capset\t");
        for (unsigned long i = 0; i < size; i++) printf("%02x", data[i]);
        puts("");
        free(data);
    }
    close(fd);
    return 0;
}

#define MAPPED_BLOB_BYTES 4096
#define MAPPED_BLOB_PASSES 2

static int mapped_blob(void) {
    int result = 1;
    int fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    if (fd < 0) { perror("mapped blob render node"); return 1; }
    void *first = MAP_FAILED, *second = MAP_FAILED;
    struct drm_virtgpu_context_set_param param = {
        .param = VIRTGPU_CONTEXT_PARAM_CAPSET_ID, .value = VIRTGPU_DRM_CAPSET_VENUS,
    };
    struct drm_virtgpu_context_init context = {
        .num_params = 1, .ctx_set_params = (uintptr_t)&param,
    };
    if (ioctl(fd, DRM_IOCTL_VIRTGPU_CONTEXT_INIT, &context) < 0) {
        perror("mapped blob context"); goto cleanup;
    }
    struct drm_virtgpu_resource_create_blob blob = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = MAPPED_BLOB_BYTES,
    };
    if (ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &blob) < 0) {
        perror("mapped blob create"); goto cleanup;
    }
    struct drm_virtgpu_map mapping = {.handle = blob.bo_handle};
    if (ioctl(fd, DRM_IOCTL_VIRTGPU_MAP, &mapping) < 0) {
        perror("mapped blob offset"); goto cleanup;
    }
    first = mmap(NULL, MAPPED_BLOB_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, mapping.offset);
    if (first == MAP_FAILED) { perror("mapped blob first mmap"); goto cleanup; }
    second = mmap(NULL, MAPPED_BLOB_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, mapping.offset);
    if (second == MAP_FAILED) { perror("mapped blob second mmap"); goto cleanup; }
    size_t mismatches = 0;
    for (unsigned pass = 0; pass < MAPPED_BLOB_PASSES; pass++) {
        volatile uint8_t *writer = pass ? second : first;
        volatile uint8_t *reader = pass ? first : second;
        for (size_t i = 0; i < MAPPED_BLOB_BYTES; i++)
            writer[i] = (uint8_t)i ^ (pass ? 0xa5 : 0x5a);
        for (size_t i = 0; i < MAPPED_BLOB_BYTES; i++)
            mismatches += reader[i] != ((uint8_t)i ^ (pass ? 0xa5 : 0x5a));
    }
    printf("mapped_blob\t%u\t%u\t%u\t%zu\n", blob.res_handle,
           MAPPED_BLOB_BYTES, MAPPED_BLOB_PASSES, mismatches);
    result = mismatches ? 1 : 0;
cleanup:
    if (second != MAP_FAILED) munmap(second, MAPPED_BLOB_BYTES);
    if (first != MAP_FAILED) munmap(first, MAPPED_BLOB_BYTES);
    close(fd);
    return result;
}

int main(int argc, char **argv) {
    if (argc < 2) return 1;
    if (!strcmp(argv[1], "gl")) return gl_info();
    if (!strcmp(argv[1], "vulkan")) return vulkan_info();
    if (!strcmp(argv[1], "caps")) return caps(argc > 2 ? argv[2] : NULL);
    if (!strcmp(argv[1], "mapped-blob")) return mapped_blob();
    return 1;
}
