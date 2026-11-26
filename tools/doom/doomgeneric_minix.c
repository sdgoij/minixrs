/*
 * The doomgeneric platform layer for minix: the framebuffer is the screen
 * and the console is the keyboard.
 *
 * Doom renders 320x200 into DG_ScreenBuffer as XRGB8888 bytes (B, G, R, X),
 * which is the byte order the fb server's 1024x768 mode already uses, so a
 * frame is an integer nearest-neighbour scale into a centred window and a
 * row-wise copy into /dev/fb. The bars around the window are cleared once,
 * here, because every frame covers the same 960x600.
 *
 * The keyboard a window drives is `/dev/kbd`, the input server's ring
 * (major 20). Its events carry both the press and the release and a HID
 * usage code, so they need neither a hold timer nor the escape-sequence
 * parsing; a program can read the ring alongside the compositor, because the
 * server keeps one cursor per reader. When the ring opens it is the only
 * source, so the terminal that launched the game stays the shell's and a
 * keystroke typed at it does not reach Doom.
 *
 * The console is the fallback when `/dev/kbd` cannot be opened, and
 * `-minix-console` adds it back for a headless debug session. In that case it
 * is put in raw mode so an Esc reaches Doom instead of the shell; a terminal
 * reports presses, not releases, so a key is held for a short interval after
 * its last repeat byte and then released, and arrow keys are the `ESC [ A`..`D`
 * sequences the console sends.
 *
 * DG_DrawFrame reads one pixel back out of the mapping and compares it with
 * the source it just wrote. The comparison is the proof that the blit
 * reached the live framebuffer rather than a private copy, and the first
 * frame that is both varied and verified prints `doom: PASS`, which is the
 * gate's marker.
 *
 * doomgeneric is built without ORIGCODE, so the IWAD search is the working
 * directory (`FILES_DIR` is "."); DG_Init enters the data directory so a
 * plain `/bin/doom` finds the shareware WAD with no arguments.
 */

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/types.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#include "doomgeneric.h"
#include "doomkeys.h"
#include "i_system.h"
#include "m_argv.h"

/* One event of `/dev/kbd`, as `crates/servers/src/input.rs` emits it: HID usage
 * page, usage code, and 1 for a press, 0 for a release, little-endian. */
struct input_event {
    uint16_t page;
    uint16_t code;
    int32_t press;
};

#define INPUT_PAGE_KEY 0x0007
/* The server copies at most 48 bytes per read (six events). */
#define INPUT_EVENTS_MAX 6

/* From `drivers/video/fb.rs`, the fb server's ioctl tags. */
#define FBIOGET_VSCREENINFO 0x4600
#define FBIOFLUSH 0x4604

/* The image installs `doom1.wad` here, and the engine searches its working
 * directory for an IWAD, so DG_Init makes this the working directory. */
#define DATA_DIR "/usr/share/games/doom"

struct fb_bitfield {
    uint32_t offset;
    uint32_t length;
    uint32_t msb_right;
};

struct fb_var_screeninfo {
    uint32_t xres;
    uint32_t yres;
    uint32_t xres_virtual;
    uint32_t yres_virtual;
    uint32_t xoffset;
    uint32_t yoffset;
    uint32_t bits_per_pixel;
    struct fb_bitfield red;
    struct fb_bitfield green;
    struct fb_bitfield blue;
    struct fb_bitfield transp;
    uint16_t reserved[10];
};

static int fb_fd = -1;
static uint8_t *fb_base;
static size_t fb_pitch;
static int fb_width;
static int fb_height;

static int kbd_fd = -1;
static int device_key_seen;

static int scale = 1;
static int win_x;
static int win_y;
static int win_w;
static int win_h;

static unsigned long frames;
static int reported;
static int exit_after_pass;

/* Keyboard. */
#define KEY_QUEUE 32
#define RAW_QUEUE 64
#define HOLD_MS 250

struct key_event {
    unsigned char key;
    unsigned char pressed;
};

static struct key_event key_queue[KEY_QUEUE];
static int key_head;
static int key_count;

static unsigned char raw_queue[RAW_QUEUE];
static int raw_head;
static int raw_count;

static int console_raw;
static struct termios saved_tio;

static unsigned char held[256];
static uint32_t held_seen[256];

static uint32_t now_ms(void);

static void console_restore(void) {
    if (console_raw) {
        tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved_tio);
    }
}

/*
 * The engine's `I_Quit` walks its exit hooks and then, built without ORIGCODE,
 * returns to the game loop rather than leaving the process. `D_Endoom` is one of
 * the hooks and calls `exit(0)` itself, so the menu's Quit Game does leave; a
 * hook of ours only runs when `D_Endoom` declines (`-noendoom`, testcontrols,
 * the screensaver), so it is the fallback that keeps a quit a quit.
 *
 * The verdict is printed from `atexit`, which runs on both paths.
 */
static void platform_exit(void) {
    printf("doom: exit\n");
    fflush(stdout);
}

static void platform_quit(void) {
    exit(0);
}

static void console_raw_mode(void) {
    struct termios t;

    if (!isatty(STDIN_FILENO)) {
        return;
    }
    if (tcgetattr(STDIN_FILENO, &saved_tio) != 0) {
        return;
    }
    t = saved_tio;
    cfmakeraw(&t);
    /* A poll, not a wait: DG_GetKey must never block the game loop. */
    t.c_cc[VMIN] = 0;
    t.c_cc[VTIME] = 0;
    if (tcsetattr(STDIN_FILENO, TCSANOW, &t) == 0) {
        console_raw = 1;
        atexit(console_restore);
    }
}

void DG_Init(void) {
    struct fb_var_screeninfo var;

    /* A missing directory is not fatal: `-iwad` can still name a file. */
    (void)chdir(DATA_DIR);

    /* The gate needs the shell prompt back after the verdict, so it asks the
     * layer to leave; a player does not pass the flag. */
    exit_after_pass = M_CheckParm("-minix-exit") != 0;

    fb_width = 1024;
    fb_height = 768;
    fb_fd = open("/dev/fb", O_RDWR);
    if (fb_fd >= 0) {
        memset(&var, 0, sizeof var);
        if (ioctl(fb_fd, FBIOGET_VSCREENINFO, &var) == 0 && var.xres && var.yres) {
            fb_width = (int)var.xres;
            fb_height = (int)var.yres;
        }
    } else {
        fprintf(stderr, "doom: open /dev/fb: %s (running headless)\n", strerror(errno));
    }

    fb_pitch = (size_t)fb_width * 4;
    if (fb_fd >= 0) {
        void *base = mmap(NULL, fb_pitch * (size_t)fb_height, PROT_READ | PROT_WRITE,
                          MAP_SHARED, fb_fd, 0);
        if (base == MAP_FAILED) {
            fprintf(stderr, "doom: mmap /dev/fb: %s (running headless)\n", strerror(errno));
            base = NULL;
        }
        fb_base = base;
    }

    scale = fb_width / DOOMGENERIC_RESX;
    if (fb_height / DOOMGENERIC_RESY < scale) {
        scale = fb_height / DOOMGENERIC_RESY;
    }
    if (scale < 1) {
        scale = 1;
    }
    if (scale > 8) {
        scale = 8;
    }
    win_w = DOOMGENERIC_RESX * scale;
    win_h = DOOMGENERIC_RESY * scale;
    win_x = (fb_width - win_w) / 2;
    win_y = (fb_height - win_h) / 2;

    if (fb_base != NULL) {
        int y;
        for (y = 0; y < fb_height; y++) {
            memset(fb_base + (size_t)y * fb_pitch, 0, fb_pitch);
        }
        ioctl(fb_fd, FBIOFLUSH, NULL);
    }

    /*
     * The buffer is malloc'd, not zeroed, and the renderer only fills
     * 320x200 of it; a frame that carries a border has to start black.
     */
    memset(DG_ScreenBuffer, 0, (size_t)DOOMGENERIC_RESX * DOOMGENERIC_RESY * 4);

    /*
     * The window's keyboard is `/dev/kbd`; the console is the fallback for an
     * image without the input server, and `-minix-console` brings it back for
     * a debug session with no window. When the ring opens, the terminal is
     * left in canonical mode and never read, so it stays the shell's.
     */
    kbd_fd = open("/dev/kbd", O_RDONLY);
    if (kbd_fd < 0) {
        fprintf(stderr, "doom: open /dev/kbd: %s (console only)\n", strerror(errno));
    }
    if (kbd_fd < 0 || M_CheckParm("-minix-console") != 0) {
        console_raw_mode();
    }
    /* After console_restore, so on the atexit walk the verdict is printed while
     * the console is still raw and the restore runs last. */
    atexit(platform_exit);
    I_AtExit(platform_quit, false);

    printf("doom: fb %dx%d xrgb8888 pitch %lu, %dx%d x%d at (%d,%d)\n",
           fb_width, fb_height, (unsigned long)fb_pitch, win_w, win_h, scale, win_x, win_y);
    fflush(stdout);
}

/* How many distinct pixels the 320x200 frame carries, up to `limit`. */
static int distinct_pixels(int limit) {
    const uint32_t *src = (const uint32_t *)DG_ScreenBuffer;
    uint32_t seen[16];
    int n = 0;
    int i;

    for (i = 0; i < DOOMGENERIC_RESX * DOOMGENERIC_RESY && n < limit; i++) {
        int j;
        int found = 0;
        for (j = 0; j < n; j++) {
            if (seen[j] == src[i]) {
                found = 1;
                break;
            }
        }
        if (!found) {
            seen[n++] = src[i];
        }
    }
    return n;
}

void DG_DrawFrame(void) {
    static uint32_t row[DOOMGENERIC_RESX * 8];
    const uint32_t *src = (const uint32_t *)DG_ScreenBuffer;
    uint32_t *dst = (uint32_t *)fb_base;
    size_t dest_pitch = fb_pitch / 4;
    int y;

    if (fb_base == NULL) {
        return;
    }

    for (y = 0; y < DOOMGENERIC_RESY; y++) {
        const uint32_t *in = src + y * DOOMGENERIC_RESX;
        int x;
        int k;

        for (x = 0; x < DOOMGENERIC_RESX; x++) {
            uint32_t pixel = in[x];
            for (k = 0; k < scale; k++) {
                row[x * scale + k] = pixel;
            }
        }
        for (k = 0; k < scale; k++) {
            uint32_t *out = dst + (size_t)(win_y + y * scale + k) * dest_pitch + win_x;
            memcpy(out, row, (size_t)win_w * 4);
        }
    }
    ioctl(fb_fd, FBIOFLUSH, NULL);

    frames++;

    if (!reported && frames >= 2) {
        uint32_t expect = src[(DOOMGENERIC_RESY / 2) * DOOMGENERIC_RESX + DOOMGENERIC_RESX / 2];
        uint32_t got = dst[(size_t)(win_y + win_h / 2) * dest_pitch + win_x + win_w / 2];
        int colors = distinct_pixels(8);

        if (got != expect) {
            printf("doom: FAIL read-back 0x%08lx, wrote 0x%08lx\n",
                   (unsigned long)got, (unsigned long)expect);
            fflush(stdout);
            exit(1);
        }
        if (colors >= 8) {
            reported = 1;
            printf("doom: frame %lu, %d+ colours, %dx%d x%d centred in %dx%d\n",
                   frames, colors, DOOMGENERIC_RESX, DOOMGENERIC_RESY, scale,
                   fb_width, fb_height);
            /* The gate's expectation is the whole line, so the verdict is its own. */
            printf("doom: PASS\n");
            fflush(stdout);
            if (exit_after_pass) {
                exit(0);
            }
        }
    }

    if (!reported && frames % 35 == 0) {
        printf("doom: frame %lu\n", frames);
        fflush(stdout);
    }
}

void DG_SleepMs(uint32_t ms) {
    usleep(ms * 1000u);
}

uint32_t DG_GetTicksMs(void) {
    struct timespec ts;

    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint32_t)((uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u);
}

static uint32_t now_ms(void) {
    return DG_GetTicksMs();
}

static void queue_event(unsigned char key, int pressed) {
    if (key_count >= KEY_QUEUE) {
        return;
    }
    key_queue[(key_head + key_count) % KEY_QUEUE].key = key;
    key_queue[(key_head + key_count) % KEY_QUEUE].pressed = (unsigned char)pressed;
    key_count++;
}

static int dequeue_event(unsigned char *key, int *pressed) {
    if (key_count == 0) {
        return 0;
    }
    *key = key_queue[key_head].key;
    *pressed = key_queue[key_head].pressed;
    key_head = (key_head + 1) % KEY_QUEUE;
    key_count--;
    return 1;
}

/* A HID keyboard usage to Doom's key code, or -1 for a key Doom does not use. */
static int usage_to_doom(int code) {
    static const unsigned char fkeys[12] = {
        KEY_F1, KEY_F2, KEY_F3, KEY_F4, KEY_F5, KEY_F6,
        KEY_F7, KEY_F8, KEY_F9, KEY_F10, KEY_F11, KEY_F12,
    };

    if (code >= 0x04 && code <= 0x1d) {
        return 'a' + (code - 0x04);
    }
    if (code >= 0x1e && code <= 0x26) {
        return '1' + (code - 0x1e);
    }
    if (code >= 0x3a && code <= 0x45) {
        return fkeys[code - 0x3a];
    }
    switch (code) {
    case 0x27:
        return '0';
    case 0x28:
    case 0x58: /* keypad enter */
        return KEY_ENTER;
    case 0x29:
        return KEY_ESCAPE;
    case 0x2a:
        return KEY_BACKSPACE;
    case 0x2b:
        return KEY_TAB;
    case 0x2c:
        return KEY_USE; /* space */
    case 0x2d:
        return KEY_MINUS;
    case 0x2e:
        return KEY_EQUALS;
    case 0x2f:
        return '[';
    case 0x30:
        return ']';
    case 0x31:
        return '\\';
    case 0x33:
        return ';';
    case 0x34:
        return '\'';
    case 0x35:
        return '`';
    case 0x36:
        return ',';
    case 0x37:
        return '.';
    case 0x38:
        return '/';
    case 0x49:
        return KEY_INS;
    case 0x4a:
        return KEY_HOME;
    case 0x4b:
        return KEY_PGUP;
    case 0x4c:
        return KEY_DEL;
    case 0x4d:
        return KEY_END;
    case 0x4e:
        return KEY_PGDN;
    case 0x4f:
        return KEY_RIGHTARROW;
    case 0x50:
        return KEY_LEFTARROW;
    case 0x51:
        return KEY_DOWNARROW;
    case 0x52:
        return KEY_UPARROW;
    case 0xe0: /* left control */
    case 0xe4: /* right control */
        return KEY_FIRE;
    case 0xe1: /* left shift */
    case 0xe5: /* right shift */
        return KEY_RSHIFT;
    case 0xe2: /* left alt */
    case 0xe6: /* right alt */
        return KEY_LALT;
    default:
        return -1;
    }
}

/* Queue what the keyboard device has, if anything. */
static void fill_kbd(void) {
    struct input_event events[INPUT_EVENTS_MAX];
    ssize_t n;
    ssize_t i;

    if (kbd_fd < 0) {
        return;
    }
    /* Non-blocking: an empty ring comes back as an error, not as a wait. */
    n = read(kbd_fd, events, sizeof events);
    for (i = 0; i + (ssize_t)sizeof events[0] <= n; i++) {
        int key;

        if (events[i].page != INPUT_PAGE_KEY) {
            continue;
        }
        key = usage_to_doom(events[i].code);
        if (key < 0) {
            continue;
        }
        queue_event((unsigned char)key, events[i].press != 0);
        if (!device_key_seen) {
            device_key_seen = 1;
            printf("doom: kbd PASS\n");
            fflush(stdout);
        }
    }
}

static int read_byte(void) {
    if (raw_count == 0) {
        return -1;
    }
    {
        int c = raw_queue[raw_head];
        raw_head = (raw_head + 1) % RAW_QUEUE;
        raw_count--;
        return c;
    }
}

static int peek_byte(void) {
    if (raw_count == 0) {
        return -1;
    }
    return raw_queue[raw_head];
}

static void fill_raw(void) {
    unsigned char buf[32];
    ssize_t n;
    ssize_t i;

    if (!console_raw || RAW_QUEUE - raw_count < (int)sizeof buf) {
        return;
    }
    n = read(STDIN_FILENO, buf, sizeof buf);
    if (n <= 0) {
        return;
    }
    for (i = 0; i < n; i++) {
        raw_queue[(raw_head + raw_count) % RAW_QUEUE] = buf[i];
        raw_count++;
    }
}

static int to_doom_key(int c) {
    switch (c) {
    case 0x03:
        return KEY_FIRE; /* Ctrl-C, safe: raw mode clears ISIG */
    case 27:
        return KEY_ESCAPE;
    case '\r':
    case '\n':
        return KEY_ENTER;
    case '\t':
        return KEY_TAB;
    case 0x7f:
    case 0x08:
        return KEY_BACKSPACE;
    case ' ':
        return KEY_USE;
    default:
        /* Every upstream backend lowercases printable keys (`tolower` in
         * doomgeneric_sdl.c), and Doom compares letters literally: the quit
         * prompt wants 'y' and the cheat sequences are lowercase. */
        if (c >= 'a' && c <= 'z') {
            return c;
        }
        if (c >= 'A' && c <= 'Z') {
            return c - 'A' + 'a';
        }
        if (c > 0x20 && c < 0x7f) {
            return c;
        }
        return -1;
    }
}

/* One decoded key from the raw bytes, or -1 when none is complete yet. */
static int decode_key(void) {
    int c = read_byte();

    if (c < 0) {
        return -1;
    }
    if (c == 27 && peek_byte() == '[') {
        int second;
        int third;

        (void)read_byte();
        second = read_byte();
        if (second < 0) {
            return KEY_ESCAPE;
        }
        if (second >= '0' && second <= '9') {
            /* `ESC [ 1 ; 2 A` and friends: drop modifiers, keep the final. */
            while (peek_byte() >= '0' && peek_byte() <= '9') {
                (void)read_byte();
            }
            if (peek_byte() == ';') {
                (void)read_byte();
            }
            while (peek_byte() >= '0' && peek_byte() <= '9') {
                (void)read_byte();
            }
            third = read_byte();
        } else {
            third = second;
        }
        switch (third) {
        case 'A':
            return KEY_UPARROW;
        case 'B':
            return KEY_DOWNARROW;
        case 'C':
            return KEY_RIGHTARROW;
        case 'D':
            return KEY_LEFTARROW;
        case 'H':
            return KEY_HOME;
        case 'F':
            return KEY_END;
        default:
            return -1;
        }
    }
    return to_doom_key(c);
}

int DG_GetKey(int *pressed, unsigned char *key) {
    uint32_t now;
    int i;

    fill_kbd();
    fill_raw();
    now = now_ms();

    for (i = 0; i < 256; i++) {
        if (held[i] && now - held_seen[i] > HOLD_MS) {
            held[i] = 0;
            queue_event((unsigned char)i, 0);
        }
    }

    if (key_count == 0) {
        int doom_key = decode_key();
        if (doom_key >= 0) {
            if (!held[doom_key]) {
                held[doom_key] = 1;
                queue_event((unsigned char)doom_key, 1);
            }
            held_seen[doom_key] = now;
        }
    }

    return dequeue_event(key, pressed);
}

void DG_SetWindowTitle(const char *title) {
    (void)title;
}

/* Every doomgeneric platform file owns the process entry point. */
int main(int argc, char **argv) {
    doomgeneric_Create(argc, argv);

    for (;;) {
        doomgeneric_Tick();
    }

    return 0;
}
