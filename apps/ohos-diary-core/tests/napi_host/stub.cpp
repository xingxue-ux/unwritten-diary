// Host implementation of the NAPI subset declared in `napi/native_api.h`.
// Success paths return fake JS values; `stub::throw_*` inject a C++ exception
// at exactly one boundary, which is what the harness asserts against.
#include <napi/native_api.h>

#include <cstring>
#include <new>
#include <string>

#include "stub.h"

namespace stub {

bool huge_string_len = false;
bool throw_create_string = false;
bool throw_create_uint32 = false;
bool throw_get_null = false;
bool throw_cb_info = false;
bool throw_define_properties = false;
bool first_arg_is_number = false;
uint32_t last_uint32 = 0;
std::string last_code;
std::string last_message;

napi_value number_value() { return reinterpret_cast<napi_value>(1); }
napi_value string_value() { return reinterpret_cast<napi_value>(2); }

void reset() {
  huge_string_len = throw_create_string = throw_create_uint32 = false;
  throw_get_null = throw_cb_info = throw_define_properties = false;
  first_arg_is_number = false;
  last_uint32 = 0;
  last_code.clear();
  last_message.clear();
}

}  // namespace stub
extern "C" {

napi_status napi_throw_error(napi_env, const char *code, const char *msg) {
  stub::last_code = code == nullptr ? "" : code;
  stub::last_message = msg == nullptr ? "" : msg;
  return napi_ok;
}

napi_status napi_typeof(napi_env, napi_value value, napi_valuetype *result) {
  if (value == stub::number_value()) { *result = napi_number; return napi_ok; }
  if (value == stub::string_value()) { *result = napi_string; return napi_ok; }
  *result = napi_undefined;
  return napi_ok;
}

napi_status napi_get_value_string_utf8(napi_env, napi_value, char *buf, size_t bufsize, size_t *result) {
  if (stub::huge_string_len) {
    if (buf == nullptr) { *result = static_cast<size_t>(1) << 40; return napi_ok; }
    return napi_ok;
  }
  const char *text = "{\"method\":\"probe\"}";
  if (buf == nullptr) { *result = std::strlen(text); return napi_ok; }
  if (bufsize > 0) { std::strncpy(buf, text, bufsize - 1); buf[bufsize - 1] = '\0'; }
  *result = std::strlen(text);
  return napi_ok;
}

napi_status napi_get_value_int64(napi_env, napi_value, int64_t *result) { *result = 1; return napi_ok; }

napi_status napi_create_string_utf8(napi_env, const char *, size_t, napi_value *result) {
  if (stub::throw_create_string) throw std::bad_alloc();
  *result = stub::string_value();
  return napi_ok;
}

napi_status napi_get_cb_info(napi_env, napi_callback_info, size_t *argc, napi_value *argv, napi_value *, void **) {
  if (stub::throw_cb_info) throw std::bad_alloc();
  if (argc != nullptr && argv != nullptr) {
    for (size_t i = 0; i < *argc; ++i) {
      if (i == 0) argv[i] = stub::first_arg_is_number ? stub::number_value() : stub::string_value();
      else argv[i] = stub::string_value();
    }
  }
  return napi_ok;
}

napi_status napi_create_uint32(napi_env, uint32_t value, napi_value *result) {
  if (stub::throw_create_uint32) throw std::bad_alloc();
  stub::last_uint32 = value;
  *result = stub::number_value();
  return napi_ok;
}

napi_status napi_get_null(napi_env, napi_value *result) {
  if (stub::throw_get_null) throw std::bad_alloc();
  *result = stub::number_value();
  return napi_ok;
}

napi_status napi_define_properties(napi_env, napi_value, size_t, const napi_property_descriptor *) {
  if (stub::throw_define_properties) throw std::bad_alloc();
  return napi_ok;
}

void napi_module_register(napi_module *) {}

}  // extern "C"
