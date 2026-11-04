/// 一个可用的模型。字段只保留「列出模型」真正能给出的东西。
class ProviderModel {
  const ProviderModel({required this.id, this.ownedBy});

  /// 模型名（原样来自远端）。
  final String id;

  /// OpenAI 兼容响应里的可选字段 `owned_by`；没有就是 null。
  final String? ownedBy;

  @override
  bool operator ==(Object other) =>
      other is ProviderModel && other.id == id && other.ownedBy == ownedBy;

  @override
  int get hashCode => Object.hash(id, ownedBy);

  @override
  String toString() => 'ProviderModel($id)';
}

/// 检测某个服务能用哪些模型（`GET {endpoint}/models`）。
///
/// 单开一个接口而不是加在 `DiaryApi` 上：`contract_surface_test` 不允许契约之外的
/// 方法混进 `DiaryApi`，而这条是界面为了「模型名不用手填」加的便利能力。
///
/// **只有显式调用才发网络请求**——打开资料库、列出服务、保存配置都不会顺手探一次。
abstract interface class ProviderModelPort {
  Future<List<ProviderModel>> listProviderModels({required String providerId});
}
