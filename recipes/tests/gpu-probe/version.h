#ifndef GPU_PROBE_VERSION_H
#define GPU_PROBE_VERSION_H

#include <string.h>

static int mesa_version_matches(const char *version)
{
    const char expected[] = "Mesa 26.3.1";
    const char *match = version ? strstr(version, expected) : NULL;
    if (!match || (match != version && match[-1] != ' '))
        return 0;
    char next = match[sizeof(expected) - 1];
    return next == '\0' || next == ' ';
}

#endif
