import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/core_wakeup.dart';

void main() {
  test('past due wakeups are delayed to avoid an immediate retry loop', () {
    final now = DateTime.utc(2026, 10, 16, 1);
    expect(
      CoreWakeupScheduler.delayUntil(
        now.subtract(const Duration(minutes: 1)),
        now,
      ),
      const Duration(seconds: 15),
    );
  });

  test('future due wakeups keep their requested delay', () {
    final now = DateTime.utc(2026, 10, 16, 1);
    expect(
      CoreWakeupScheduler.delayUntil(
        now.add(const Duration(minutes: 2)),
        now,
      ),
      const Duration(minutes: 2),
    );
  });

  test('wakeup log gets machine codes, never Dart runtime type names', () {
    // These are the values the diagnostics exporter keeps; the old
    // `error.runtimeType.toString()` produced names that were always redacted.
    expect(
      diagnosticErrorCode(const DiaryException(
        code: DiaryErrorCode.invalidArgument,
        message: 'bad input',
      )),
      'invalid_argument',
    );
    expect(
      diagnosticErrorCode(const DiaryException(
        code: DiaryErrorCode.pluginDenied,
        message: 'plugin denied',
      )),
      'plugin_denied',
    );
    expect(
      diagnosticErrorCode(PlatformException(code: 'ERR_DIARY_HANDLE')),
      'ERR_DIARY_HANDLE',
    );
    expect(diagnosticErrorCode(StateError('not a platform error')), 'unclassified_error');
    expect(diagnosticErrorCode(StateError('x')), isNot('StateError'));
  });

  _wakeupDesktopTests();
}

/// 桌面端必须**自己**把队列推起来。
///
/// 之前 `afterOpen` / `afterCoreMutation` 开头就是 `if (!Platform.isAndroid) return;`，
/// Windows 上排队的任务永远没人执行，界面一直停在「排队中」。
class _PumpApi extends MockDiaryApi {
  int runDueCalls = 0;
  String? lastTrigger;
  DateTime? scheduledWakeup;

  @override
  Future<DueRunSummary> runDueJobs({
    required int budgetMs,
    required String trigger,
  }) async {
    runDueCalls++;
    lastTrigger = trigger;
    return DueRunSummary(
      startedAt: DateTime.utc(2026, 11, 7),
      trigger: trigger,
      processed: 1,
      succeeded: 1,
      nextWakeup: scheduledWakeup,
    );
  }
}

void _wakeupDesktopTests() {
  test('桌面端 afterCoreMutation 会真的跑一轮队列', () async {
    final api = _PumpApi();
    await CoreWakeupScheduler.afterCoreMutation(api);
    // _pumpOnDesktop 是 fire-and-forget，让出一拍等它跑完。
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(api.runDueCalls, 1, reason: '桌面端不该像以前那样直接 return');
    expect(api.lastTrigger, 'foreground');
  });

  test('桌面端 afterOpen 也会跑一轮，且先看能力清单', () async {
    final api = _PumpApi();
    const snapshot = CoreSnapshot(
      coreInfo: CoreInfo(
        apiVersion: '1.1',
        dataSchemaVersion: 18,
        buildVersion: 'test',
        libraryId: 'lib-test',
        capabilities: {'jobs.runDue', 'jobs.nextWakeup'},
      ),
      recovery: RecoverySummary(),
    );
    await CoreWakeupScheduler.afterOpen(api, snapshot);
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(api.runDueCalls, 1);
    expect(api.lastTrigger, 'startup');
  });
}
