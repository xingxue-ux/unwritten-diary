// libdiary_bge.so（探针用，非产品 ABI）的类型声明。
// 两个函数都返回 **JSON 字符串**——ArkTS 侧只负责把它打到 hilog，不做业务解析。

/** 本 shim 自己被 dlopen 出来的目录（排障用；也是 libonnxruntime.so 的所在目录）。 */
export const selfDir: () => string;

/**
 * 在应用进程里 dlopen `libdiary_bge_core.so`，用**产品同一条路径**
 * （`diary_core::embedding::load` + `Embedder::embed_document`，ORT load-dynamic、
 * CLS pooling、无查询前缀、512 维）对探针内置语料求向量。
 *
 * @param modelDir 沙箱里放 `model_quantized.onnx` 与 `tokenizer.json` 的目录（例如 filesDir）
 * @returns JSON 字符串：{ dlopen_ok, dlopen_used, dlopen_attempts[], dlsym_ok, abi,
 *                         core_loaded_at, params, core: "<Rust 返回的 JSON 原文>" }
 *          —— `core` 里的字段见 probes/ohos-bge-smoke/README.md
 */
export const runBge: (modelDir: string) => string;