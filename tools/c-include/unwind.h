/* Minimal unwind.h for the minix OS. The port loads no unwind tables, so the
 * walk reports the end of the stack; the declarations exist so a caller that
 * includes the header links against the same two symbols. */
#ifndef _UNWIND_H
#define _UNWIND_H

struct _Unwind_Context;
typedef int _Unwind_Reason_Code;
typedef unsigned long _Unwind_Ptr;
typedef unsigned long _Unwind_Word;
typedef unsigned long long _Unwind_Exception_Class;
typedef _Unwind_Reason_Code (*_Unwind_Trace_Fn)(struct _Unwind_Context *, void *);

#ifdef __cplusplus
extern "C" {
#endif

/* The Itanium C++ ABI header the compiler's `__cxa_exception` embeds. libc++abi
 * places it last in its own struct and derives the header's negative offset from
 * this size, so the four words and the alignment are the ABI, not a detail. */
typedef void (*_Unwind_Exception_Cleanup_Fn)(_Unwind_Reason_Code,
                                            struct _Unwind_Exception *);
struct _Unwind_Exception {
  _Unwind_Exception_Class exception_class;
  _Unwind_Exception_Cleanup_Fn exception_cleanup;
  _Unwind_Word private_1;
  _Unwind_Word private_2;
} __attribute__((__aligned__));

_Unwind_Reason_Code _Unwind_Backtrace(_Unwind_Trace_Fn cb, void *arg);
_Unwind_Ptr _Unwind_GetIP(struct _Unwind_Context *ctx);
_Unwind_Word _Unwind_GetCFA(struct _Unwind_Context *ctx);
void *_Unwind_FindEnclosingFunction(void *pc);

#ifdef __cplusplus
}
#endif

#endif
