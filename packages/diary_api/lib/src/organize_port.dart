import 'models_transport.dart';

/// 「整理时机」的可选能力，**不属于共享契约**。
///
/// 契约里这块归 `settings.get` / `settings.update`（那一对覆盖全部可编辑设置，
/// 含日记时区等平台字段）。Windows 桥没有实现 `settings.*`，但有核心真实导出的
/// `organize_settings`；把这个差异做成**独立接口**，而不是往 [DiaryApi] 上加方法——
/// `test/contract_surface_test.dart` 会挡住「接口上多出契约之外的方法」，
/// 那条约束是对的：宿主只实现了契约的一部分时，不该让 `DiaryApi` 变形。
///
/// 拿不到这个能力时（`api is! OrganizeSchedulePort`），界面应如实说明
/// 「当前核心没有开放整理时机的读写」，而不是显示一组点了没用的开关。
abstract interface class OrganizeSchedulePort {
  /// 读当前值。
  Future<OrganizeSchedule> organizeSchedule();

  /// 写回。核心要求整份对象；实现方负责保留界面没展示的字段。
  /// `expectedRevision` 不匹配抛 `revisionConflict`，核心零改动。
  Future<OrganizeSchedule> updateOrganizeSchedule({
    required OrganizeSchedule schedule,
    required int expectedRevision,
  });
}
