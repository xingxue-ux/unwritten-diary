/// 「不写日记」两层共享的 Dart 类型与抽象接口。
///
/// 依据 `docs/development/01-共享接口契约.md`（v1 已冻结）。
/// 页面只依赖这里的抽象接口；实际实现由桥接层与 Mock 各自提供。
///
/// 本包刻意不依赖 Flutter，也不做 JSON 序列化：传输编码由桥接层负责。
library;

export 'src/diary_api.dart';
export 'src/events.dart';
export 'src/models_content.dart';
export 'src/models_diary.dart';
export 'src/models_transport.dart';
export 'src/platform_host.dart';
export 'src/values.dart';

/// 契约的主次版本。任何破坏兼容的改动都要提升主版本。
const String kContractVersion = 'v1';

/// 与 [kContractVersion] 对应的 apiVersion 字面量。
const String kApiVersion = '1.1';

/// 支持的 apiVersion 范围，供插件清单与兼容性检查使用。
const String kSupportedApiRange = '>=1.0.0 <2.0.0';