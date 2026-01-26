#ifndef PORTS_H
#define PORTS_H

#include <stdint.h>

/* Read a byte from the specified port */
static inline uint8_t inb(uint16_t port) {
    uint8_t result;
    __asm__ volatile("inb %1, %0" : "=a"(result) : "Nd"(port));
    return result;
}

/* Write a byte to the specified port */
static inline void outb(uint16_t port, uint8_t data) {
    __asm__ volatile("outb %0, %1" : : "a"(data), "Nd"(port));
}

/* Read a word (16 bits) from the specified port */
static inline uint16_t inw(uint16_t port) {
    uint16_t result;
    __asm__ volatile("inw %1, %0" : "=a"(result) : "Nd"(port));
    return result;
}

/* Write a word (16 bits) to the specified port */
static inline void outw(uint16_t port, uint16_t data) {
    __asm__ volatile("outw %0, %1" : : "a"(data), "Nd"(port));
}

/* I/O wait (small delay for slow devices) */
static inline void io_wait(void) {
    outb(0x80, 0);
}

#endif /* PORTS_H */
