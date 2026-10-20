// Fake `libdiary_capi.so` for the host harness. It implements the six C ABI
// symbols with the simplest possible semantics and exposes call counters so
// the harness can prove "freed/closed exactly once".
//
// `diary_close` counts every call *before* freeing: a second close of the same
// pointer is exactly the double-close/UAF the harness wants to catch.
#include <cstdlib>
#include <cstring>

namespace {
int g_frees = 0;
int g_closes = 0;
}  // namespace

extern "C" {
char *diary_capi_version() {
  char *p = static_cast<char *>(std::malloc(7));
  std::memcpy(p, "fake-1", 7);
  return p;
}
void *diary_open(const char *) { return std::malloc(1); }
int diary_close(void *p) {
  ++g_closes;
  std::free(p);
  return 0;
}
char *diary_call(void *, const char *) {
  char *p = static_cast<char *>(std::malloc(3));
  std::memcpy(p, "{}", 3);
  return p;
}
void diary_string_free(char *p) {
  ++g_frees;
  std::free(p);
}
char *diary_last_error() { return nullptr; }
int diary_test_free_count() { return g_frees; }
int diary_test_close_count() { return g_closes; }
}
