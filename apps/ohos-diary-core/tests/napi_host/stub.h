// Fault-injection switches for the host NAPI stub. See `harness.cpp`.
#pragma once
#include <napi/native_api.h>
#include <string>

namespace stub {
extern bool huge_string_len;          // napi_get_value_string_utf8 reports a huge length
extern bool throw_create_string;      // napi_create_string_utf8 throws std::bad_alloc
extern bool throw_create_uint32;      // napi_create_uint32 throws std::bad_alloc
extern bool throw_get_null;           // napi_get_null throws std::bad_alloc
extern bool throw_cb_info;            // napi_get_cb_info throws std::bad_alloc
extern bool throw_define_properties;  // napi_define_properties throws std::bad_alloc
extern bool first_arg_is_number;      // argv[0] is a number (handle id) instead of a string
extern uint32_t last_uint32;          // last value passed to napi_create_uint32
extern std::string last_code;         // last napi_throw_error code
extern std::string last_message;      // last napi_throw_error message
napi_value number_value();
napi_value string_value();
void reset();
}  // namespace stub
