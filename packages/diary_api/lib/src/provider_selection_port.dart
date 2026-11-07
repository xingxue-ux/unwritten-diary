import 'models_diary.dart';

/// 指定「用哪个服务整理日记」。
///
/// 单开一个接口而不是加在 `DiaryApi` 上：`contract_surface_test` 不允许契约之外的
/// 方法混进 `DiaryApi`，而这条是界面为了「配了多个服务时到底用哪个」加的。
///
/// 语义是**单选**：选中的那条启用，其余能承担 `generate_text` 的在线服务一并停用。
/// 这样核心的生成闸门只会挑到用户指定的那一个，不用在每个调用点各记一份「想用哪个」。
abstract interface class ProviderSelectionPort {
  /// 选中某个服务用于整理。只接受在线（HTTP）且声明了 `generate_text` 的服务；
  /// 传本机引擎会得到 `invalid_argument`（它不做文本生成）。
  Future<Provider> selectGenerationProvider({required String providerId});
}
