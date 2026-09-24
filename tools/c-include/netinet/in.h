/* netinet/in.h for the minix OS — the Internet address family.
 *
 * The socket family and `sockaddr_in` live in sys/socket.h here: MINIX's
 * addresses carry a length byte and a one-byte family, so they are not the
 * byte-for-byte glibc layout. This header is that plus what C code expects to
 * find in <netinet/in.h> — the address constants and the byte-order helpers. */
#ifndef _NETINET_IN_H
#define _NETINET_IN_H

#include <sys/socket.h>

typedef unsigned int in_addr_t;
typedef unsigned short in_port_t;

/* Host byte order, as in glibc: pass it through htonl() on the way into a
 * sockaddr_in. INADDR_ANY is zero, which needs no conversion. */
#define INADDR_ANY ((in_addr_t)0x00000000u)
#define INADDR_LOOPBACK ((in_addr_t)0x7f000001u)
#define INADDR_BROADCAST ((in_addr_t)0xffffffffu)
/* What inet_addr() answers for text that is not an address — and also the
 * address 255.255.255.255, which is why inet_aton exists. */
#define INADDR_NONE ((in_addr_t)0xffffffffu)

#define IPPROTO_IP 0
#define IPPROTO_TCP 6
#define IPPROTO_UDP 17

#ifdef __cplusplus
extern "C" {
#endif

unsigned short htons(unsigned short hostshort);
unsigned int htonl(unsigned int hostlong);
unsigned short ntohs(unsigned short netshort);
unsigned int ntohl(unsigned int netlong);

#ifdef __cplusplus
}
#endif

#endif
