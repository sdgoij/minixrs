/* netinet/in.h for the minix OS — the Internet address family.
 *
 * The socket family and the `sockaddr` variants live in sys/socket.h here. This
 * header is what C code expects to find in <netinet/in.h> — the protocol
 * numbers, the socket options and the byte-order helpers. The numbering follows
 * Linux's, as the rest of the port's socket headers do. */
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
#define IPPROTO_ICMP 1
#define IPPROTO_IGMP 2
#define IPPROTO_IPIP 4
#define IPPROTO_TCP 6
#define IPPROTO_EGP 8
#define IPPROTO_PUP 12
#define IPPROTO_UDP 17
#define IPPROTO_IDP 22
#define IPPROTO_TP 29
#define IPPROTO_DCCP 33
#define IPPROTO_IPV6 41
#define IPPROTO_ROUTING 43
#define IPPROTO_FRAGMENT 44
#define IPPROTO_RSVP 46
#define IPPROTO_GRE 47
#define IPPROTO_ESP 50
#define IPPROTO_AH 51
#define IPPROTO_ICMPV6 58
#define IPPROTO_MTP 92
#define IPPROTO_BEETPH 94
#define IPPROTO_ENCAP 98
#define IPPROTO_PIM 103
#define IPPROTO_COMP 108
#define IPPROTO_SCTP 132
#define IPPROTO_MH 135
#define IPPROTO_UDPLITE 136
#define IPPROTO_MPLS 137
#define IPPROTO_RAW 255

/* IPv4 socket options (setsockopt level IPPROTO_IP). */
#define IP_TTL 2
#define IP_MULTICAST_IF 32
#define IP_MULTICAST_TTL 33
#define IP_MULTICAST_LOOP 34
#define IP_ADD_MEMBERSHIP 35
#define IP_DROP_MEMBERSHIP 36

/* IPv6 socket options (setsockopt level IPPROTO_IPV6). */
#define IPV6_UNICAST_HOPS 16
#define IPV6_MULTICAST_IF 17
#define IPV6_MULTICAST_HOPS 18
#define IPV6_MULTICAST_LOOP 19
#define IPV6_ADD_MEMBERSHIP 20
#define IPV6_DROP_MEMBERSHIP 21
#define IPV6_V6ONLY 26
#define IPV6_TCLASS 67

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
