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
}
