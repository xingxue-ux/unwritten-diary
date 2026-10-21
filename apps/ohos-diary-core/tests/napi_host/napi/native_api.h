// Minimal host-side NAPI surface used by `../harness.cpp`.
//
// This is **not** a copy of the OHOS SDK header: it declares only the symbols
// `napi_shim.cpp` actually calls, so the shim can be compiled and executed on a
// normal Linux host (g++) without the OHOS toolchain. Behavior lives in
// `../stub.cpp`; the fake core `char *`/handle implementations live in
// `../fake_capi.cpp`.
#pragma once
#include <cstddef>
#include <cstdint>

struct napi_env__; using napi_env = napi_env__ *;
struct napi_value__; using napi_value = napi_value__ *;
struct napi_callback_info__; using napi_callback_info = napi_callback_info__ *;
using napi_callback = napi_value (*)(napi_env, napi_callback_info);

enum napi_status { napi_ok = 0, napi_invalid_arg = 1, napi_generic_failure = 2, napi_pending_exception = 3 };
enum napi_valuetype {
  napi_undefined = 0, napi_null = 1, napi_boolean = 2, napi_number = 3,
  napi_string = 4, napi_symbol = 5, napi_object = 6, napi_function = 7,
  napi_external = 8, napi_bigint = 9
};
enum napi_property_attributes { napi_default = 0 };

struct napi_property_descriptor {
  const char *utf8name;
  napi_value name;
  napi_callback method;
  napi_value getter;
  napi_value setter;
  napi_value value;
  napi_property_attributes attributes;
  void *data;
};

struct napi_module {
  int nm_version;
  unsigned int nm_flags;
  const char *nm_filename;
  napi_value (*nm_register_func)(napi_env, napi_value);
  const char *nm_modname;
  void *nm_priv;
  void *reserved[4];
};

extern "C" {
napi_status napi_throw_error(napi_env env, const char *code, const char *msg);
napi_status napi_typeof(napi_env env, napi_value value, napi_valuetype *result);
napi_status napi_get_value_string_utf8(napi_env env, napi_value value, char *buf, size_t bufsize, size_t *result);
napi_status napi_get_value_int64(napi_env env, napi_value value, int64_t *result);
napi_status napi_create_string_utf8(napi_env env, const char *str, size_t length, napi_value *result);
napi_status napi_get_cb_info(napi_env env, napi_callback_info info, size_t *argc, napi_value *argv, napi_value *this_arg, void **data);
napi_status napi_create_uint32(napi_env env, uint32_t value, napi_value *result);
napi_status napi_get_null(napi_env env, napi_value *result);
napi_status napi_define_properties(napi_env env, napi_value object, size_t count, const napi_property_descriptor *properties);
void napi_module_register(napi_module *mod);
}
