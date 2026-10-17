// 鸿蒙沙箱探针的 C++ NAPI shim。
//
// 它自己**不含任何产品逻辑**，只做一件事：
//   1. 在应用进程里 dlopen 打进 HAP 的 `libdiary_core_smoke.so`（Rust 编出来的核心）；
//   2. dlsym 出 `diary_smoke_*` 三个符号并调用；
//   3. 把「dlopen 成功没有 / 从哪个路径加载的 / dlerror 原文 / ABI 戳 / 核心返回的 JSON 原文」
//      拼成一个 JSON 字符串还给 ArkTS，由 ArkTS 打进 hilog。
//
// 这层的存在本身就是结论的一部分：**HAP 里的原生库必须由应用进程自己 dlopen，
// shell 直接执行 ELF 会被系统拒绝（第 1 步已实测）**。

#include <dlfcn.h>
#include <napi/native_api.h>

#include <cerrno>
#include <sys/stat.h>
#include <unistd.h>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>

// 定义在 ort_probe.cpp：用 ONNX Runtime 的 C API 在沙箱里把 libonnxruntime.so 跑起来。
std::string OrtProbeImpl(const std::string &dir);

namespace {

// Rust 侧导出的 C ABI（见 probes/ohos-core-smoke/rust/src/lib.rs）。
using SmokeRunFn = char *(*)(const char *);
using SmokeFreeFn = void (*)(char *);
using SmokeAbiFn = const char *(*)();


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

// 本 shim 自己被加载的绝对路径（由 dladdr 给出）——据此推出同目录下的核心库。
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
    std::string used;       // 成功的那个名字（空 = 全失败）
    std::string attempts;   // JSON 数组：[{name, ok, err}]
};

LoadResult LoadCore() {
    LoadResult r;
    const std::string dir = SelfDir();
    std::string candidates[2];
    int n = 0;
    // ① 裸文件名：走进程的默认搜索路径（OHOS 应用命名空间里包含自己的 libs 目录）
    candidates[n++] = "libdiary_core_smoke.so";
    // ② 绝对路径：本 shim 所在目录的兄弟（最稳，不依赖搜索路径配置）
    if (!dir.empty()) {
        candidates[n++] = dir + "libdiary_core_smoke.so";
    }

    std::string arr = "[";
    for (int i = 0; i < n; i++) {
        if (i > 0) arr += ",";
        // 每次都先清空 dlerror。
        (void)dlerror();
        void *h = dlopen(candidates[i].c_str(), RTLD_NOW | RTLD_LOCAL);
        const char *err = dlerror();
        bool ok = (h != nullptr);
        arr += "{\"name\":" + Quoted(candidates[i]) + ",\"ok\":" + (ok ? "true" : "false") +
               ",\"err\":" + Quoted(err != nullptr ? err : "") + "}";
        if (ok && r.handle == nullptr) {
            r.handle = h;
            r.used = candidates[i];
            // 注意：dladdr 要的是「库内某个地址」，不能拿 dlopen 的句柄当地址用。
            // 真实加载路径在 RunSmoke 里对 diary_smoke_abi 的函数地址取 dladdr 得到。
        } else if (ok && h != nullptr) {
            // 已经拿到一个了，把这个多余的关掉，避免重复句柄。
            dlclose(h);
        }
    }
    arr += "]";
    r.attempts = arr;
    return r;
}

// 把 dlopen + 调用核心的完整过程做成一个 JSON 字符串。
std::string RunSmoke(const std::string &dir) {
    LoadResult lr = LoadCore();
    std::string out = "{";
    out += "\"dlopen_ok\":" + std::string(lr.handle != nullptr ? "true" : "false");
    out += ",\"dlopen_used\":" + Quoted(lr.used);
    out += ",\"dlopen_attempts\":" + lr.attempts;

    if (lr.handle == nullptr) {
        out += ",\"core\":null}";
        return out;
    }

    auto run = reinterpret_cast<SmokeRunFn>(dlsym(lr.handle, "diary_smoke_run"));
    auto free_fn = reinterpret_cast<SmokeFreeFn>(dlsym(lr.handle, "diary_smoke_free"));
    auto abi = reinterpret_cast<SmokeAbiFn>(dlsym(lr.handle, "diary_smoke_abi"));
    if (run == nullptr || free_fn == nullptr) {
        const char *err = dlerror();
        out += ",\"dlsym_ok\":false,\"dlsym_err\":" + Quoted(err != nullptr ? err : "") + ",\"core\":null}";
        return out;
    }
    out += ",\"dlsym_ok\":true";
    out += ",\"abi\":" + Quoted(abi != nullptr ? abi() : "<no diary_smoke_abi>");

    // ★ 真实加载路径：对**函数符号**取 dladdr，得到 .so 的文件路径。
    if (abi != nullptr) {
        Dl_info info{};
        if (dladdr(reinterpret_cast<void *>(abi), &info) != 0 && info.dli_fname != nullptr) {
            out += ",\"core_loaded_at\":" + Quoted(info.dli_fname);
        } else {
            out += ",\"core_loaded_at\":\"<dladdr failed>\"";
        }
    }

    char *res = run(dir.c_str());
    if (res == nullptr) {
        out += ",\"core\":null,\"core_null\":true}";
        return out;
    }
    out += ",\"core\":" + Quoted(std::string(res));
    free_fn(res);
    out += "}";
    return out;
}

// JS: runSmoke(dir: string) -> string(JSON)
napi_value RunSmokeJs(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value argv[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);

    std::string dir;
    if (argc >= 1) {
        size_t len = 0;
        if (napi_get_value_string_utf8(env, argv[0], nullptr, 0, &len) == napi_ok && len > 0) {
            dir.resize(len);
            size_t copied = 0;
            napi_get_value_string_utf8(env, argv[0], dir.data(), len + 1, &copied);
            dir.resize(copied);
        }
    }

    std::string json = RunSmoke(dir);
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

// 把 HAP 里的核心库复制到**可写沙箱目录**（el2），再从那里 dlopen。
//
// 这是给「将来能不能动态下发原生库（模型运行时 / 插件）」这个问题探路的：
// 第 1 步已经实测 shell 直接执行 ELF 会被系统拒绝（Q5）；这里是应用进程里
// 从 el2 dlopen。系统策略是否允许，只有实测能回答。
// 单个库的「复制到 el2 → 校验 → dlopen」实验。返回一段 JSON 对象。
//
// 校验这一步是**故意加的**：上一轮 dlopen 报的是 "No such file or directory"，
// 但文件明明写成功。为了排除「文件没写完 / 权限位 / 内容损坏」这几种解释，
// 这里把 源大小、复制字节数、目的文件 stat 大小、前 4 字节 ELF magic、mode 全打出来。
std::string TryDlopenCopy(const std::string &dir, const std::string &name, const std::string &dstName) {
    const std::string src = SelfDir() + name;
    const std::string dst = dir + "/" + dstName;
    std::string out = "{";
    out += "\"name\":" + Quoted(name);
    out += ",\"src\":" + Quoted(src) + ",\"dst\":" + Quoted(dst);

    struct stat srcStat {};
    out += ",\"src_bytes\":";
    out += (stat(src.c_str(), &srcStat) == 0) ? std::to_string(srcStat.st_size) : std::string("-1");

    // ① 复制
    FILE *in = fopen(src.c_str(), "rb");
    if (in == nullptr) {
        out += ",\"copy_ok\":false,\"copy_err\":" + Quoted(strerror(errno)) + ",\"dlopen_ok\":false}";
        return out;
    }
    FILE *of = fopen(dst.c_str(), "wb");
    if (of == nullptr) {
        int e = errno;
        fclose(in);
        out += ",\"copy_ok\":false,\"copy_err\":" + Quoted(strerror(e)) + ",\"dlopen_ok\":false}";
        return out;
    }
    char buf[65536];
    size_t n = 0;
    long long total = 0;
    while ((n = fread(buf, 1, sizeof(buf), in)) > 0) {
        if (fwrite(buf, 1, n, of) != n) break;
        total += static_cast<long long>(n);
    }
    fclose(in);
    fclose(of);
    out += ",\"copy_ok\":true,\"copy_bytes\":" + std::to_string(total);

    // ①b 可执行位（普通 Linux dlopen 不需要，这里排除「OHOS 要求 +x」这个解释）
    (void)chmod(dst.c_str(), 0755);

    // ①c 写完之后重新 stat + 读回前 4 字节确认真的是完整 ELF
    struct stat dstStat {};
    bool haveStat = (stat(dst.c_str(), &dstStat) == 0);
    out += ",\"dst_exists\":";
    out += (haveStat ? "true" : "false");
    out += ",\"dst_bytes\":";
    out += haveStat ? std::to_string(dstStat.st_size) : std::string("-1");
    {
        char mbuf[16];
        (void)snprintf(mbuf, sizeof(mbuf), "%o", haveStat ? (dstStat.st_mode & 07777) : 0);
        out += ",\"dst_mode_oct\":" + Quoted(mbuf);
    }
    out += ",\"readable\":";
    out += (access(dst.c_str(), R_OK) == 0 ? "true" : "false");
    out += ",\"executable\":";
    out += (access(dst.c_str(), X_OK) == 0 ? "true" : "false");
    {
        FILE *chk = fopen(dst.c_str(), "rb");
        unsigned char magic[4] = {0, 0, 0, 0};
        bool magicOk = false;
        if (chk != nullptr) {
            size_t got = fread(magic, 1, 4, chk);
            magicOk = (got == 4 && magic[0] == 0x7f && magic[1] == 'E' && magic[2] == 'L' && magic[3] == 'F');
            fclose(chk);
        }
        out += ",\"readback_elf_magic\":";
        out += (magicOk ? "true" : "false");
    }

    // ② 从可写目录 dlopen
    (void)dlerror();
    void *h = dlopen(dst.c_str(), RTLD_NOW | RTLD_LOCAL);
    const char *err = dlerror();
    out += ",\"dlopen_ok\":";
    out += (h != nullptr ? "true" : "false");
    out += ",\"dlopen_err\":" + Quoted(err != nullptr ? err : "");
    if (h != nullptr) {
        auto abi = reinterpret_cast<SmokeAbiFn>(dlsym(h, "diary_smoke_abi"));
        out += ",\"abi\":" + Quoted(abi != nullptr ? abi() : "<no diary_smoke_abi>");
        dlclose(h);
    }
    out += "}";
    return out;
}

// 对两个库都做一遍：小的（16 KB，排除体积因素）和大的（3.8 MB，核心本体）。
std::string DlopenFromDir(const std::string &dir) {
    std::string out = "{\"dst_dir\":" + Quoted(dir) + ",\"results\":[";
    out += TryDlopenCopy(dir, "libdiary_napi.so", "libcopy_napi.so");
    out += ",";
    out += TryDlopenCopy(dir, "libonnxruntime.so", "libcopy_ort.so");
    out += "]}";
    return out;
}

napi_value DlopenFromDirJs(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value argv[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    std::string dir;
    if (argc >= 1) {
        size_t len = 0;
        if (napi_get_value_string_utf8(env, argv[0], nullptr, 0, &len) == napi_ok && len > 0) {
            dir.resize(len);
            size_t copied = 0;
            napi_get_value_string_utf8(env, argv[0], dir.data(), len + 1, &copied);
            dir.resize(copied);
        }
    }
    std::string json = DlopenFromDir(dir);
    napi_value ret = nullptr;
    napi_create_string_utf8(env, json.c_str(), json.size(), &ret);
    return ret;
}

// JS: ortProbe(dir: string) -> string(JSON)
// 在沙箱里 dlopen libonnxruntime.so，取版本号/API 表，建 session，跑 tiny_add.onnx。
napi_value OrtProbeJs(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value argv[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    std::string dir;
    if (argc >= 1) {
        size_t len = 0;
        if (napi_get_value_string_utf8(env, argv[0], nullptr, 0, &len) == napi_ok && len > 0) {
            dir.resize(len);
            size_t copied = 0;
            napi_get_value_string_utf8(env, argv[0], dir.data(), len + 1, &copied);
            dir.resize(copied);
        }
    }
    std::string json = OrtProbeImpl(dir);
    napi_value ret = nullptr;
    napi_create_string_utf8(env, json.c_str(), json.size(), &ret);
    return ret;
}

// JS: ortCrateProbe() -> string(JSON)
// 走**产品真实调用路径**：Rust 侧 `ort` crate 的 load-dynamic。
napi_value OrtCrateProbeJs(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value argv[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    std::string modelDir;
    if (argc >= 1) {
        size_t len = 0;
        if (napi_get_value_string_utf8(env, argv[0], nullptr, 0, &len) == napi_ok && len > 0) {
            modelDir.resize(len);
            size_t copied = 0;
            napi_get_value_string_utf8(env, argv[0], modelDir.data(), len + 1, &copied);
            modelDir.resize(copied);
        }
    }
    const std::string ortPath = SelfDir() + "libonnxruntime.so";
    std::string json = "{\"error\":\"libdiary_core_smoke.so 未加载\"}";
    // 复用 runSmoke 那套 dlopen/dlsym：这里只需要 diary_ort_crate_probe 一个符号。
    void *h = dlopen("libdiary_core_smoke.so", RTLD_NOW | RTLD_LOCAL);
    if (h == nullptr) {
        h = dlopen((SelfDir() + "libdiary_core_smoke.so").c_str(), RTLD_NOW | RTLD_LOCAL);
    }
    if (h != nullptr) {
        using CrateProbeFn = char *(*)(const char *, const char *);
        auto probe = reinterpret_cast<CrateProbeFn>(dlsym(h, "diary_ort_crate_probe"));
        auto freeFn = reinterpret_cast<SmokeFreeFn>(dlsym(h, "diary_smoke_free"));
        if (probe != nullptr) {
            char *res = probe(ortPath.c_str(), modelDir.c_str());
            if (res != nullptr) {
                json = std::string(res);
                if (freeFn != nullptr) {
                    freeFn(res);
                }
            }
        } else {
            const char *e = dlerror();
            json = std::string("{\"dlsym_err\":") + Quoted(e != nullptr ? e : "") + "}";
        }
    }
    napi_value ret = nullptr;
    napi_create_string_utf8(env, json.c_str(), json.size(), &ret);
    return ret;
}

napi_value Init(napi_env env, napi_value exports) {
    napi_property_descriptor desc[] = {
        {"runSmoke", nullptr, RunSmokeJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"selfDir", nullptr, SelfDirJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"dlopenFromDir", nullptr, DlopenFromDirJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"ortProbe", nullptr, OrtProbeJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"ortCrateProbe", nullptr, OrtCrateProbeJs, nullptr, nullptr, nullptr, napi_default, nullptr},
    };
    napi_define_properties(env, exports, sizeof(desc) / sizeof(desc[0]), desc);
    return exports;
}

napi_module g_module = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "diary_napi",  // 必须与 libdiary_napi.so 去掉 lib/后缀一致
    .nm_priv = nullptr,
    .reserved = {nullptr},
};

}  // namespace

extern "C" __attribute__((constructor)) void RegisterDiaryNapiModule() {
    napi_module_register(&g_module);
}