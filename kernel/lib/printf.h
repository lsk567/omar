#ifndef PRINTF_H
#define PRINTF_H

#include <stdarg.h>

/* Simple printf implementation for kernel */
int kprintf(const char *format, ...);

#endif /* PRINTF_H */
