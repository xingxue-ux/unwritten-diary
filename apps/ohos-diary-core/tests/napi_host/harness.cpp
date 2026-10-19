// Host harness for `entry/src/main/cpp/napi_shim.cpp`.
//
// Why this exists: the shim promises that **any** C++ exception escaping a NAPI
// callback becomes `ERR_DIARY_CALL`, with C resources returned on the way out.
// Device/ArkTS tests cannot inject `std::bad_alloc` at an exact NAPI boundary,
// so this harness compiles the real shim against a tiny NAPI stub
// (`napi/native_api.h` + `stub.cpp`) and a fake core (`fake_capi.cpp`), then
// injects one exception per entry point.
//
// Run it with `./run.sh` (g++ only, no OHOS SDK). It also asserts the handle
// table is clean after `open()` fails between `Handles().Add()` and
// `napi_create_uint32`: a dangling `id -> closed pointer` entry would make
// `close(id)` double-close/UAF and `openHandleCount()` report a fake leak.
#include "../../entry/src/main/cpp/napi_shim.cpp"

#include <cstdio>
#include <cstdlib>
#include <dlfcn.h>
#include <exception>

#include "stub.h"

namespace {
int failures = 0;

void fail(const char *message) {
  std::printf("FAIL %s\n", message);
  ++failures;
}

void report(const char *name, napi_value result) {
  if (result != nullptr) {
    std::printf("FAIL %s: returned non-null without throwing\n", name);
    ++failures;
  } else if (stub::last_code != "ERR_DIARY_CALL") {
    std::printf("FAIL %s: code=%s message=%s\n", name, stub::last_code.c_str(),
                stub::last_message.c_str());
    ++failures;
  } else {
    std::printf("PASS %s: ERR_DIARY_CALL (%s)\n", name, stub::last_message.c_str());
  }
}
}  // namespace

int main() {
  // The unfixed shim aborts in the double-close check below; keep earlier FAIL
  // lines visible even without a flushed exit.
  std::setvbuf(stdout, nullptr, _IONBF, 0);
  napi_env env = nullptr;

  // `LoadCore()` caches process-wide on first callback, so the env var must be
  // set before anything touches `Core()`. `run.sh` exports it.
  const char *fake_path = std::getenv("DIARY_CAPI_LIB");
  if (fake_path == nullptr || *fake_path == '\0') {
    std::printf("FAIL DIARY_CAPI_LIB is not set; run via run.sh\n");
    return 2;
  }
  void *fake_lib = dlopen(fake_path, RTLD_NOW);
  auto fake_free_count = fake_lib == nullptr
                             ? nullptr
                             : reinterpret_cast<int (*)()>(dlsym(fake_lib, "diary_test_free_count"));
  auto fake_close_count = fake_lib == nullptr
                              ? nullptr
                              : reinterpret_cast<int (*)()>(dlsym(fake_lib, "diary_test_close_count"));
  if (fake_lib == nullptr || fake_free_count == nullptr || fake_close_count == nullptr) {
    std::printf("FAIL could not load the fake core at %s\n", fake_path);
    return 2;
  }

  {
    stub::reset(); stub::huge_string_len = true; stub::first_arg_is_number = false;
    napi_value result = nullptr;
    try { result = OpenJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("open: std::bad_alloc escaped the NAPI callback"); }
    report("open", result);
  }
  {
    stub::reset(); stub::huge_string_len = true; stub::first_arg_is_number = true;
    napi_value result = nullptr;
    try { result = CallJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("call: std::bad_alloc escaped the NAPI callback"); }
    report("call", result);
  }
  {
    stub::reset(); stub::throw_create_string = true;
    napi_value result = nullptr;
    try { result = SelfDirJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("selfDir: std::bad_alloc escaped the NAPI callback"); }
    report("selfDir", result);
  }
  {
    stub::reset(); stub::throw_create_uint32 = true;
    napi_value result = nullptr;
    try { result = OpenHandleCountJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("openHandleCount: std::bad_alloc escaped the NAPI callback"); }
    report("openHandleCount", result);
  }
  {
    stub::reset(); stub::throw_get_null = true;
    napi_value result = nullptr;
    try { result = LastErrorJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("lastError: std::bad_alloc escaped the NAPI callback"); }
    report("lastError", result);
  }
  {
    stub::reset(); stub::throw_cb_info = true;
    napi_value result = nullptr;
    try { result = CloseJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("close: std::bad_alloc escaped the NAPI callback"); }
    report("close", result);
  }
  {
    stub::reset(); stub::throw_define_properties = true;
    napi_value result = nullptr;
    try { result = Init(env, nullptr); }
    catch (const std::bad_alloc &) { fail("init: std::bad_alloc escaped the NAPI callback"); }
    report("init", result);
  }
  {
    // version(): load the fake core, then make napi_create_string_utf8 throw.
    // Proves the wrapper covers it and that CoreText frees the C string once.
    stub::reset(); stub::throw_create_string = true;
    napi_value result = nullptr;
    try { result = VersionJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("version: std::bad_alloc escaped the NAPI callback"); }
    report("version", result);
    const int frees = fake_free_count();
    if (frees != 1) {
      std::printf("FAIL version: diary_string_free calls=%d (expect 1)\n", frees);
      ++failures;
    } else {
      std::printf("PASS version: C string freed exactly once on the exception path\n");
    }
  }
  {
    // open(): the C handle is already registered by `Handles().Add(...)` when
    // `napi_create_uint32` throws. Real NAPI usually returns `napi_status`, but
    // the shim promises to catch any C++ exception, and the harness injects one
    // on exactly this line. The table entry must be taken *and* the C handle
    // closed; either leak is a bug.
    stub::reset(); stub::throw_create_uint32 = true; stub::first_arg_is_number = false;
    const int closes_before = fake_close_count();
    napi_value result = nullptr;
    try { result = OpenJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("open(after Add): std::bad_alloc escaped the NAPI callback"); }
    report("open(after Add)", result);
    const int closes_after = fake_close_count();
    if (closes_after != closes_before + 1) {
      std::printf("FAIL open(after Add): diary_close calls=%d (expect %d)\n",
                  closes_after, closes_before + 1);
      ++failures;
    } else {
      std::printf("PASS open(after Add): C handle closed exactly once\n");
    }

    // Ask through the JS entry point (not by reading the private table), so
    // this is the same number ArkTS would see.
    stub::reset();
    (void)OpenHandleCountJs(env, nullptr);
    if (stub::last_uint32 != 0) {
      std::printf("FAIL openHandleCount() after failed open: %u (expect 0; dangling table entry)\n",
                  stub::last_uint32);
      ++failures;
    } else {
      std::printf("PASS openHandleCount()==0 after failed open\n");
    }

    // The id was taken, so closing it again must report ERR_DIARY_HANDLE and
    // must not call diary_close a second time (double-close/UAF).
    stub::reset(); stub::first_arg_is_number = true;
    try { (void)CloseJs(env, nullptr); }
    catch (const std::bad_alloc &) { fail("close(after failed open): exception escaped"); }
    if (stub::last_code != "ERR_DIARY_HANDLE") {
      std::printf("FAIL close(after failed open): code=%s (expect ERR_DIARY_HANDLE)\n",
                  stub::last_code.c_str());
      ++failures;
    } else if (fake_close_count() != closes_after) {
      std::printf("FAIL close(after failed open): double-close (calls=%d, expect %d)\n",
                  fake_close_count(), closes_after);
      ++failures;
    } else {
      std::printf("PASS close(stale id): ERR_DIARY_HANDLE, no double-close\n");
    }
  }

  std::printf(failures == 0 ? "ALL ENTRY POINTS GUARDED · HANDLE TABLE CLEAN\n"
                            : "%d FAILURES\n",
              failures);
  return failures == 0 ? 0 : 1;
}
