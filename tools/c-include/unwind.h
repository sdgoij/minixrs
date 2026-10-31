/* Minimal unwind.h for the minix OS. The port loads no unwind tables, so the
 * walk reports the end of the stack; the declarations exist so a caller that
 * includes the header links against the same two symbols. */
#ifndef _UNWIND_H
#define _UNWIND_H

struct _Unwind_Context;
typedef int _Unwind_Reason_Code;
typedef unsigned long _Unwind_Ptr;
typedef _Unwind_Reason_Code (*_Unwind_Trace_Fn)(struct _Unwind_Context *, void *);

#ifdef __cplusplus
extern "C" {
#endif

_Unwind_Reason_Code _Unwind_Backtrace(_Unwind_Trace_Fn cb, void *arg);
_Unwind_Ptr _Unwind_GetIP(struct _Unwind_Context *ctx);

#ifdef __cplusplus
}
#endif

#endif
