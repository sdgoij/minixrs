# The tty's window size

`TIOCGWINSZ` is how a full-screen program learns how big its terminal is, and
`more` (and `ls`'s column layout, and `vi`) take their page length from it. This
note records what each terminal line answers and why, because one answer is a
deliberate deviation from the C reference.

## What each line reports

| Line | `ws_row` / `ws_col` | Who sets it |
|---|---|---|
| Console (`/dev/console`, minor 0-3) | 24 / 80 | The port (`tty::CONSOLE_ROWS`/`CONSOLE_COLS`) |
| RS-232 (minor 16-19) | 0 / 0 | Nobody; `rs_init` sets no size |
| PTY slave (minor 128-131) | 0 / 0 | The master, i.e. the terminal emulator (`wterm`) |

A pty's size is the emulator's: `wterm` opens `/dev/ptyp0` and sends `TIOCSWINSZ`
with its own grid before it forks the shell (`crates/userland/src/bin/wterm.rs`),
and the master's request is copied onto the slave line
(`test_master_tiocswinsz_forwards_to_slave`). Until a master sets one, a pty
slave answers zero, which is the reference's own default.

A console line answers the geometry the console model composes
(`crates/servers/src/console.rs`'s `COLS`/`ROWS`, which now come from the tty's
constants, so the screen the front end draws and the size a program is told
cannot drift apart).

## The deviation

MINIX 3.3.0 keeps `winsize_defaults` zeroed and `tty_init` assigns it to every
line (`minix/drivers/tty/tty/tty.c:126,773`), so a console there answers `0 0`
and a full-screen program has to fall back to a compiled-in guess (NetBSD's
`more` treats `ws_row == 0` as "assume 24"). This port answers 24x80 instead.

Why:

- The port *knows* the console's geometry. It composes the console's cells
  itself (`console.rs`), and its shell's line editor already blanks a row with
  `COLS - 1` spaces because it assumes those 80 columns. A zero answer would be a
  program guessing at a number the machine has.
- The alternative, teaching each applet to guess, is a per-applet rule with no
  owner. `more` is the applet that made this concrete: as shipped it takes its
  page length straight from `terminal::size()`, so a zero answer is one line per
  page (`KNOWN_ISSUES.md` item 77).

What the answer is not: the serial host terminal's real size. QEMU's stdio
chardev does not tell the guest, so the console's 24x80 is the cell grid the
port defines (and the one the wserver window shows), not the size of whatever
window is displaying the serial stream. A host terminal that is 200x50 shows a
pager paging 23 lines, which is what a terminal that has not been resized
reports anyway.

`stty size` prints `24 80` on the console, and the coreutils scenario pins that
(`tools/smoke/coreutils-minix.tsv`).

## Not covered

- No front end tells the *console* line its size. The wserver console is a fixed
  grid, so there is nothing to tell it; a serial host's size would need a report
  the transport does not have.
- A pty slave that no master has sized answers zero, and `more` on such a tty
  pages one line at a time. `wterm` always sizes its pty, so this is the "some
  other emulator" case.
