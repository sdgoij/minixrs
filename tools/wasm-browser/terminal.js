// The console's renderer: bytes in, lines out.
//
// A line accumulator would be wrong. The shell's line editor moves the cursor with `\r` and `\b`
// and redraws *over* what it wrote — it blanks a row with 79 spaces and rewrites the prompt on
// top — so appending every byte would show the redraws as repetitions and the transcript would
// read as garbage. This is the smallest model that gets that right: one line, a cursor, and the
// three control characters the editor uses.
//
// What it is not: a terminal emulator. No scrolling region, no wrapping, no ANSI escapes, no
// alternate screen. Nothing in this port emits any of them — the shell draws with CR, BS and
// spaces only — and a real one is M5's `wserver`/canvas work rather than something to grow here.
//
// Both front ends use it, which is the point: `run.js` asserts against what this produces, so
// the page's renderer is the tested one.

/// How many finished lines to keep by default. A guest that never stops writing must not be able
/// to grow the host's memory without bound, and a page only shows the last screenful anyway.
const DEFAULT_MAX_LINES = 2000;

export function createTerminal({ maxLines = DEFAULT_MAX_LINES } = {}) {
  let lines = [];
  let line = [];
  let col = 0;

  function flush() {
    // Trailing spaces are how the editor blanks a row; they are not content.
    lines.push(line.join('').replace(/\s+$/, ''));
    if (lines.length > maxLines) lines = lines.slice(lines.length - maxLines);
    line = [];
    col = 0;
  }

  return {
    write(byte) {
      const ch = String.fromCharCode(byte & 0xff);
      switch (ch) {
        case '\n':
          flush();
          return;
        case '\r':
          col = 0;
          return;
        case '\b':
          if (col > 0) col -= 1;
          return;
        case '\x07': // bell
        case '\x1b': // escape: nothing in this port draws with one
          return;
        default:
          break;
      }
      if (col < line.length) line[col] = ch;
      else line.push(ch);
      col += 1;
    },

    /// The finished lines.
    get lines() {
      return lines;
    },

    /// The line the cursor is on, which for a shell at its prompt is the prompt.
    get partial() {
      return line.join('');
    },

    /// Everything, unfinished line included. A check that looks for a prompt needs this: a prompt
    /// is written and then waited at, so it is exactly the text no newline has ended.
    text() {
      return [...lines, line.join('')].join('\n');
    },
  };
}
