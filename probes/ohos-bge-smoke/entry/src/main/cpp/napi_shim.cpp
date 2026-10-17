// 鸿蒙沙箱探针（第 3 步，真 bge 嵌入）的 C++ NAPI shim。
//
// 它自己**不含任何产品逻辑**，只做三件事：
//   1. 在应用进程里 dlopen 打进 HAP 的 `libdiary_bge_core.so`（Rust 侧编出来的探针核心）；
//   2. 拼出参数 JSON（模型目录 + 同目录下的 `libonnxruntime.so`）并调用 `diary_bge_run`；
//   3. 把「dlopen 成功没有 / 从哪个路径加载的 / dlerror 原文 / ABI 戳 / 核心返回的 JSON 原文」
//      拼成一个 JSON 字符串还给 ArkTS，由 ArkTS 打进 hilog。
//
// 与第 2 步 `probes/ohos-core-smoke` 的 shim 的差别：去掉了 ORT C API 探针与 el2 dlopen 对照
// 实验（那些结论已经拿到），只留下「真模型 → 真嵌入」这一条。

#include <dlfcn.h>
#include <napi/native_api.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>

namespace {

// Rust 侧导出的 C ABI（见 probes/ohos-bge-smoke/rust/src/lib.rs）。
using BgeRunFn = char *(*)(const char *);
using BgeFreeFn = void (*)(char *);
using BgeAbiFn = const char *(*)();

constexpr const char *kCoreSo = "libdiary_bge_core.so";

// 最小的 JSON 字符串转义（够用即可：路径、dlerror 原文、核心返回的 JSON 都要塞进去）。
std::string JsonEscape(const std::string &in) {
    std::string out;
    out.reserve(in.size() + 16);
    for (unsigned char c : in) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (c < 0x20) {
                    char buf[8];
                    (void)snprintf(buf, sizeof(buf), "\\u%04x", c);
                    out += buf;
                } else {
                    out += static_cast<char>(c);
                }
        }
    }
    return out;
}

std::string Quoted(const std::string &s) { return "\"" + JsonEscape(s) + "\""; }

// 本 shim 自己被加载的绝对路径（由 dladdr 给出）——据此推出同目录下的核心库与 ORT。
std::string SelfDir() {
    Dl_info info{};
    if (dladdr(reinterpret_cast<void *>(&SelfDir), &info) == 0 || info.dli_fname == nullptr) {
        return {};
    }
    std::string self(info.dli_fname);
    auto slash = self.find_last_of('/');
    if (slash == std::string::npos) {
        return {};
    }
    return self.substr(0, slash + 1);  // 带结尾 '/'
}

// 按顺序试若干个候选名；第一个成功的就采用。全部失败时留下全部 dlerror 原文。
struct LoadResult {
    void *handle = nullptr;
    std::string used;
    std::string attempts;  // JSON 数组：[{name, ok, err}]
};

LoadResult LoadCore() {
    LoadResult r;
    const std::string dir = SelfDir();
    std::string candidates[2];
    int n = 0;
    // ① 裸文件名：走进程的默认搜索路径（OHOS 应用命名空间里包含自己的 libs 目录）
    candidates[n++] = kCoreSo;
    // ② 绝对路径：本 shim 所在目录的兄弟（最稳，不依赖搜索路径配置）
    if (!dir.empty()) {
        candidates[n++] = dir + kCoreSo;
    }

    std::string arr = "[";
    for (int i = 0; i < n; i++) {
        if (i > 0) arr += ",";
        (void)dlerror();  // 每次都先清空 dlerror
        void *h = dlopen(candidates[i].c_str(), RTLD_NOW | RTLD_LOCAL);
        const char *err = dlerror();
        bool ok = (h != nullptr);
        arr += "{\"name\":" + Quoted(candidates[i]) + ",\"ok\":" + (ok ? "true" : "false") +
               ",\"err\":" + Quoted(err != nullptr ? err : "") + "}";
        if (ok && r.handle == nullptr) {
            r.handle = h;
            r.used = candidates[i];
        } else if (ok && h != nullptr) {
            dlclose(h);  // 多余的句柄，关掉
        }
    }
    arr += "]";
    r.attempts = arr;
    return r;
}

// dlopen + 调用核心的完整过程 → 一个 JSON 字符串。
std::string RunBge(const std::string &modelDir) {
    LoadResult lr = LoadCore();
    std::string out = "{";
    out += "\"dlopen_ok\":" + std::string(lr.handle != nullptr ? "true" : "false");
    out += ",\"dlopen_used\":" + Quoted(lr.used);
    out += ",\"dlopen_attempts\":" + lr.attempts;

    if (lr.handle == nullptr) {
        out += ",\"stage\":\"dlopen_failed\",\"core\":null}";
        return out;
    }

    auto run = reinterpret_cast<BgeRunFn>(dlsym(lr.handle, "diary_bge_run"));
    auto free_fn = reinterpret_cast<BgeFreeFn>(dlsym(lr.handle, "diary_bge_free"));
    auto abi = reinterpret_cast<BgeAbiFn>(dlsym(lr.handle, "diary_bge_abi"));
    if (run == nullptr || free_fn == nullptr) {
        const char *err = dlerror();
        out += ",\"dlsym_ok\":false,\"dlsym_err\":" + Quoted(err != nullptr ? err : "") +
               ",\"core\":null}";
        return out;
    }
    out += ",\"dlsym_ok\":true";
    out += ",\"abi\":" + Quoted(abi != nullptr ? abi() : "<no diary_bge_abi>");
    if (abi != nullptr) {
        // ★ 真实加载路径：对**函数符号**取 dladdr，得到 .so 的文件路径。
        Dl_info info{};
        if (dladdr(reinterpret_cast<void *>(abi), &info) != 0 && info.dli_fname != nullptr) {
            out += ",\"core_loaded_at\":" + Quoted(info.dli_fname);
        } else {
            out += ",\"core_loaded_at\":\"<dladdr failed>\"";
        }
    }

    // 参数 JSON：模型目录由 ArkTS 给（rawfile 落到 filesDir 的那个目录），
    // ORT 就在本 shim 的同目录（HAP 的 libs/arm64/）。
    const std::string ortSo = SelfDir() + "libonnxruntime.so";
    std::string params = "{\"model_dir\":" + Quoted(modelDir) + ",\"ort_so\":" + Quoted(ortSo) + "}";
    out += ",\"params\":" + Quoted(params);

    char *res = run(params.c_str());
    if (res == nullptr) {
        out += ",\"core\":null,\"core_null\":true}";
        return out;
    }
    out += ",\"core\":" + Quoted(std::string(res));
    free_fn(res);
    out += "}";
    return out;
}

std::string ArgString(napi_env env, napi_value v) {
    std::string s;
    size_t len = 0;
    if (napi_get_value_string_utf8(env, v, nullptr, 0, &len) == napi_ok && len > 0) {
        s.resize(len);
        size_t copied = 0;
        napi_get_value_string_utf8(env, v, s.data(), len + 1, &copied);
        s.resize(copied);
    }
    return s;
}

// JS: runBge(modelDir: string) -> string(JSON)
napi_value RunBgeJs(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value argv[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    std::string dir;
    if (argc >= 1) {
        dir = ArgString(env, argv[0]);
    }
    std::string json = RunBge(dir);
    napi_value ret = nullptr;
    napi_create_string_utf8(env, json.c_str(), json.size(), &ret);
    return ret;
}

// JS: selfDir() -> string   本 shim 自己被加载的目录（排障用）
napi_value SelfDirJs(napi_env env, napi_callback_info info) {
    (void)info;
    std::string d = SelfDir();
    napi_value ret = nullptr;
    napi_create_string_utf8(env, d.c_str(), d.size(), &ret);
    return ret;
}

napi_value Init(napi_env env, napi_value exports) {
    napi_property_descriptor desc[] = {
        {"runBge", nullptr, RunBgeJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"selfDir", nullptr, SelfDirJs, nullptr, nullptr, nullptr, napi_default, nullptr},
    };
    napi_define_properties(env, exports, sizeof(desc) / sizeof(desc[0]), desc);
    return exports;
}

napi_module g_module = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "diary_bge",  // 必须与 libdiary_bge.so 去掉 lib/后缀一致
    .nm_priv = nullptr,
    .reserved = {nullptr},
};

}  // namespace

extern "C" __attribute__((constructor)) void RegisterDiaryBgeModule() {
    napi_module_register(&g_module);
}