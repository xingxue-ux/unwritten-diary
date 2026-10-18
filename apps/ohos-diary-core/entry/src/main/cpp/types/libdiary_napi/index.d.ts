/**
 * `libdiary_napi.so` 的类型声明（C++ NAPI shim，源码见 `entry/src/main/cpp/napi_shim.cpp`）。
 *
 * 接口只有 7 个函数，全部是**同步**的：
 *
 * - `version()` / `selfDir()` / `lastError()` / `openHandleCount()` 是诊断面；
 * - `open()` / `close()` / `call()` 是数据面；
 * - `call()` 的入参与返回值都是**一整条 JSON 字符串**，本层不拆不改（一进一出）。
 *
 * 错误口径：只在「调用没能发生」时抛 JS `Error`，`code` 是 `ERR_DIARY_*` 之一：
 * `ERR_DIARY_LIB`（核心 .so 加载失败）、`ERR_DIARY_ARGS`（参数类型不对）、
 * `ERR_DIARY_HANDLE`（句柄 id 未知或已关闭）、`ERR_DIARY_OPEN`（`open` 返回 NULL，
 * message 是 `diary_last_error()` 的 JSON 原文）、`ERR_DIARY_CALL`（`diary_call`
 * 返回 NULL）。**业务错误不抛**：`call()` 照常返回 `ok:false` 的信封字符串。
 *
 * 句柄是 shim 自己发的 `uint32` id（不是 C 指针、不是 bigint）；只在本进程有效。
 */
export interface DiaryNativeModule {
  /** `diary_capi_version()` 原文，形如 `diary_capi/0.1.0 apiVersion=1.0`。 */
  version(): string;

  /** 打开（或创建）资料库；`path` 是**文件路径**（例如 `<filesDir>/library.sqlite`）。 */
  open(path: string): number;

  /** 关闭句柄；重复关闭会抛 `ERR_DIARY_HANDLE`。 */
  close(handle: number): void;

  /** 调一个方法，返回信封 JSON 原文（成功 `ok:true` / 失败 `ok:false`，都带 `apiVersion`）。 */
  call(handle: number, methodJson: string): string;

  /** 本线程最近一条错误（JSON，与信封里的 `error` 同形）；没有则 `null`。 */
  lastError(): string | null;

  /** 本 shim 被加载的目录（排障用；正常是 `.../libs/arm64`）。 */
  selfDir(): string;

  /** 还没关闭的句柄数（排障 / 泄漏自检；全部关闭后应为 0）。 */
  openHandleCount(): number;
}

declare const diaryNapi: DiaryNativeModule;
export default diaryNapi;
