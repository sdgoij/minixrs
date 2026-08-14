// C++ smoke test for the minix libc++ port — exercises heap allocation
// (std::vector/string/map through operator new → minix-libc malloc) and
// printf (c-libc). No static constructors (crt0 does not run .init_array
// yet), no exceptions, no iostream.
#include <cstdio>
#include <cstdlib>
#include <map>
#include <string>
#include <vector>

// -ffreestanding mangles `main` (C++ main is only special in hosted
// mode), and crt0 calls it unmangled.
extern "C" int main();

// Static constructor/destructor: crt0 must run .init_array before main,
// and exit() must run the __cxa_atexit-registered destructor.
struct InitTest {
    InitTest() { std::printf("ctor: ran\n"); }
    ~InitTest() { std::printf("dtor: ran\n"); }
};
InitTest g_init;

static void atexit_fn() { std::printf("atexit: ran\n"); }

extern "C" int main() {
    std::vector<int> v;
    for (int i = 0; i < 10; i++) {
        v.push_back(i * i);
    }
    std::string s = "hello from c++";
    std::map<int, const char *> m;
    m[1] = "one";
    m[2] = "two";

    long sum = 0;
    for (int x : v) {
        sum += x;
    }
    std::printf("%s: vector size=%zu sum=%ld map[2]=%s\n", s.c_str(), v.size(),
                sum, m[2]);
    ::atexit(atexit_fn);
    std::printf("cpp: PASS\n");
    return 0;
}
