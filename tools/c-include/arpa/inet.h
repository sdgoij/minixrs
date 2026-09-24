/* arpa/inet.h for the minix OS — the text-to-address conversions.
 *
 * Pulls in netinet/in.h, which is where `struct in_addr` and the byte-order
 * helpers live here as on glibc. IPv4 only: the port's net stack has no IPv6
 * addresses, so there is no inet_pton/inet_ntop to promise. */
#ifndef _ARPA_INET_H
#define _ARPA_INET_H

#include <netinet/in.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The old interface: 255.255.255.255 is indistinguishable from failure here,
 * so inet_aton is the one that reports a bad address properly. */
in_addr_t inet_addr(const char *cp);
int inet_aton(const char *cp, struct in_addr *inp);
/* Formats into one process-wide buffer that the next call overwrites. */
char *inet_ntoa(struct in_addr in);

#ifdef __cplusplus
}
#endif

#endif
