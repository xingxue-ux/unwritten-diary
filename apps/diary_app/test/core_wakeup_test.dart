import 'package:diary_api/diary_api.dart';
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
}
