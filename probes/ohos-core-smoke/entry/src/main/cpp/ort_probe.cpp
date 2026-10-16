// 鸿蒙沙箱里的 ONNX Runtime 探针（**这是本步的核心产出之一**）。
//
// 背景：产品 Rust 侧用 `ort` crate 的 **load-dynamic**（运行时 dlopen 指定路径的 ORT），
// 所以「沙箱里能不能 dlopen ORT」= 「鸿蒙版有没有语义检索」。
//
// 这个文件**不经过 ort crate**，直接用 ORT 的 C API 把运行时跑起来，因此能把
// 「沙箱策略/依赖」与「crate 的版本门槛」两件事分开回答：
//   ① dlopen libonnxruntime.so（HAP 里 bundled 的那一份）
//   ② OrtGetApiBase → GetVersionString()
//   ③ GetApi(17) / GetApi(16) 分别能不能拿到 API 表 ← 这是 ort crate 的门槛所在
//   ④ CreateEnv → CreateSessionOptions → CreateSession(<dir>/tiny_add.onnx)
//   ⑤ 真跑一次推理：C = A + B，把输出打出来与宿主侧对照
//
// 编译期用的是 **ORT 1.16.3 自带的 onnxruntime_c_api.h**（`ort16/`），
// 因此 OrtApi 表的布局与设备上那个 .so 完全一致，不会出现用新头读旧表的错位。

#include "ort16/onnxruntime_c_api.h"

#include <dlfcn.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

std::string JsonEscape(const std::string &in);
std::string Quoted(const std::string &s);

using GetApiBaseFn = const OrtApiBase *(*)();

std::string SelfDirForOrt() {
    Dl_info info{};
    if (dladdr(reinterpret_cast<void *>(&SelfDirForOrt), &info) == 0 || info.dli_fname == nullptr) {
        return {};
    }
    std::string self(info.dli_fname);
    auto slash = self.find_last_of('/');
    return slash == std::string::npos ? std::string() : self.substr(0, slash + 1);
}

// 把 OrtStatus 变成可读字符串（成功返回空串）。
std::string StatusText(const OrtApi *api, OrtStatus *st) {
    if (st == nullptr) {
        return {};
    }
    const char *msg = api->GetErrorMessage(st);
    std::string out = msg != nullptr ? msg : "<no message>";
    api->ReleaseStatus(st);
    return out;
}

}  // namespace

// 返回 JSON 字符串。`dir` 里应当有 tiny_add.onnx（ArkTS 侧从 rawfile 落到沙箱）。
std::string OrtProbeImpl(const std::string &dir) {
    std::string out = "{";
    const std::string ortPath = SelfDirForOrt() + "libonnxruntime.so";
    out += "\"ort_path\":" + Quoted(ortPath);

    // ── ① dlopen
    (void)dlerror();
    void *h = dlopen(ortPath.c_str(), RTLD_NOW | RTLD_LOCAL);
    const char *dlErr = dlerror();
    out += ",\"dlopen_ok\":";
    out += (h != nullptr ? "true" : "false");
    out += ",\"dlopen_err\":" + Quoted(dlErr != nullptr ? dlErr : "");
    if (h == nullptr) {
        out += "}";
        return out;
    }

    // ── ② OrtGetApiBase
    (void)dlerror();
    auto getBase = reinterpret_cast<GetApiBaseFn>(dlsym(h, "OrtGetApiBase"));
    const char *symErr = dlerror();
    out += ",\"dlsym_OrtGetApiBase_ok\":";
    out += (getBase != nullptr ? "true" : "false");
    out += ",\"dlsym_err\":" + Quoted(symErr != nullptr ? symErr : "");
    if (getBase == nullptr) {
        out += "}";
        return out;
    }

    const OrtApiBase *base = getBase();
    out += ",\"base_nonnull\":";
    out += (base != nullptr ? "true" : "false");
    if (base == nullptr) {
        out += "}";
        return out;
    }
    const char *ver = base->GetVersionString();
    out += ",\"ort_version_string\":" + Quoted(ver != nullptr ? ver : "");

    // ── ③ API 表版本门槛：这几个数字就是「我们的 ort crate 能不能用这个 .so」的答案
    const OrtApi *api17 = base->GetApi(17);
    out += ",\"GetApi17_nonnull\":";
    out += (api17 != nullptr ? "true" : "false");
    const OrtApi *api16 = base->GetApi(16);
    out += ",\"GetApi16_nonnull\":";
    out += (api16 != nullptr ? "true" : "false");
    if (api16 == nullptr) {
        out += "}";
        return out;
    }
    const OrtApi *api = api16;

    // ── ④ 建环境 / session options / 列 EP
    OrtEnv *env = nullptr;
    std::string st = StatusText(api, api->CreateEnv(ORT_LOGGING_LEVEL_WARNING, "ohos-ort-probe", &env));
    out += ",\"CreateEnv_ok\":";
    out += (env != nullptr ? "true" : "false");
    out += ",\"CreateEnv_err\":" + Quoted(st);
    if (env == nullptr) {
        out += "}";
        return out;
    }

    OrtSessionOptions *opts = nullptr;
    st = StatusText(api, api->CreateSessionOptions(&opts));
    out += ",\"CreateSessionOptions_ok\":";
    out += (opts != nullptr ? "true" : "false");
    out += ",\"CreateSessionOptions_err\":" + Quoted(st);
    if (opts == nullptr) {
        out += "}";
        return out;
    }
    (void)api->SetIntraOpNumThreads(opts, 1);
    (void)api->SetSessionGraphOptimizationLevel(opts, ORT_ENABLE_BASIC);

    char **providers = nullptr;
    int providerCount = 0;
    st = StatusText(api, api->GetAvailableProviders(&providers, &providerCount));
    out += ",\"providers_err\":" + Quoted(st);
    out += ",\"providers\":[";
    for (int i = 0; i < providerCount && providers != nullptr; i++) {
        if (i > 0) out += ",";
        out += Quoted(providers[i] != nullptr ? providers[i] : "");
    }
    out += "]";
    if (providers != nullptr) {
        (void)api->ReleaseAvailableProviders(providers, providerCount);
    }

    // ── ⑤ 真建 session + 真跑一次推理
    const std::string modelPath = dir + "/tiny_add.onnx";
    out += ",\"model_path\":" + Quoted(modelPath);
    OrtSession *session = nullptr;
    st = StatusText(api, api->CreateSession(env, modelPath.c_str(), opts, &session));
    out += ",\"CreateSession_ok\":";
    out += (session != nullptr ? "true" : "false");
    out += ",\"CreateSession_err\":" + Quoted(st);
    if (session == nullptr) {
        out += "}";
        return out;
    }

    size_t inputCount = 0;
    size_t outputCount = 0;
    (void)api->SessionGetInputCount(session, &inputCount);
    (void)api->SessionGetOutputCount(session, &outputCount);
    out += ",\"input_count\":" + std::to_string(inputCount);
    out += ",\"output_count\":" + std::to_string(outputCount);

    // C = A + B，A 是 0..5，B 全 1 → 期望 [[1,2,3],[4,5,6]]
    float aData[6] = {0.f, 1.f, 2.f, 3.f, 4.f, 5.f};
    float bData[6] = {1.f, 1.f, 1.f, 1.f, 1.f, 1.f};
    const int64_t shape[2] = {2, 3};
    const size_t elemCount = 6;

    OrtMemoryInfo *memInfo = nullptr;
    st = StatusText(api, api->CreateCpuMemoryInfo(OrtArenaAllocator, OrtMemTypeDefault, &memInfo));
    if (memInfo == nullptr) {
        out += ",\"CreateCpuMemoryInfo_err\":" + Quoted(st) + "}";
        return out;
    }

    OrtValue *inA = nullptr;
    OrtValue *inB = nullptr;
    OrtValue *outC = nullptr;
    st = StatusText(api, api->CreateTensorWithDataAsOrtValue(
                            memInfo, aData, elemCount * sizeof(float), shape, 2,
                            ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT, &inA));
    out += ",\"CreateTensorA_err\":" + Quoted(st);
    st = StatusText(api, api->CreateTensorWithDataAsOrtValue(
                            memInfo, bData, elemCount * sizeof(float), shape, 2,
                            ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT, &inB));
    out += ",\"CreateTensorB_err\":" + Quoted(st);

    if (inA != nullptr && inB != nullptr) {
        const char *inputNames[2] = {"A", "B"};
        const char *outputNames[1] = {"C"};
        const OrtValue *inputs[2] = {inA, inB};
        OrtValue *outputs[1] = {nullptr};
        st = StatusText(api, api->Run(session, nullptr, inputNames,
                                      reinterpret_cast<const OrtValue *const *>(inputs), 2,
                                      outputNames, 1, outputs));
        out += ",\"Run_ok\":";
        out += (outputs[0] != nullptr ? "true" : "false");
        out += ",\"Run_err\":" + Quoted(st);
        if (outputs[0] != nullptr) {
            outC = outputs[0];
            float *result = nullptr;
            (void)api->GetTensorMutableData(outC, reinterpret_cast<void **>(&result));
            out += ",\"C\":[";
            for (size_t i = 0; i < elemCount; i++) {
                if (i > 0) out += ",";
                char buf[32];
                (void)snprintf(buf, sizeof(buf), "%g", result != nullptr ? result[i] : -999.f);
                out += buf;
            }
            out += "]";
            // 与宿主侧 onnxruntime 1.31 的参考输出逐项对照
            const float expected[6] = {1.f, 2.f, 3.f, 4.f, 5.f, 6.f};
            bool match = result != nullptr;
            for (size_t i = 0; i < elemCount && match; i++) {
                match = (result[i] == expected[i]);
            }
            out += ",\"C_matches_host\":";
            out += (match ? "true" : "false");
            api->ReleaseValue(outC);
        }
    }

    if (inA != nullptr) api->ReleaseValue(inA);
    if (inB != nullptr) api->ReleaseValue(inB);
    api->ReleaseMemoryInfo(memInfo);
    api->ReleaseSession(session);
    api->ReleaseSessionOptions(opts);
    api->ReleaseEnv(env);
    out += "}";
    return out;
}

// ── 下面两个是给 napi_shim.cpp 用的最小 JSON 助手（避免把 JSON 逻辑引进来）──
namespace {
std::string JsonEscape(const std::string &in) {
    std::string o;
    o.reserve(in.size() + 8);
    for (unsigned char c : in) {
        switch (c) {
            case '"': o += "\\\""; break;
            case '\\': o += "\\\\"; break;
            case '\n': o += "\\n"; break;
            case '\r': o += "\\r"; break;
            case '\t': o += "\\t"; break;
            default:
                if (c < 0x20) {
                    char buf[8];
                    (void)snprintf(buf, sizeof(buf), "\\u%04x", c);
                    o += buf;
                } else {
                    o += static_cast<char>(c);
                }
        }
    }
    return o;
}
std::string Quoted(const std::string &s) { return "\"" + JsonEscape(s) + "\""; }
}  // namespace