import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

/// 「整理时机」的读写层：自动整理开关、计划时间、补充候选开关。
///
/// 数据来源是核心真实的 `organize_settings`（桥接 `organizeSettings` /
/// `updateOrganizeSettings`），**不是契约的 `settings.*`**——后者覆盖全部可编辑
/// 设置（含日记时区等平台层字段），Windows 桥这一层没有实现它。界面要改的三项
/// 都在 `organize_settings` 里，所以这段能真的落库。
///
/// 纪律：
/// - 每次写都带 `expectedRevision`；冲突时**重读再重试一次**，不拿旧 revision 硬撞；
/// - 写失败**不改本地状态**，只把原因放进 `message`；
/// - 所有写操作共用 `saving`，连点开关不会交叉两次写。
class ScheduleSettingsController extends ChangeNotifier {
  /// `port` 为 null 表示当前宿主没有实现 `organize_settings` 这一块
  /// （契约里它归 `settings.*`，Windows 桥还没接）。这时界面只显示原因，
  /// 不摆一组点了没用的开关。
  ScheduleSettingsController(this._port);

  final OrganizeSchedulePort? _port;

  OrganizeSchedule? schedule;
  bool loading = false;
  bool saving = false;
  String? message;

  bool get enabled => schedule?.enabled ?? false;
  String get autoOrganizeTime => schedule?.autoOrganizeTime ?? '22:30';
  bool get supplementOnNewMaterials =>
      schedule?.supplementOnNewMaterials ?? true;

  Future<void> load() async {
    if (loading) return;
    loading = true;
    message = null;
    notifyListeners();
    final port = _port;
    if (port == null) {
      loading = false;
      message = '当前核心没有开放整理时机的读写，这里的改动不会保存。';
      notifyListeners();
      return;
    }
    try {
      schedule = await port.organizeSchedule();
      if (schedule != null && !schedule!.enabled) {
        message = '自动整理默认是关的：打开后才会在计划时间整理已保存的材料。';
      }
    } on UnsupportedError {
      message = '当前核心没有开放整理时机的读写，这里的改动不会保存。';
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '整理时机暂时没有读出来，请稍后重试。';
    } finally {
      loading = false;
      notifyListeners();
    }
  }

  /// 把 `patch` 应用到当前值上并写回。返回 `true` 表示核心已接受。
  Future<bool> _apply(
    OrganizeSchedule Function(OrganizeSchedule current) patch, {
    required String success,
    required String failure,
  }) async {
    final port = _port;
    final current = schedule;
    if (port == null || current == null || saving) return false;
    final next = patch(current);
    if (next == current) return true;

    saving = true;
    message = '正在保存…';
    notifyListeners();
    var ok = false;
    try {
      schedule = await port.updateOrganizeSchedule(
        schedule: next,
        expectedRevision: current.revision,
      );
      message = success;
      ok = true;
    } on DiaryException catch (error) {
      if (error.code != DiaryErrorCode.revisionConflict) {
        message = error.message;
        return false;
      }
      // 别处改过设置：重读一次，再按新 revision 重试一遍。
      try {
        final latest = await port.organizeSchedule();
        schedule = await port.updateOrganizeSchedule(
          schedule: patch(latest),
          expectedRevision: latest.revision,
        );
        message = success;
        ok = true;
      } on DiaryException catch (retryError) {
        message = retryError.message;
      } on Object {
        message = failure;
      }
    } on UnsupportedError {
      message = '当前核心没有开放整理时机的写入，改动没有保存。';
    } on Object {
      message = failure;
    } finally {
      saving = false;
      notifyListeners();
    }
    return ok;
  }

  Future<bool> setEnabled(bool value) => _apply(
    (current) => current.copyWith(enabled: value),
    success: value ? '自动整理已开启。' : '自动整理已关闭。',
    failure: '自动整理的开关没有保存，原设置保持不变。',
  );

  Future<bool> setSupplementOnNewMaterials(bool value) => _apply(
    (current) => current.copyWith(supplementOnNewMaterials: value),
    success: value
        ? '新材料来了会自动补一版候选。'
        : '新材料来了不会自动补做，只在你点「再来一版」时整理。',
    failure: '补充候选的开关没有保存，原设置保持不变。',
  );

  /// `time` 可以是 `9:5` / `09:05` / `905` 这些写法，统一规范成 `HH:mm`。
  Future<bool> setAutoOrganizeTime(String time) {
    final normalized = normalizeClock(time);
    if (normalized == null) {
      message = '时间格式不对：需要 00:00–23:59。';
      notifyListeners();
      return Future.value(false);
    }
    return _apply(
      (current) => current.copyWith(autoOrganizeTime: normalized),
      success: '计划时间已更新。',
      failure: '计划时间没有保存，原设置保持不变。',
    );
  }

  /// 把用户输入的字符串规范成 `HH:mm`；不合法返回 null。
  ///
  /// 接受 `9:5`、`09:05`、`2359`、`930`——手动敲的时候没人记得补零，
  /// 界面不该因此拒绝。
  static String? normalizeClock(String raw) {
    final text = raw.trim();
    if (text.isEmpty) return null;
    String hourPart;
    String minutePart;
    if (text.contains(':')) {
      final parts = text.split(':');
      if (parts.length != 2) return null;
      hourPart = parts[0].trim();
      minutePart = parts[1].trim();
    } else if (RegExp(r'^\d{3,4}$').hasMatch(text)) {
      hourPart = text.substring(0, text.length - 2);
      minutePart = text.substring(text.length - 2);
    } else {
      return null;
    }
    if (hourPart.isEmpty || minutePart.isEmpty) return null;
    if (!RegExp(r'^\d{1,2}$').hasMatch(hourPart)) return null;
    if (!RegExp(r'^\d{1,2}$').hasMatch(minutePart)) return null;
    final hour = int.parse(hourPart);
    final minute = int.parse(minutePart);
    if (hour > 23 || minute > 59) return null;
    return '${hour.toString().padLeft(2, '0')}:'
        '${minute.toString().padLeft(2, '0')}';
  }
}