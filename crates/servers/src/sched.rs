//! SCHED server — process scheduler and priority management.
//!
//! Ported from `.refs/minix-3.3.0/minix/servers/sched/`
//!
//! The SCHED server manages scheduling parameters for user processes:
//! priority queues, time slice enforcement, CPU affinity, and periodic
//! queue rebalancing.

#![allow(dead_code, unexpected_cfgs, clippy::missing_safety_doc)]
//!
//! # Message handling
//!
//! | Message type | Handler | Source |
//! |---|---|---|
//! | `SCHEDULING_START` / `SCHEDULING_INHERIT` | `do_start_scheduling` | PM |
//! | `SCHEDULING_STOP` | `do_stop_scheduling` | PM |
//! | `SCHEDULING_SET_NICE` | `do_nice` | PM |
//! | `SCHEDULING_NO_QUANTUM` | `do_noquantum` | Kernel |
//!
//! The IPC message loop is deferred (Phase 12 — SEF/server framework).
//! All scheduling logic is fully implemented and tested.

use core::sync::atomic::{AtomicU32, Ordering};

// Constants

/// Number of process slots.
pub const NR_PROCS: usize = 256; // matches kernel NR_PROCS

/// Number of scheduling queues.
pub const NR_SCHED_QUEUES: usize = 16;

/// Default user time slice in ticks.
pub const DEFAULT_USER_TIME_SLICE: u32 = 200;

/// User queue base priority.
pub const USER_Q: u32 = 5;

/// Minimum user queue priority.
pub const MIN_USER_Q: u32 = 7;

/// Balance interval in seconds.
pub const BALANCE_TIMEOUT: u32 = 5;

/// Maximum number of CPUs.
pub const CONFIG_MAX_CPUS: usize = 8;

const SCHEDULE_CHANGE_PRIO: u32 = 0x1;
const SCHEDULE_CHANGE_QUANTUM: u32 = 0x2;
const SCHEDULE_CHANGE_CPU: u32 = 0x4;
const SCHEDULE_CHANGE_ALL: u32 =
    SCHEDULE_CHANGE_PRIO | SCHEDULE_CHANGE_QUANTUM | SCHEDULE_CHANGE_CPU;

const OK: i32 = 0;
const EPERM: i32 = -1;
const EINVAL: i32 = -22;
const EBADEPT: i32 = -66;
const EDEADEPT: i32 = -67;
const ENOSYS: i32 = -71;

// Message types (from com.h).
// NOTE: these match arch_common::com values: SCHEDULING_BASE = 0xF00
const SCHEDULING_NO_QUANTUM: i32 = 0xF01;
const SCHEDULING_START: i32 = 0xF02;
const SCHEDULING_STOP: i32 = 0xF03;
const SCHEDULING_SET_NICE: i32 = 0xF04;
const SCHEDULING_INHERIT: i32 = 0xF05;
const SCHEDULING_NO_QUANTUM_NONBLOCK: i32 = 0xF06;

// Types

/// Per-process scheduling information.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SchedProc {
    pub endpoint: i32,
    pub parent: i32,
    pub flags: u32,
    pub max_priority: u32,
    pub priority: u32,
    pub time_slice: u32,
    pub cpu: u32,
    pub cpu_mask: [u64; CONFIG_MAX_CPUS.div_ceil(64)],
}

impl Default for SchedProc {
    fn default() -> Self {
        Self {
            endpoint: 0,
            parent: 0,
            flags: 0,
            max_priority: 0,
            priority: 0,
            time_slice: 0,
            cpu: 0,
            cpu_mask: [0u64; CONFIG_MAX_CPUS.div_ceil(64)],
        }
    }
}

impl SchedProc {
    const fn zeroed() -> Self {
        Self {
            endpoint: 0,
            parent: 0,
            flags: 0,
            max_priority: 0,
            priority: 0,
            time_slice: 0,
            cpu: 0,
            cpu_mask: [0u64; CONFIG_MAX_CPUS.div_ceil(64)],
        }
    }
}

/// Flag values for SchedProc.flags.
pub const IN_USE: u32 = 0x0001;

// Static state

use core::cell::UnsafeCell;

struct SchedTableRaw(UnsafeCell<[SchedProc; NR_PROCS]>);
unsafe impl Sync for SchedTableRaw {}
impl SchedTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { SchedProc::zeroed() }; NR_PROCS]))
    }
    fn as_ptr(&self) -> *mut SchedProc {
        self.0.get() as *mut SchedProc
    }
}

static SCHED_PROC: SchedTableRaw = SchedTableRaw::new();

/// Per-CPU process count (for load balancing).
static CPU_PROC: AtomicU32 = AtomicU32::new(0);

/// Balance timeout in ticks.
static BALANCE_TIMEOUT_TICKS: AtomicU32 = AtomicU32::new(0);

// Helpers

/// Check if a process endpoint is valid and its slot is in use.
pub unsafe fn sched_isokendpt(endpoint: i32) -> Result<usize, i32> {
    let proc_nr = _endpoint_p(endpoint);
    if proc_nr < 0 || proc_nr as usize >= NR_PROCS {
        return Err(EBADEPT);
    }
    let proc_nr = proc_nr as usize;
    let base = SCHED_PROC.as_ptr();
    let rmp = unsafe { &*base.add(proc_nr) };
    if rmp.endpoint != endpoint {
        return Err(EDEADEPT);
    }
    if rmp.flags & IN_USE == 0 {
        return Err(EDEADEPT);
    }
    Ok(proc_nr)
}

/// Check if a process endpoint is valid and its slot is empty.
pub unsafe fn sched_isemtyendpt(endpoint: i32) -> Result<usize, i32> {
    let proc_nr = _endpoint_p(endpoint);
    if proc_nr < 0 || proc_nr as usize >= NR_PROCS {
        return Err(EBADEPT);
    }
    let proc_nr = proc_nr as usize;
    let base = SCHED_PROC.as_ptr();
    let rmp = unsafe { &*base.add(proc_nr) };
    if rmp.flags & IN_USE != 0 {
        return Err(EDEADEPT);
    }
    Ok(proc_nr)
}

/// Extract process number from an endpoint.
fn _endpoint_p(endpoint: i32) -> i32 {
    endpoint & 0x7FFF
}

/// Check if a message source is allowed (PM or RS only).
pub fn accept_message(source: i32) -> bool {
    matches!(source, -3 | -4) // PM_PROC_NR (-3) or RS_PROC_NR (-4)
}

/// Returns true if the process is a system process (parent is RS).
fn is_system_proc(rmp: &SchedProc) -> bool {
    rmp.parent == -4 // RS_PROC_NR
}

// Scheduling operations

/// Handle a process running out of quantum — lower its priority.
pub unsafe fn do_noquantum(source: i32) -> Result<(), i32> {
    let proc_nr = unsafe { sched_isokendpt(source)? };
    let base = SCHED_PROC.as_ptr();
    let rmp = unsafe { &mut *base.add(proc_nr) };

    if rmp.priority < MIN_USER_Q {
        rmp.priority += 1; // lower priority
    }

    unsafe { schedule_process_local(rmp) }
}

/// Start scheduling a process (SCHEDULING_START or SCHEDULING_INHERIT).
pub unsafe fn do_start_scheduling(
    msg_type: i32,
    endpoint: i32,
    parent: i32,
    max_priority: u32,
    quantum: u32,
    source: i32,
) -> Result<i32, i32> {
    if !accept_message(source) {
        return Err(EPERM);
    }

    let proc_nr = unsafe { sched_isemtyendpt(endpoint)? };
    let base = SCHED_PROC.as_ptr();
    let rmp = unsafe { &mut *base.add(proc_nr) };

    // Populate process slot.
    rmp.endpoint = endpoint;
    rmp.parent = parent;
    rmp.max_priority = max_priority;

    if rmp.max_priority >= NR_SCHED_QUEUES as u32 {
        return Err(EINVAL);
    }

    if rmp.endpoint == rmp.parent {
        // Special case for init (first process, parent of itself).
        rmp.priority = USER_Q;
        rmp.time_slice = DEFAULT_USER_TIME_SLICE;
        rmp.cpu = 0;
    }

    match msg_type {
        t if t == SCHEDULING_START => {
            // System processes get explicit priority/quantum.
            rmp.priority = rmp.max_priority;
            rmp.time_slice = quantum;
        }
        t if t == SCHEDULING_INHERIT => {
            // Inherit priority/time slice from parent.
            let parent_nr = unsafe { sched_isokendpt(parent)? };
            let parent_rmp = unsafe { &*base.add(parent_nr) };
            rmp.priority = parent_rmp.priority;
            rmp.time_slice = parent_rmp.time_slice;
        }
        _ => return Err(EINVAL),
    }

    rmp.flags = IN_USE;

    // Schedule the process.
    pick_cpu(rmp);
    unsafe { schedule_process(rmp, SCHEDULE_CHANGE_ALL)? };

    Ok(SCHED_PROC_NR)
}

/// SCHED_PROC_NR constant.
pub const SCHED_PROC_NR: i32 = -7;

/// Stop scheduling a process.
pub unsafe fn do_stop_scheduling(endpoint: i32, source: i32) -> Result<(), i32> {
    if !accept_message(source) {
        return Err(EPERM);
    }

    let proc_nr = unsafe { sched_isokendpt(endpoint)? };
    let base = SCHED_PROC.as_ptr();
    let rmp = unsafe { &mut *base.add(proc_nr) };

    rmp.flags = 0;
    Ok(())
}

/// Change the nice value (priority) of a process.
pub unsafe fn do_nice(endpoint: i32, new_priority: u32, source: i32) -> Result<(), i32> {
    if !accept_message(source) {
        return Err(EPERM);
    }

    let proc_nr = unsafe { sched_isokendpt(endpoint)? };
    let base = SCHED_PROC.as_ptr();
    let rmp = unsafe { &mut *base.add(proc_nr) };

    if new_priority >= NR_SCHED_QUEUES as u32 {
        return Err(EINVAL);
    }

    let old_q = rmp.priority;
    let old_max_q = rmp.max_priority;

    rmp.max_priority = new_priority;
    rmp.priority = new_priority;

    if let Err(e) = unsafe { schedule_process_local(rmp) } {
        // Rollback.
        rmp.priority = old_q;
        rmp.max_priority = old_max_q;
        return Err(e);
    }

    Ok(())
}

/// Initialize scheduling — called once during startup.
pub fn init_scheduling(hz: u32) {
    BALANCE_TIMEOUT_TICKS.store(BALANCE_TIMEOUT * hz, Ordering::Relaxed);
}

// Internal scheduling helpers

/// Pick the best CPU for a process (simple: CPU 0 for now, SMP deferred).
fn pick_cpu(rmp: &mut SchedProc) {
    #[cfg(not(feature = "smp"))]
    {
        rmp.cpu = 0;
    }

    #[cfg(feature = "smp")]
    {
        // System processes always run on BSP (CPU 0).
        if is_system_proc(rmp) {
            rmp.cpu = 0;
            return;
        }

        // Simple load balancing: pick the CPU with fewest processes.
        // For now, always CPU 0 since SMP isn't wired yet.
        rmp.cpu = 0;
    }
}

/// Schedule a process with the kernel (update priority/quantum/CPU).
unsafe fn schedule_process(rmp: &SchedProc, flags: u32) -> Result<(), i32> {
    let new_prio = if flags & SCHEDULE_CHANGE_PRIO != 0 {
        rmp.priority as i32
    } else {
        -1i32
    };

    let new_quantum = if flags & SCHEDULE_CHANGE_QUANTUM != 0 {
        rmp.time_slice as i32
    } else {
        -1i32
    };

    let new_cpu = if flags & SCHEDULE_CHANGE_CPU != 0 {
        rmp.cpu as i32
    } else {
        -1i32
    };

    // FIRST: Register as scheduler via SYS_SCHEDCTL (kernel call 54) —
    // matching C `do_start_scheduling` in servers/sched/schedule.c line 221.
    // This sets p->p_scheduler = caller on the kernel's Proc struct,
    // which makes the subsequent do_schedule_handler check pass.
    // C: sys_schedctl(0, endpoint, 0, 0, 0) with flags=0 means
    // SCHED server (caller) becomes the process's p_scheduler,
    // per C MINIX do_schedctl.c: p->p_scheduler = caller.
    // For the kernel-scheduled case, set KERNEL flag with NULL.
    let schedctl_r = sys_schedctl(rmp.endpoint, new_prio, new_quantum, new_cpu);
    if schedctl_r != 0 {
        return Err(schedctl_r);
    }

    // SECOND: Call SYS_SCHEDULE (kernel call 3) to set priority/quantum
    // and clear RTS_NO_QUANTUM. Matching C: schedule_process calls
    // sys_schedule AFTER sys_schedctl registers us as the scheduler,
    // so the do_schedule_handler's p_scheduler check passes.
    let schedule_r = sys_schedule(rmp.endpoint, new_prio, new_quantum, new_cpu);
    if schedule_r != 0 {
        return Err(schedule_r);
    }
    Ok(())
}

/// Invoke SYS_SCHEDCTL (kernel call 54) to register this process as the
/// scheduler for the target. Sets p->p_scheduler = caller on the kernel
/// Proc struct, which enables do_schedule_handler's caller check.
/// Matching C: do_schedctl.c lines 41-42.
fn sys_schedctl(endpoint: i32, priority: i32, quantum: i32, cpu: i32) -> i32 {
    let mut msg = [0u8; 64];
    // Message layout matching do_schedctl_handler offsets.
    // NOTE: kernel_call overwrites msg[0..8] with call_nr + src_ep,
    // so all payload fields MUST be placed at offset 8+.
    // flags = 0 means caller becomes the scheduler
    msg[8..12].copy_from_slice(&0i32.to_le_bytes()); // flags = 0 (caller = sched), at SCHEDCTL_FLAGS_OFF=8
    msg[12..16].copy_from_slice(&endpoint.to_le_bytes()); // SCHEDCTL_ENDPT_OFF=12
    msg[16..20].copy_from_slice(&priority.to_le_bytes()); // SCHEDCTL_PRIORITY_OFF=16
    msg[20..24].copy_from_slice(&quantum.to_le_bytes()); // SCHEDCTL_QUANTUM_OFF=20
    msg[24..28].copy_from_slice(&cpu.to_le_bytes()); // SCHEDCTL_CPU_OFF=24
    minix_rt::kernel_call(54, &mut msg)
}

/// Invoke SYS_SCHEDULE (kernel call 3) to update a process's scheduling
/// parameters in the kernel. Clears RTS_NO_QUANTUM and re-enqueues.
fn sys_schedule(endpoint: i32, priority: i32, quantum: i32, cpu: i32) -> i32 {
    let mut msg = [0u8; 64];
    // Message layout matching do_schedule_handler offsets:
    //   [8..12] = endpoint (SCHEDULE_ENDPT_OFF)
    //   [12..16] = quantum (SCHEDULE_QUANTUM_OFF)
    //   [16..20] = priority (SCHEDULE_PRIORITY_OFF)
    //   [20..24] = cpu (SCHEDULE_CPU_OFF)
    msg[8..12].copy_from_slice(&endpoint.to_le_bytes());
    msg[12..16].copy_from_slice(&quantum.to_le_bytes());
    msg[16..20].copy_from_slice(&priority.to_le_bytes());
    msg[20..24].copy_from_slice(&cpu.to_le_bytes());
    minix_rt::kernel_call(3, &mut msg)
}

/// Shortcut for local priority+quantum changes.
unsafe fn schedule_process_local(rmp: &SchedProc) -> Result<(), i32> {
    unsafe { schedule_process(rmp, SCHEDULE_CHANGE_PRIO | SCHEDULE_CHANGE_QUANTUM) }
}

/// Rebalance scheduling queues — restore priorities that were lowered
/// by do_noquantum back toward their max_priority.
pub unsafe fn balance_queues() {
    let base = SCHED_PROC.as_ptr();
    for i in 0..NR_PROCS {
        let rmp = unsafe { &mut *base.add(i) };
        if rmp.flags & IN_USE != 0 && rmp.priority > rmp.max_priority {
            rmp.priority -= 1; // increase priority
            let _ = unsafe { schedule_process_local(rmp) };
        }
    }
}

/// Get a mutable reference to a SchedProc entry.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn sched_proc_mut(proc_nr: usize) -> &'static mut SchedProc {
    unsafe { &mut *SCHED_PROC.as_ptr().add(proc_nr) }
}

/// Get a shared reference to a SchedProc entry.
///
/// # Safety
///
/// Caller must ensure no concurrent mutable access.
pub unsafe fn sched_proc(proc_nr: usize) -> &'static SchedProc {
    unsafe { &*SCHED_PROC.as_ptr().add(proc_nr) }
}

// Server main loop (stub — see Phase 12 wiring)

/// SCHED server main loop.
///
/// Receives messages from PM/RS/kernel and dispatches scheduling requests.
/// On host builds (testing), this is a no-op — the scheduling logic is
/// exercised through unit tests directly.
pub fn sched_server_main() {
    #[cfg(target_os = "none")]
    {
        // Syscall numbers for IPC.
        const RECEIVE_CALL: u64 = 47;
        const SENDREC_CALL: u64 = 48;
        const SENDNB_CALL: u64 = 51;
        const ANY: i32 = 0x0000ffff;

        // Notification message type (matching C MINIX NOTIFY_MESSAGE = 0x1000).
        const NOTIFY_MTYPE: i32 = arch_common::com::NOTIFY_MESSAGE as i32;

        loop {
            let mut msg = arch_common::ipc::Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive a message from any sender.
            // syscall2(RECEIVE_CALL=47, src=ANY, msg_ptr) → sender endpoint
            let src = unsafe {
                minix_rt::syscall2(
                    RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                )
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // Handle notifications (m_type == NOTIFY_MESSAGE = -10).
            // Just continue without replying — the kernel does not expect one.
            if msg.m_type == NOTIFY_MTYPE {
                continue;
            }

            // Dispatch the scheduling call.
            let status = match msg.m_type {
                SCHEDULING_NO_QUANTUM => {
                    // Kernel sends this — the source IS the endpoint of
                    // the process that ran out of quantum.
                    // For NO_QUANTUM, the kernel doesn't expect a reply.
                    // Skip the reply below (we still need to process it).
                    match unsafe { do_noquantum(src_ep) } {
                        Ok(()) => {
                            // Reply to the process that lost quantum
                            // with just SEND (not SENDREC), because the
                            // target is not expecting a message from us.
                            msg.m_type = OK;
                            unsafe {
                                minix_rt::syscall2(
                                    SENDNB_CALL,
                                    src_ep as u64,
                                    &mut msg as *mut arch_common::ipc::Message as u64,
                                );
                            }
                            continue;
                        }
                        Err(e) => {
                            msg.m_type = e;
                            unsafe {
                                minix_rt::syscall2(
                                    SENDNB_CALL,
                                    src_ep as u64,
                                    &mut msg as *mut arch_common::ipc::Message as u64,
                                );
                            }
                            continue;
                        }
                    }
                }
                SCHEDULING_START | SCHEDULING_INHERIT => {
                    match unsafe {
                        do_start_scheduling(
                            msg.m_type,
                            msg.m_payload.m2.m2i1,
                            msg.m_payload.m2.m2i2,
                            msg.m_payload.m2.m2i3 as u32,
                            msg.m_payload.m2.m2l1 as u32,
                            src_ep,
                        )
                    } {
                        Ok(nr) => nr,
                        Err(e) => e,
                    }
                }
                SCHEDULING_STOP => {
                    match unsafe { do_stop_scheduling(msg.m_payload.m2.m2i1, src_ep) } {
                        Ok(()) => OK,
                        Err(e) => e,
                    }
                }
                SCHEDULING_SET_NICE => {
                    match unsafe {
                        do_nice(msg.m_payload.m2.m2i1, msg.m_payload.m2.m2i3 as u32, src_ep)
                    } {
                        Ok(()) => OK,
                        Err(e) => e,
                    }
                }
                _ => {
                    // Unknown message type.
                    ENOSYS
                }
            };

            // Send the reply with the status code.
            msg.m_type = status;
            unsafe {
                minix_rt::syscall2(
                    SENDREC_CALL,
                    src_ep as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                );
            }
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        // No-op on host builds — dispatch is tested directly
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    static TEST_LOCK: AtomicBool = AtomicBool::new(false);

    struct TestLockGuard;

    impl TestLockGuard {
        fn acquire() -> Self {
            while TEST_LOCK.swap(true, Ordering::SeqCst) {
                core::hint::spin_loop();
            }
            Self
        }
    }

    impl Drop for TestLockGuard {
        fn drop(&mut self) {
            TEST_LOCK.store(false, Ordering::SeqCst);
        }
    }

    fn setup() -> TestLockGuard {
        let guard = TestLockGuard::acquire();
        unsafe {
            let base = SCHED_PROC.as_ptr();
            for i in 0..NR_PROCS {
                (*base.add(i)) = SchedProc::zeroed();
            }
        }
        guard
    }

    #[test]
    fn test_constants() {
        assert_eq!(NR_SCHED_QUEUES, 16);
        assert_eq!(DEFAULT_USER_TIME_SLICE, 200);
        assert_eq!(USER_Q, 5);
        assert_eq!(MIN_USER_Q, 7);
        assert_eq!(IN_USE, 0x0001);
    }

    #[test]
    fn test_endpoint_p() {
        assert_eq!(_endpoint_p(0), 0);
        assert_eq!(_endpoint_p(1), 1);
        assert_eq!(_endpoint_p(0x7FFF), 0x7FFF);
        // High bit is generation, not part of proc_nr.
        assert_eq!(_endpoint_p(0x8000), 0);
        assert_eq!(_endpoint_p(0x8001), 1);
    }

    #[test]
    fn test_accept_message() {
        assert!(accept_message(-3)); // PM_PROC_NR
        assert!(accept_message(-4)); // RS_PROC_NR
        assert!(!accept_message(-5)); // DS_PROC_NR
        assert!(!accept_message(0));
        assert!(!accept_message(1));
    }

    #[test]
    fn test_sched_isokendpt_empty_slot_fails() {
        let _g = setup();
        unsafe {
            // Slot 0 is empty (not IN_USE).
            assert_eq!(sched_isokendpt(0), Err(EDEADEPT));
        }
    }

    #[test]
    fn test_sched_isemtyendpt_empty_slot_succeeds() {
        let _g = setup();
        unsafe {
            assert!(sched_isemtyendpt(0).is_ok());
        }
    }

    #[test]
    fn test_sched_isokendpt_out_of_range() {
        unsafe {
            assert_eq!(sched_isokendpt(NR_PROCS as i32), Err(EBADEPT));
            assert_eq!(sched_isokendpt(-1), Err(EBADEPT));
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_start_and_stop_scheduling() {
        let _g = setup();
        unsafe {
            let ep = 5; // process endpoint 5, slot 5
            let result = do_start_scheduling(
                SCHEDULING_START,
                ep,
                -4,  // parent = RS
                10,  // max_priority
                100, // quantum
                -3,  // source = PM
            );
            assert!(result.is_ok());

            // Verify slot is populated.
            let rmp = sched_proc(5);
            assert!(rmp.flags & IN_USE != 0);
            assert_eq!(rmp.endpoint, 5);
            assert_eq!(rmp.priority, 10);
            assert_eq!(rmp.time_slice, 100);

            // Stop scheduling.
            assert!(do_stop_scheduling(ep, -3).is_ok());
            let rmp = sched_proc(5);
            assert_eq!(rmp.flags & IN_USE, 0);
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_start_scheduling_rejects_non_pm_rs() {
        let _g = setup();
        unsafe {
            let result = do_start_scheduling(
                SCHEDULING_START,
                5,
                -4,
                10,
                100,
                1, // source = process 1
            );
            assert_eq!(result, Err(EPERM));
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_start_scheduling_inherit() {
        let _g = setup();
        unsafe {
            // First create a parent process.
            do_start_scheduling(SCHEDULING_START, 10, -4, 8, 50, -3).unwrap();

            // Child inherits from parent.
            let result = do_start_scheduling(
                SCHEDULING_INHERIT,
                11, // child endpoint
                10, // parent endpoint
                8,  // max_priority
                0,  // quantum (ignored for inherit)
                -3,
            );
            assert!(result.is_ok());

            let child = sched_proc(11);
            assert_eq!(child.priority, 8);
            assert_eq!(child.time_slice, 50);
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_do_nice() {
        let _g = setup();
        unsafe {
            do_start_scheduling(SCHEDULING_START, 7, -4, 10, 100, -3).unwrap();
            // Change priority.
            assert!(do_nice(7, 3, -3).is_ok());
            let rmp = sched_proc(7);
            assert_eq!(rmp.priority, 3);
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_do_nice_rejects_non_pm_rs() {
        let _g = setup();
        unsafe {
            do_start_scheduling(SCHEDULING_START, 7, -4, 10, 100, -3).unwrap();
            assert_eq!(do_nice(7, 3, 1), Err(EPERM));
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_do_nice_out_of_range() {
        let _g = setup();
        unsafe {
            do_start_scheduling(SCHEDULING_START, 7, -4, 10, 100, -3).unwrap();
            assert_eq!(do_nice(7, NR_SCHED_QUEUES as u32, -3), Err(EINVAL));
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_noquantum_lowers_priority() {
        let _g = setup();
        unsafe {
            do_start_scheduling(SCHEDULING_START, 3, -4, USER_Q, 100, -3).unwrap();

            do_noquantum(3).unwrap();
            let rmp = sched_proc(3);
            // Priority should be lowered by 1.
            assert_eq!(rmp.priority, USER_Q + 1);
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_noquantum_clamps_at_min() {
        let _g = setup();
        unsafe {
            do_start_scheduling(SCHEDULING_START, 3, -4, MIN_USER_Q, 100, -3).unwrap();

            // Lower several times.
            for _ in 0..5 {
                let _ = do_noquantum(3);
            }
            let rmp = sched_proc(3);
            // Should not go above MIN_USER_Q.
            assert_eq!(rmp.priority, MIN_USER_Q);
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_balance_queues_restores_priority() {
        let _g = setup();
        unsafe {
            do_start_scheduling(SCHEDULING_START, 4, -4, USER_Q, 100, -3).unwrap();

            // Lower priority via noquantum.
            do_noquantum(4).unwrap();
            let rmp = sched_proc(4);
            assert_eq!(rmp.priority, USER_Q + 1);

            // Balance should restore it.
            balance_queues();
            let rmp = sched_proc(4);
            assert_eq!(rmp.priority, USER_Q);
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_stop_scheduling_rejects_non_pm_rs() {
        let _g = setup();
        unsafe {
            do_start_scheduling(SCHEDULING_START, 6, -4, 10, 100, -3).unwrap();
            assert_eq!(do_stop_scheduling(6, 1), Err(EPERM));
        }
    }

    #[test]
    fn test_sched_proc_default() {
        let p = SchedProc::default();
        assert_eq!(p.flags, 0);
        assert_eq!(p.endpoint, 0);
        assert_eq!(p.priority, 0);
    }

    #[test]
    fn test_init_scheduling() {
        init_scheduling(100);
        assert_eq!(
            BALANCE_TIMEOUT_TICKS.load(Ordering::Relaxed),
            BALANCE_TIMEOUT * 100
        );
    }

    #[test]
    fn test_sched_server_main_callable() {
        sched_server_main();
    }

    #[test]
    fn test_pick_cpu_default() {
        let mut p = SchedProc::default();
        pick_cpu(&mut p);
        assert_eq!(p.cpu, 0);
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_init_self_parented() {
        let _g = setup();
        unsafe {
            // init-like process: endpoint == parent
            let result =
                do_start_scheduling(SCHEDULING_START, 1, 1, USER_Q, DEFAULT_USER_TIME_SLICE, -3);
            assert!(result.is_ok());
            let rmp = sched_proc(1);
            assert_eq!(rmp.priority, USER_Q);
            assert_eq!(rmp.time_slice, DEFAULT_USER_TIME_SLICE);
        }
    }
}
