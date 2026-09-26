/* A shared object nothing names at link time.
 *
 * The program that loads it (`tools/dynopen.c`) does not have it in `DT_NEEDED`, and that is
 * the point: it asks the loader for the file by path at run time, through `dlopen`, and
 * reaches these functions through `dlsym`. What a driver lookup does cannot be expressed as
 * a build-time dependency (`DYNAMIC_LINKING.md` §7 Phase 6, §6.9).
 *
 * `dynopen_text` returns a pointer into *this* object's read-only data and `dynopen_marker`
 * a number from its code. Neither string is in the program, and the gate fails if it is, so
 * a printed line holding them is evidence that this file was opened, mapped, relocated and
 * its symbols callable.
 *
 * Freestanding — see `libdyn.c`.
 */

const char *dynopen_text(void) {
    return "dynopen-text";
}

int dynopen_marker(void) {
    return 3;
}
