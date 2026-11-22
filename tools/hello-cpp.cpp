// C++ smoke test for the minix libc++ port: heap allocation (std::vector,
// std::string and std::map through operator new), a static constructor and an
// `atexit` destructor, and printf. It ships as /usr/src/hello.cpp in the staged
// toolchain root and is compiled by the guest's own `clang++` in
// `just test-toolchain-x86`.
//
// `extern "C" int main` keeps the C name, which is what crt0 calls. A hosted
// compile (the toolchain wrapper) warns about it; a `-ffreestanding` one needs
// it, and this file is also used that way. No exceptions and no iostream, which
// is how the staged libc++ runtime is built.
#include <cstdio>
#include <cstdlib>
#include <map>
#include <string>
#include <vector>

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
