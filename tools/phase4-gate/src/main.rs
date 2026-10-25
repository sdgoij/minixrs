//! Phase 4a's gate: calloop's event loop on this port.
//!
//! Four sources, one dispatch loop: a calloop timer (the poll-timeout path), a timerfd, an
//! eventfd and a pipe. Each is a different way into the readiness layer, and the loop only
//! passes when all four have fired in one process — the timer is what smithay's own loop wakes
//! on, the timerfd is a deadline in the port's timer server, and the eventfd and pipe are the
//! wake-ups a `calloop::ping` and a passed fd use.
use minix_libc as _;

use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, Mode, PostAction};
use rustix::time::{Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec};

const TIMER_MS: u64 = 30;
const TIMERFD_MS: u64 = 60;
const DEADLINE: Duration = Duration::from_secs(5);

const SAW_TIMER: u8 = 1 << 0;
const SAW_TIMERFD: u8 = 1 << 1;
const SAW_EVENTFD: u8 = 1 << 2;
const SAW_PIPE: u8 = 1 << 3;
const ALL: u8 = SAW_TIMER | SAW_TIMERFD | SAW_EVENTFD | SAW_PIPE;

struct State {
    /// The eventfd's dupe for the writer; its registered source owns the original.
    event: Option<rustix::fd::OwnedFd>,
    /// The pipe's write end: the timer writes a byte, the registered read end wakes the loop.
    pipe: Option<rustix::fd::OwnedFd>,
    seen: u8,
}

fn main() {
    let mut event_loop: calloop::EventLoop<State> = calloop::EventLoop::try_new().unwrap();
    let handle = event_loop.handle();

    // A timerfd: a deadline that *is* a descriptor, so the readiness layer sees it.
    let timerfd = rustix::time::timerfd_create(TimerfdClockId::Monotonic, TimerfdFlags::NONBLOCK)
        .expect("calloopgate: timerfd_create");
    let when = Itimerspec {
        it_interval: Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: Timespec {
            tv_sec: 0,
            tv_nsec: (TIMERFD_MS * 1_000_000) as _,
        },
    };
    rustix::time::timerfd_settime(&timerfd, TimerfdTimerFlags::empty(), &when)
        .expect("calloopgate: timerfd_settime");

    // An eventfd: a counter the readiness layer reports as readable, woken by an 8-byte write.
    let event = rustix::event::eventfd(0, rustix::event::EventfdFlags::NONBLOCK)
        .expect("calloopgate: eventfd");
    let event_w = rustix::io::dup(&event).expect("calloopgate: dup");

    // A pipe: the wake-up `calloop::ping` is built on for this port (`pipe2(O_NONBLOCK)`).
    let (pipe_r, pipe_w) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::NONBLOCK).expect("calloopgate: pipe2");

    handle
        .insert_source(
            Generic::new(timerfd, Interest::READ, Mode::Level),
            |_, fd, state| {
                let mut buf = [0u8; 8];
                let read = matches!(rustix::io::read(&**fd, &mut buf), Ok(8));
                println!("calloopgate: timerfd read={read}");
                state.seen |= SAW_TIMERFD;
                Ok(PostAction::Remove)
            },
        )
        .expect("calloopgate: register the timerfd");

    handle
        .insert_source(
            Generic::new(event, Interest::READ, Mode::Level),
            |_, fd, state| {
                let mut buf = [0u8; 8];
                let read = matches!(rustix::io::read(&**fd, &mut buf), Ok(8));
                println!("calloopgate: eventfd read={read}");
                state.seen |= SAW_EVENTFD;
                Ok(PostAction::Remove)
            },
        )
        .expect("calloopgate: register the eventfd");

    handle
        .insert_source(
            Generic::new(pipe_r, Interest::READ, Mode::Level),
            |_, fd, state| {
                let mut buf = [0u8; 1];
                let read = matches!(rustix::io::read(&**fd, &mut buf), Ok(1));
                println!("calloopgate: pipe read={read} byte={:?}", buf[0] as char);
                state.seen |= SAW_PIPE;
                Ok(PostAction::Remove)
            },
        )
        .expect("calloopgate: register the pipe");

    let mut state = State {
        event: Some(event_w),
        pipe: Some(pipe_w),
        seen: 0,
    };

    // The calloop timer: a deadline the loop holds itself, which is what smithay schedules on.
    handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(TIMER_MS)),
            |_, _, state| {
                println!("calloopgate: timer");
                state.seen |= SAW_TIMER;
                if let Some(fd) = state.event.as_ref() {
                    let _ = rustix::io::write(fd, &1u64.to_ne_bytes());
                }
                if let Some(fd) = state.pipe.as_ref() {
                    let _ = rustix::io::write(fd, b"x");
                }
                TimeoutAction::Drop
            },
        )
        .expect("calloopgate: register the timer");

    // Four events, in whatever order the readiness layer reports them, then out.
    let deadline = Instant::now() + DEADLINE;
    while state.seen != ALL && Instant::now() < deadline {
        event_loop
            .dispatch(Duration::from_millis(250), &mut state)
            .expect("calloopgate: dispatch");
    }

    let seen = state.seen;
    if seen == ALL {
        println!("calloopgate: PASS timer, timerfd, eventfd and pipe all dispatched");
    } else {
        println!(
            "calloopgate: FAIL timer={} timerfd={} eventfd={} pipe={}",
            seen & SAW_TIMER != 0,
            seen & SAW_TIMERFD != 0,
            seen & SAW_EVENTFD != 0,
            seen & SAW_PIPE != 0,
        );
    }

    drop(state);
    drop(event_loop);
    std::process::exit(if seen == ALL { 0 } else { 1 });
}
