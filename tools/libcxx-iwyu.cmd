@echo off
rem The standalone libcxx cmake never sets Python3_EXECUTABLE, and the only
rem python on this host is the embeddable CPython beside LLVM: its
rem python311._pth pins sys.path to the stdlib zip + the bin dir (no cwd,
rem site, or PYTHONPATH), so libcxx's local modules are not importable and
rem the IWYU mapping script cannot run. The mapping is only consumed by
rem include-what-you-use, which the freestanding Minix C++ build does not
rem run; emit a stub so the generate-cxx-headers step completes.
rem Arguments: script -o output.
echo [] > "%3"
exit /b 0
