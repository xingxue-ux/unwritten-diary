//! `diary_bridge`：对 Flutter 的薄接口。
//!
//! M0 阶段这里只放足以验证调用链的最小 API：同步调用、异步调用，以及契约里
//! 的版本信息结构。**没有真实存储、没有索引、没有模型**，那些属于 B1 起。
//!
//! 生成的 Dart 绑定在 `packages/diary_bridge/lib/src/rust/`，由
//! `flutter_rust_bridge_codegen generate` 产生，禁止手改。

pub mod api;

mod frb_generated;