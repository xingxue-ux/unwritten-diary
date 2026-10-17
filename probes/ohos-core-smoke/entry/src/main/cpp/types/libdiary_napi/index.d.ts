// libdiary_napi.so（探针用，非产品 ABI）的类型声明。
// 两个函数都返回 **JSON 字符串**——ArkTS 侧只负责把它打到 hilog，不做业务解析。

/** 本 shim 自己被 dlopen 出来的目录（排障用；daemon 的 selfDir）。 */
export const selfDir: () => string;

/**
 * 在应用进程里 dlopen `libdiary_core_smoke.so`，调用它做「建库 + 迁移 + 写入 + 读回」。
 *
 * @param dir 沙箱里放库文件的目录（filesDir 或 databaseDir）
 * @returns JSON 字符串：{ dlopen_ok, dlopen_used, dlopen_attempts[], dlsym_ok, abi,
 *                         core_loaded_at, core: "<Rust 返回的 JSON 原文>" }
 */
export const runSmoke: (dir: string) => string;

/**
 * 把 HAP 里的核心库复制到给定的**可写**沙箱目录（el2），再从那里 dlopen。
 * 用来回答「将来能不能把原生库（模型运行时/插件）动态放到沙箱里加载」。
 *
 * @param dir 可写目录（例如 filesDir）
 * @returns JSON 字符串：{ src, dst, copy_ok, copy_bytes, dlopen_ok, dlopen_err, abi }
 */
export const dlopenFromDir: (dir: string) => string;

/**
 * 在沙箱里 dlopen HAP 自带的 `libonnxruntime.so`（走 ONNX Runtime 的 C API，
 * 不经过 `ort` crate），取版本号、试 `GetApi(17)/GetApi(16)`、建 OrtSession、
 * 跑一次 tiny_add.onnx 推理。
 *
 * @param dir 放着 tiny_add.onnx 的沙箱目录（例如 filesDir）
 * @returns JSON 字符串（见 README「ORT 一节」的字段说明）
 */
export const ortProbe: (dir: string) => string;
/**
 * 走**产品真实调用路径**：Rust 侧 `ort` crate 的 load-dynamic（`ort::init_from` + `commit()`）。
 * 与 ortProbe 的区别：ortProbe 用 ORT 的 C API（回答「沙箱能不能 dlopen ORT」），
 * 这个用我们自己的 crate（回答「我们这版 crate 能不能用这个 ORT」）。
 */
export const ortCrateProbe: (modelDir: string) => string;
