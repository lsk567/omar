#ifndef STRING_H
#define STRING_H

#include <stddef.h>
#include <stdint.h>

/* Memory operations */
void *memset(void *dest, int val, size_t count);
void *memcpy(void *dest, const void *src, size_t count);
void *memmove(void *dest, const void *src, size_t count);
int memcmp(const void *ptr1, const void *ptr2, size_t count);

/* String operations */
size_t strlen(const char *str);
char *strcpy(char *dest, const char *src);
char *strncpy(char *dest, const char *src, size_t count);
int strcmp(const char *str1, const char *str2);
int strncmp(const char *str1, const char *str2, size_t count);
char *strcat(char *dest, const char *src);
char *strchr(const char *str, int c);

/* Number to string conversions */
void itoa(int value, char *str, int base);
void utoa(uint32_t value, char *str, int base);

#endif /* STRING_H */
