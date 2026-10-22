// libdiary_napi.so —— 把 `crates/diary_capi` 的稳定 C ABI 包成 ArkTS 能直接 import 的
// NAPI 模块（#108 第二块）。
//
// 这一层**不做业务**、**不拆 JSON**：
//
//   * 请求是一整条 JSON 字符串（`{"method":"…","params":{…}}`），原样交给
//     `diary_call(handle, json)`；
//   * 响应是 `diary_capi` 返回的信封 JSON（成功 `ok:true` / 失败 `ok:false`），原样作为
//     JS string 交回去；
//   * 语义解析（把 `ok:false` 变成 ArkTS 异常、把 `data` 变成类型化对象）只在 ArkTS 侧的
//     `DiaryCore.ets` 里做一次。
//
// 这么分层的理由：C ABI 的方法面还会继续加（现在 59 条），每加一条都不该动 C++ 胶水；
// 而「一进一出」的 JSON 面是已经冻结、可版本化的（信封里的 `apiVersion`）。
//
// JS 面（与 `entry/src/main/cpp/types/libdiary_napi/index.d.ts` 一一对应）：
//
//   version(): string                   —— diary_capi_version() 原文（诊断用版本戳）
//   open(path: string): number          —— 打开资料库，返回**句柄 id**（小整数）
//   close(handle: number): void         —— 关闭句柄
//   call(handle: number, json: string): string —— 调一个方法，返回信封 JSON 原文
//   lastError(): string | null          —— 本线程最近一条错误（JSON，与信封 error 同形）
//   selfDir(): string                   —— 本 shim 被加载的目录（排障用）
//   openHandleCount(): number           —— 还没关闭的句柄数（排障 / 泄漏自检）
//
// # 为什么句柄是 number 而不是 bigint / 裸指针
//
// `diary_open` 返回的是 64 位指针，JS 的 number 装不下。这里**不把指针交给 JS**，
// 而是登记成 1、2、3… 的 id（`uint32`，远小于 2^53）：
//
//   * 指针不跨语言，JS 造不出一个「像指针的数」去喂 C；
//   * ArkTS 侧不用 bigint（JSON / hilog 里都不需要特殊处理）；
//   * `DiaryCore` 类把 id 收在私有字段里，应用代码根本看不到它。
//
// id 只在本进程有效，进程重启即失效；每次 `open` 都换新 id，不复用。
//
// # 错误口径（与 ArkTS 侧一致，见 docs/architecture/m6-ArkTS-NAPI.md §4）
//
// NAPI 层只在「调用没能发生」时抛 JS 异常：
//
//   * `ERR_DIARY_LIB`    —— 核心 .so 加载失败 / 符号缺失（打包或部署问题）；
//   * `ERR_DIARY_ARGS`   —— 参数类型不对；
//   * `ERR_DIARY_HANDLE` —— 句柄 id 未知或已关闭；
//   * `ERR_DIARY_OPEN`   —— `diary_open` 返回 NULL，message 是 `diary_last_error()` 的 JSON 原文；
//   * `ERR_DIARY_CALL`   —— `diary_call` 返回 NULL（只可能是内存分配失败）。
//
// **业务错误不抛**：`call()` 照常返回 `ok:false` 的信封字符串。把信封变成异常是 ArkTS
// 侧 `DiaryCore` 的职责——这样「能用 JSON 的地方」（日志、回放、离线测试）仍拿到原始信封。
//
// 本文件里所有到达 JS 的字符串都在 C++ 侧复制过，并立刻用 `diary_string_free` 释放
// 核心返回的 `char *`。**没有一根 C 指针活到 JS 里**，所以不需要导出 `stringFree`：
// 也就没有「重复释放 / 忘了释放」这一整类错误。
//
// 注意：`diary_capi` 自己的入口已经 `catch_unwind`，panic 不会穿过 FFI；本层不重复兜底。
//
// # C++ 异常兜底（#133）
//
// 本层会分配 `std::string` / `std::vector` / `std::unordered_map` 节点；分配失败时
// 抛 `std::bad_alloc`。C++ 异常穿过 NAPI 边界不是 UB，而是直接 `std::terminate`
// （JS 侧看不到错误码，进程也没了）。所以**每个** NAPI 回调入口都经过 `Guarded()`：
// 兜底成既有的 `ERR_DIARY_CALL`（不新增码），并保证已经拿到的 C 资源（`char *` /
// 句柄）在栈展开时归还（`CoreText` / `PendingHandle`）。

#include <dlfcn.h>
#include <napi/native_api.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <mutex>
#include <new>
#include <string>
#include <unordered_map>
#include <vector>

namespace {

// ---------------------------------------------------------------- 核心 C ABI

// `crates/diary_capi/src/lib.rs` 导出的 6 个符号。句柄是 `void *`（C 侧只当令牌用）。
using VersionFn = char *(*)();
using OpenFn = void *(*)(const char *);
using CloseFn = int (*)(void *);
using CallFn = char *(*)(void *, const char *);
using StringFreeFn = void (*)(char *);
using LastErrorFn = char *(*)();

struct CoreApi {
    void *handle = nullptr;  // dlopen 句柄；空 = 没加载成功
    std::string load_error;  // 失败原因（带每次尝试的 dlerror 原文），已经是可以直接给人看的字符串
    VersionFn version = nullptr;
    OpenFn open = nullptr;
    CloseFn close = nullptr;
    CallFn call = nullptr;
    StringFreeFn string_free = nullptr;
    LastErrorFn last_error = nullptr;
};

// 拥有 `diary_capi` 返回的 `char *`：拷贝成 `std::string` 时若抛 `std::bad_alloc`，
// 或后续任何一步抛错，析构都会把 C 指针还给核心——不会漏掉 `string_free`。
class CoreText {
public:
    CoreText(CoreApi &api, char *text) : api_(&api), text_(text) {}
    ~CoreText() {
        if (text_ != nullptr) {
            api_->string_free(text_);
        }
    }
    CoreText(const CoreText &) = delete;
    CoreText &operator=(const CoreText &) = delete;

    const char *get() const { return text_; }
    std::string Copy() const { return std::string(text_); }

private:
    CoreApi *api_;
    char *text_;
};

// 本 shim 自己被加载的目录（带结尾 '/'）。给「同目录的兄弟 .so」用，不依赖搜索路径配置。
std::string SelfDir() {
    Dl_info info{};
    if (dladdr(reinterpret_cast<void *>(&SelfDir), &info) == 0 || info.dli_fname == nullptr) {
        return {};
    }
    const std::string self(info.dli_fname);
    const auto slash = self.find_last_of('/');
    if (slash == std::string::npos) {
        return {};
    }
    return self.substr(0, slash + 1);
}

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

// 按顺序试候选路径，第一个能同时取到 6 个符号的采用。结果缓存（进程内只加载一次）。
CoreApi LoadCore() {
    CoreApi api;
    std::vector<std::string> candidates;
    // ① 环境变量：排障 / 灰度时可以指到任意一份（真机上一般用不到）。
    if (const char *override = std::getenv("DIARY_CAPI_LIB"); override != nullptr && *override != '\0') {
        candidates.emplace_back(override);
    }
    // ② 本 shim 的兄弟：HAP 里两个 .so 都在 libs/arm64-v8a/，设备上落到同一个目录。
    const std::string dir = SelfDir();
    if (!dir.empty()) {
        candidates.push_back(dir + "libdiary_capi.so");
    }
    // ③ 裸文件名：走应用进程的默认搜索路径（OHOS 的 module namespace 里有自己的 libs 目录）。
    candidates.emplace_back("libdiary_capi.so");

    std::string attempts = "[";
    for (size_t i = 0; i < candidates.size(); i++) {
        if (i > 0) {
            attempts += ",";
        }
        (void)dlerror();  // 清掉上一次的残留，dlerror 才准
        void *h = dlopen(candidates[i].c_str(), RTLD_NOW | RTLD_LOCAL);
        const char *err = dlerror();
        const bool ok = (h != nullptr);
        attempts += "{\"path\":" + Quoted(candidates[i]) + ",\"ok\":" + (ok ? "true" : "false") +
                    ",\"err\":" + Quoted(err != nullptr ? err : "") + "}";
        if (!ok) {
            continue;
        }
        auto version = reinterpret_cast<VersionFn>(dlsym(h, "diary_capi_version"));
        auto open = reinterpret_cast<OpenFn>(dlsym(h, "diary_open"));
        auto close = reinterpret_cast<CloseFn>(dlsym(h, "diary_close"));
        auto call = reinterpret_cast<CallFn>(dlsym(h, "diary_call"));
        auto string_free = reinterpret_cast<StringFreeFn>(dlsym(h, "diary_string_free"));
        auto last_error = reinterpret_cast<LastErrorFn>(dlsym(h, "diary_last_error"));
        if (version == nullptr || open == nullptr || close == nullptr || call == nullptr ||
            string_free == nullptr || last_error == nullptr) {
            api.load_error = "核心库加载了，但符号不全（" + candidates[i] + "）：" + attempts + "]";
            dlclose(h);
            continue;
        }
        api.handle = h;
        api.version = version;
        api.open = open;
        api.close = close;
        api.call = call;
        api.string_free = string_free;
        api.last_error = last_error;
        return api;
    }
    attempts += "]";
    api.load_error = "打不开 libdiary_capi.so（selfDir=" + Quoted(dir) + "，尝试：" + attempts +
                     "；请确认库在 entry/libs/arm64-v8a/ 且 libc++_shared.so 随包）";
    return api;
}

// 进程内只加载一次。加载失败也缓存（要恢复得重装 / 重启进程，重试没有意义）。
CoreApi &Core() {
    static CoreApi api = LoadCore();
    return api;
}

// 取走 `diary_last_error()` 的 JSON 原文（没有错误时给一条兜底消息）。
std::string TakeLastError(CoreApi &api) {
    if (api.last_error == nullptr) {
        return "{\"code\":\"invalid_state\",\"message\":\"核心没有给出错误详情\",\"retryable\":false}";
    }
    CoreText text(api, api.last_error());
    if (text.get() == nullptr) {
        return "{\"code\":\"invalid_state\",\"message\":\"核心没有给出错误详情\",\"retryable\":false}";
    }
    return text.Copy();
}

// ---------------------------------------------------------------- 句柄登记表

// JS 可见的句柄 id → C 句柄。**永不把 C 指针交给 JS**。
class HandleTable {
public:
    uint32_t Add(void *handle) {
        std::lock_guard<std::mutex> lock(mutex_);
        const uint32_t id = next_++;
        entries_.emplace(id, handle);
        return id;
    }

    // 只查不改（`call` 用：调用完句柄还要能继续用）。不存在返回 nullptr。
    void *Peek(uint32_t id) {
        std::lock_guard<std::mutex> lock(mutex_);
        const auto it = entries_.find(id);
        return it == entries_.end() ? nullptr : it->second;
    }

    // 取走并移除；不存在返回 nullptr（调用方据此报「句柄无效或已关闭」）。
    void *Take(uint32_t id) {
        std::lock_guard<std::mutex> lock(mutex_);
        const auto it = entries_.find(id);
        if (it == entries_.end()) {
            return nullptr;
        }
        void *handle = it->second;
        entries_.erase(it);
        return handle;
    }

    size_t Size() {
        std::lock_guard<std::mutex> lock(mutex_);
        return entries_.size();
    }

private:
    std::mutex mutex_;
    std::unordered_map<uint32_t, void *> entries_;
    uint32_t next_ = 1;
};

HandleTable &Handles() {
    static HandleTable table;
    return table;
}

// 拥有一个「还没交给 JS」的 open 结果：C 句柄 + 可能已经登记进表的 id。
//
// open() 的顺序是 `Handles().Add(handle)` → `napi_create_uint32` → 交给 JS。
// 中间任何一步失败（返回 `napi_status` 失败）**或抛 C++ 异常**（本层承诺兜底任意
// 异常，harness 正是往这里注入 `bad_alloc` 的），两样都不能留：
//   * 没关的 C 句柄 → `diary_close` 泄漏；
//   * `id → 已关闭指针` 的悬空表项 → 之后 `close(id)` 可能 double-close / UAF，
//     `openHandleCount()` 还会虚报泄漏。
// 所以析构**先** `Take(id)` **再** close：两步都在栈展开时执行。正常路径
// `Dismiss()` 之后，id 归 JS、句柄归调用方。
class PendingHandle {
public:
    PendingHandle(CoreApi &api, void *handle) : api_(&api), handle_(handle) {}
    ~PendingHandle() {
        if (id_ != 0) {
            (void)Handles().Take(id_);
        }
        if (handle_ != nullptr && api_->close != nullptr) {
            (void)api_->close(handle_);
        }
    }
    PendingHandle(const PendingHandle &) = delete;
    PendingHandle &operator=(const PendingHandle &) = delete;

    // 记下表里的 id。`Handles().Add` 自己抛错时 id 仍是 0，析构只 close 句柄。
    void Adopt(uint32_t id) { id_ = id; }
    void Dismiss() {
        id_ = 0;
        handle_ = nullptr;
    }

private:
    CoreApi *api_;
    void *handle_;
    uint32_t id_ = 0;
};

// ---------------------------------------------------------------- NAPI 小工具

void Throw(napi_env env, const char *code, const std::string &message) {
    // napi_throw_error 会把 code 挂到 JS Error 的 `code` 上（ArkTS 侧据此区分「原生层失败」）。
    napi_throw_error(env, code, message.c_str());
}

// 异常兜底路径本身不能再分配内存（bad_alloc 时再构造 std::string 会直接 terminate）。
// 调用方用栈上的 `char` 缓冲 + `snprintf`，这里只做一次 C 调用。
void ThrowLiteral(napi_env env, const char *code, const char *message) noexcept {
    (void)napi_throw_error(env, code, message);
}

// 所有 NAPI 回调的统一入口：任何漏网的 C++ 异常（含 std::bad_alloc）都变成
// JS 侧的 `ERR_DIARY_CALL`，绝不让它穿过 NAPI 边界 terminate 进程。
// `snprintf` 不分配内存，catch 里构造消息是安全的。
template <typename Body>
napi_value Guarded(napi_env env, const char *entry, Body &&body) noexcept {
    try {
        return body();
    } catch (const std::bad_alloc &) {
        char message[256];
        (void)snprintf(message, sizeof(message), "%s: std::bad_alloc（原生层内存不足）", entry);
        ThrowLiteral(env, "ERR_DIARY_CALL", message);
    } catch (const std::exception &error) {
        char message[512];
        (void)snprintf(message, sizeof(message), "%s: %s", entry, error.what());
        ThrowLiteral(env, "ERR_DIARY_CALL", message);
    } catch (...) {
        char message[256];
        (void)snprintf(message, sizeof(message), "%s: 未知 C++ 异常", entry);
        ThrowLiteral(env, "ERR_DIARY_CALL", message);
    }
    return nullptr;
}

bool IsString(napi_env env, napi_value value) {
    napi_valuetype type = napi_undefined;
    if (napi_typeof(env, value, &type) != napi_ok) {
        return false;
    }
    return type == napi_string;
}

bool IsNumber(napi_env env, napi_value value) {
    napi_valuetype type = napi_undefined;
    if (napi_typeof(env, value, &type) != napi_ok) {
        return false;
    }
    return type == napi_number;
}

// JS string → UTF-8 std::string。两段式取长度，避免给 std::string::data() 越界写 NUL。
bool ReadString(napi_env env, napi_value value, std::string &out) {
    size_t len = 0;
    if (napi_get_value_string_utf8(env, value, nullptr, 0, &len) != napi_ok) {
        return false;
    }
    std::vector<char> buf(len + 1, '\0');
    size_t copied = 0;
    if (napi_get_value_string_utf8(env, value, buf.data(), buf.size(), &copied) != napi_ok) {
        return false;
    }
    out.assign(buf.data(), copied);
    return true;
}

bool ReadUint32(napi_env env, napi_value value, uint32_t &out) {
    int64_t number = 0;
    if (napi_get_value_int64(env, value, &number) != napi_ok) {
        return false;
    }
    if (number <= 0 || number > 4294967295LL) {  // id 从 1 开始；上界 = uint32
        return false;
    }
    out = static_cast<uint32_t>(number);
    return true;
}

napi_value MakeString(napi_env env, const std::string &text) {
    napi_value out = nullptr;
    if (napi_create_string_utf8(env, text.c_str(), text.size(), &out) != napi_ok) {
        return nullptr;
    }
    return out;
}

// 取第 index 个参数并读成字符串；失败时已经抛好异常，返回 false。
bool ReadStringArg(napi_env env, napi_callback_info info, size_t index, const char *what,
                   std::string &out) {
    size_t argc = index + 1;
    std::vector<napi_value> argv(argc, nullptr);
    if (napi_get_cb_info(env, info, &argc, argv.data(), nullptr, nullptr) != napi_ok) {
        Throw(env, "ERR_DIARY_ARGS", std::string("读不到参数：") + what);
        return false;
    }
    if (argc <= index || argv[index] == nullptr) {
        Throw(env, "ERR_DIARY_ARGS", std::string("缺少参数：") + what);
        return false;
    }
    if (!IsString(env, argv[index])) {
        Throw(env, "ERR_DIARY_ARGS", std::string("参数不是字符串：") + what);
        return false;
    }
    if (!ReadString(env, argv[index], out)) {
        Throw(env, "ERR_DIARY_ARGS", std::string("参数不是合法 UTF-8：") + what);
        return false;
    }
    return true;
}

// 取第 index 个参数并读成句柄 id；失败时已经抛好异常，返回 false。
bool ReadHandleArg(napi_env env, napi_callback_info info, size_t index, uint32_t &out) {
    size_t argc = index + 1;
    std::vector<napi_value> argv(argc, nullptr);
    if (napi_get_cb_info(env, info, &argc, argv.data(), nullptr, nullptr) != napi_ok) {
        Throw(env, "ERR_DIARY_ARGS", "读不到句柄参数");
        return false;
    }
    if (argc <= index || argv[index] == nullptr) {
        Throw(env, "ERR_DIARY_ARGS", "缺少句柄参数");
        return false;
    }
    if (!IsNumber(env, argv[index]) || !ReadUint32(env, argv[index], out)) {
        Throw(env, "ERR_DIARY_ARGS", "句柄必须是 open() 返回的正整数");
        return false;
    }
    return true;
}

// 核心没加载成功时统一在这里抛，message 里带完整的尝试记录。
bool RequireCore(napi_env env, CoreApi *&out) {
    CoreApi &api = Core();
    if (api.handle == nullptr) {
        Throw(env, "ERR_DIARY_LIB", api.load_error);
        return false;
    }
    out = &api;
    return true;
}

// ---------------------------------------------------------------- JS 函数

// JS: version() -> string
napi_value VersionJsImpl(napi_env env, napi_callback_info info) {
    (void)info;
    CoreApi *api = nullptr;
    if (!RequireCore(env, api)) {
        return nullptr;
    }
    CoreText text(*api, api->version());
    if (text.get() == nullptr) {
        Throw(env, "ERR_DIARY_LIB", "diary_capi_version() 返回 NULL");
        return nullptr;
    }
    return MakeString(env, text.Copy());
}

// JS: open(path: string) -> number（句柄 id）
napi_value OpenJsImpl(napi_env env, napi_callback_info info) {
    std::string path;
    if (!ReadStringArg(env, info, 0, "path", path)) {
        return nullptr;
    }
    CoreApi *api = nullptr;
    if (!RequireCore(env, api)) {
        return nullptr;
    }
    void *handle = api->open(path.c_str());
    if (handle == nullptr) {
        // message 就是 diary_last_error() 的 JSON 原文：ArkTS 侧能从中还原 code/message/retryable。
        Throw(env, "ERR_DIARY_OPEN", TakeLastError(*api));
        return nullptr;
    }
    // 拿到 C 句柄后、JS 拿到 id 之前，任何一步抛错/失败（含 napi_create_uint32
    // 抛 C++ 异常）都由 `pending` 负责：先从登记表 Take 掉 id，再关闭 C 句柄。
    PendingHandle pending(*api, handle);
    const uint32_t id = Handles().Add(handle);
    pending.Adopt(id);
    napi_value out = nullptr;
    if (napi_create_uint32(env, id, &out) != napi_ok) {
        // 极端情况：JS 值都造不出来。表项与 C 句柄都由 pending 析构归还。
        Throw(env, "ERR_DIARY_CALL", "创建返回值失败");
        return nullptr;
    }
    pending.Dismiss();
    return out;
}

// JS: close(handle: number) -> void
napi_value CloseJsImpl(napi_env env, napi_callback_info info) {
    uint32_t id = 0;
    if (!ReadHandleArg(env, info, 0, id)) {
        return nullptr;
    }
    CoreApi *api = nullptr;
    if (!RequireCore(env, api)) {
        return nullptr;
    }
    void *handle = Handles().Take(id);
    if (handle == nullptr) {
        Throw(env, "ERR_DIARY_HANDLE", "句柄无效或已关闭（id=" + std::to_string(id) + "）");
        return nullptr;
    }
    const int rc = api->close(handle);
    if (rc != 0) {
        // 0 成功 / 1 空指针 / 2 无效或已关闭 / 3 桥面内部错误（见 C ABI 文档 §4.1）。
        // 走到这里说明登记表和 C 侧不一致（例如 C 侧自己判了无效），如实报出来。
        Throw(env, "ERR_DIARY_CLOSE",
              "diary_close 返回 " + std::to_string(rc) + "：" + TakeLastError(*api));
        return nullptr;
    }
    return nullptr;
}

// JS: call(handle: number, methodJson: string) -> string（信封 JSON 原文）
napi_value CallJsImpl(napi_env env, napi_callback_info info) {
    uint32_t id = 0;
    if (!ReadHandleArg(env, info, 0, id)) {
        return nullptr;
    }
    std::string request;
    if (!ReadStringArg(env, info, 1, "methodJson", request)) {
        return nullptr;
    }
    CoreApi *api = nullptr;
    if (!RequireCore(env, api)) {
        return nullptr;
    }
    // 句柄只在取指针时锁一下，核心调用期间不持有句柄表的锁（与 C ABI 同一口径）。
    // 这里不能 Take（调用后句柄还要用），只查。
    void *handle = Handles().Peek(id);
    if (handle == nullptr) {
        Throw(env, "ERR_DIARY_HANDLE", "句柄无效或已关闭（id=" + std::to_string(id) + "）");
        return nullptr;
    }
    CoreText text(*api, api->call(handle, request.c_str()));
    if (text.get() == nullptr) {
        // C ABI 文档：diary_call 只在内存分配失败时返回 NULL。
        Throw(env, "ERR_DIARY_CALL", "diary_call 返回 NULL：" + TakeLastError(*api));
        return nullptr;
    }
    // 复制后由 CoreText 析构归还：C 指针不进 JS，抛错路径也不漏。
    return MakeString(env, text.Copy());
}

// JS: lastError() -> string | null
napi_value LastErrorJsImpl(napi_env env, napi_callback_info info) {
    (void)info;
    CoreApi &api = Core();
    if (api.handle == nullptr || api.last_error == nullptr) {
        napi_value null_value = nullptr;
        napi_get_null(env, &null_value);
        return null_value;
    }
    CoreText text(api, api.last_error());
    if (text.get() == nullptr) {
        napi_value null_value = nullptr;
        napi_get_null(env, &null_value);
        return null_value;
    }
    return MakeString(env, text.Copy());
}

// JS: selfDir() -> string
napi_value SelfDirJsImpl(napi_env env, napi_callback_info info) {
    (void)info;
    return MakeString(env, SelfDir());
}

// JS: openHandleCount() -> number（排障 / 泄漏自检：正常应是 0）
napi_value OpenHandleCountJsImpl(napi_env env, napi_callback_info info) {
    (void)info;
    napi_value out = nullptr;
    napi_create_uint32(env, static_cast<uint32_t>(Handles().Size()), &out);
    return out;
}

// ---------------------------------------------------------------- 统一异常边界
//
// 下面的薄包装是**唯一**注册给 NAPI 的入口：函数体里的 `std::bad_alloc` / 其它
// C++ 异常都在这里变成 `ERR_DIARY_CALL` 的 JS 异常，不会 terminate 进程。

napi_value VersionJs(napi_env env, napi_callback_info info) noexcept {
    return Guarded(env, "version", [&] { return VersionJsImpl(env, info); });
}

napi_value OpenJs(napi_env env, napi_callback_info info) noexcept {
    return Guarded(env, "open", [&] { return OpenJsImpl(env, info); });
}

napi_value CloseJs(napi_env env, napi_callback_info info) noexcept {
    return Guarded(env, "close", [&] { return CloseJsImpl(env, info); });
}

napi_value CallJs(napi_env env, napi_callback_info info) noexcept {
    return Guarded(env, "call", [&] { return CallJsImpl(env, info); });
}

napi_value LastErrorJs(napi_env env, napi_callback_info info) noexcept {
    return Guarded(env, "lastError", [&] { return LastErrorJsImpl(env, info); });
}

napi_value SelfDirJs(napi_env env, napi_callback_info info) noexcept {
    return Guarded(env, "selfDir", [&] { return SelfDirJsImpl(env, info); });
}

napi_value OpenHandleCountJs(napi_env env, napi_callback_info info) noexcept {
    return Guarded(env, "openHandleCount", [&] { return OpenHandleCountJsImpl(env, info); });
}

napi_value InitImpl(napi_env env, napi_value exports) {
    napi_property_descriptor desc[] = {
        {"version", nullptr, VersionJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"open", nullptr, OpenJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"close", nullptr, CloseJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"call", nullptr, CallJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"lastError", nullptr, LastErrorJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"selfDir", nullptr, SelfDirJs, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"openHandleCount", nullptr, OpenHandleCountJs, nullptr, nullptr, nullptr, napi_default,
         nullptr},
    };
    napi_define_properties(env, exports, sizeof(desc) / sizeof(desc[0]), desc);
    return exports;
}

napi_value Init(napi_env env, napi_value exports) noexcept {
    return Guarded(env, "init", [&] { return InitImpl(env, exports); });
}

napi_module g_module = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "diary_napi",  // 必须与 libdiary_napi.so 去掉 lib/ 与 .so 后一致
    .nm_priv = nullptr,
    .reserved = {nullptr},
};

}  // namespace

// 定义在匿名 namespace 外的代价说明：这里**只有**模块注册这一个跨 TU 符号。
// 探针 §24.2 的坑是「声明在匿名 namespace、定义在全局作用域」——本文件是单 TU，
// 不存在那个问题；但为保险，所有跨 TU 的东西都只有下面这一个。
extern "C" __attribute__((constructor)) void RegisterDiaryNapiModule() {
    napi_module_register(&g_module);
}
