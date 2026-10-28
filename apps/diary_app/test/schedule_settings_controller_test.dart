import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/schedule_settings_controller.dart';

/// 假核心：`organize_settings` 的最小实现，可以指定第一次写是否报冲突。
class ScheduleApi extends MockDiaryApi {
  ScheduleApi({this.conflictOnce = false});

  bool conflictOnce;
  OrganizeSchedule current = const OrganizeSchedule(
    enabled: false,
    autoOrganizeTime: '22:30',
    supplementOnNewMaterials: true,
    revision: 3,
  );
  int writes = 0;

  @override
  Future<OrganizeSchedule> organizeSchedule() async => current;

  @override
  Future<OrganizeSchedule> updateOrganizeSchedule({
    required OrganizeSchedule schedule,
    required int expectedRevision,
  }) async {
    writes++;
    if (conflictOnce) {
      conflictOnce = false;
      // 模拟「别人刚改过」：核心拒绝，并且当前值已经推进到 revision+1。
      current = OrganizeSchedule(
        enabled: true,
        autoOrganizeTime: current.autoOrganizeTime,
        supplementOnNewMaterials: current.supplementOnNewMaterials,
        revision: current.revision + 1,
      );
      throw const DiaryException(
        code: DiaryErrorCode.revisionConflict,
        message: '设置已被改动，请重新读取后再试。',
      );
    }
    if (expectedRevision != current.revision) {
      throw const DiaryException(
        code: DiaryErrorCode.revisionConflict,
        message: '设置已被改动，请重新读取后再试。',
      );
    }
    current = OrganizeSchedule(
      enabled: schedule.enabled,
      autoOrganizeTime: schedule.autoOrganizeTime,
      supplementOnNewMaterials: schedule.supplementOnNewMaterials,
      revision: current.revision + 1,
    );
    return current;
  }
}

void main() {
  group('ScheduleSettingsController', () {
    test('load 读的是核心真实值，不是界面默认值', () async {
      final api = ScheduleApi();
      api.current = const OrganizeSchedule(
        enabled: true,
        autoOrganizeTime: '07:15',
        supplementOnNewMaterials: false,
        revision: 9,
      );
      final controller = ScheduleSettingsController(api);
      await controller.load();
      expect(controller.enabled, isTrue);
      expect(controller.autoOrganizeTime, '07:15');
      expect(controller.supplementOnNewMaterials, isFalse);
    });

    test('关着的时候如实说「默认是关的」，不假装会整理', () async {
      final controller = ScheduleSettingsController(ScheduleApi());
      await controller.load();
      expect(controller.message, contains('默认是关的'));
    });

    test('打开开关会写回核心，revision 前进', () async {
      final api = ScheduleApi();
      final controller = ScheduleSettingsController(api);
      await controller.load();
      expect(controller.enabled, isFalse);
      expect(await controller.setEnabled(true), isTrue);
      expect(controller.enabled, isTrue);
      expect(api.current.revision, 4);
      expect(controller.message, '自动整理已开启。');
    });

    test('冲突时重读再重试一次，最终落库', () async {
      final api = ScheduleApi(conflictOnce: true);
      final controller = ScheduleSettingsController(api);
      await controller.load();
      expect(await controller.setEnabled(true), isTrue);
      expect(api.writes, 2);
      expect(controller.enabled, isTrue);
      expect(controller.message, '自动整理已开启。');
    });

    test('写失败不改本地值，只给原因', () async {
      final api = _AlwaysFailApi();
      final controller = ScheduleSettingsController(api);
      await controller.load();
      expect(await controller.setEnabled(true), isFalse);
      expect(controller.enabled, isFalse);
      // 报的是核心给的原话，不是一句笼统的「没保存」——用户要知道为什么被拒。
      expect(controller.message, '核心拒绝了这次修改。');
    });

    test('时间格式：补零、拒绝越界', () {
      expect(ScheduleSettingsController.normalizeClock('9:5'), '09:05');
      expect(ScheduleSettingsController.normalizeClock('09:05'), '09:05');
      expect(ScheduleSettingsController.normalizeClock('2359'), '23:59');
      expect(ScheduleSettingsController.normalizeClock('930'), '09:30');
      expect(ScheduleSettingsController.normalizeClock('24:00'), isNull);
      expect(ScheduleSettingsController.normalizeClock('12:60'), isNull);
      expect(ScheduleSettingsController.normalizeClock('abc'), isNull);
      expect(ScheduleSettingsController.normalizeClock(''), isNull);
    });

    test('时间写回前先规范化', () async {
      final api = ScheduleApi();
      final controller = ScheduleSettingsController(api);
      await controller.load();
      await controller.setAutoOrganizeTime('7:5');
      expect(api.current.autoOrganizeTime, '07:05');
    });
  });
}

class _AlwaysFailApi extends ScheduleApi {
  @override
  Future<OrganizeSchedule> updateOrganizeSchedule({
    required OrganizeSchedule schedule,
    required int expectedRevision,
  }) async {
    throw const DiaryException(
      code: DiaryErrorCode.invalidArgument,
      message: '核心拒绝了这次修改。',
    );
  }
}