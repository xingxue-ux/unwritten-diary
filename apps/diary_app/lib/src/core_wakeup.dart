import 'dart:convert';
import 'dart:io';
import 'dart:ui' show DartPluginRegistrant;

import 'package:diary_api/diary_api.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:path/path.dart' as path;
import 'package:path_provider/path_provider.dart';
import 'package:workmanager/workmanager.dart';

import 'app_services.dart';

/// Android's persistent worker owns the wakeup, not an open Flutter screen.
/// WorkManager can delay execution; the core remains the authority on what is due.
class CoreWakeupScheduler {
  CoreWakeupScheduler._();

  static const _taskName = 'diary-core-run-due';
  static const _periodicName = 'diary-core-periodic';
  static const _oneOffName = 'diary-core-wakeup';
  static const _budgetMs = 20000;
  static const _minimumDelay = Duration(seconds: 15);

  static Future<void> initialize() async {
    if (!Platform.isAndroid) return;
    await Workmanager().initialize(coreWakeupDispatcher);
    // Older builds used two alternating names, allowing concurrent workers.
    await Workmanager().cancelByUniqueName('diary-core-wakeup-a');
    await Workmanager().cancelByUniqueName('diary-core-wakeup-b');
    // A periodic safety net catches jobs created by system share/import entry
    // points before they have had a chance to schedule their exact next wake.
    await Workmanager().registerPeriodicTask(
      _periodicName,
      _taskName,
      frequency: const Duration(minutes: 15),
      existingWorkPolicy: ExistingPeriodicWorkPolicy.keep,
      inputData: const {},
    );
  }

  /// Call after opening the real core. This also reclaims interrupted jobs.
  static Future<void> afterOpen(DiaryApi api, CoreSnapshot opened) async {
    if (!Platform.isAndroid || !_hasJobs(opened)) return;
    try {
      final result = await api.runDueJobs(
        budgetMs: _budgetMs,
        trigger: 'startup',
      );
      await _scheduleNext(result.nextWakeup);
      await _logQuietly({
        'trigger': 'startup',
        'observedAt': DateTime.now().toUtc().toIso8601String(),
        ..._summary(result),
      });
    } on Object catch (error) {
      await _logQuietly({
        'trigger': 'startup',
        'observedAt': DateTime.now().toUtc().toIso8601String(),
        'error': diagnosticErrorCode(error),
      });
    }
  }

  /// Re-read the core clock after a foreground mutation schedules new work.
  static Future<void> afterCoreMutation(DiaryApi api) async {
    if (!Platform.isAndroid) return;
    try {
      await _scheduleNext(await api.nextWakeup());
    } on Object catch (error) {
      await _logQuietly({
        'trigger': 'foreground-schedule',
        'observedAt': DateTime.now().toUtc().toIso8601String(),
        'error': diagnosticErrorCode(error),
      });
    }
  }

  static bool _hasJobs(CoreSnapshot opened) =>
      opened.coreInfo.capabilities.contains('jobs.runDue') &&
      opened.coreInfo.capabilities.contains('jobs.nextWakeup');

  @visibleForTesting
  static Duration delayUntil(DateTime nextWakeup, DateTime now) {
    final remaining = nextWakeup.toUtc().difference(now.toUtc());
    return remaining < _minimumDelay ? _minimumDelay : remaining;
  }

  static Future<void> _scheduleNext(DateTime? nextWakeup) async {
    if (nextWakeup == null) return;
    final now = DateTime.now().toUtc();
    final expected = nextWakeup.toUtc();
    await Workmanager().registerOneOffTask(
      _oneOffName,
      _taskName,
      initialDelay: delayUntil(expected, now),
      // The current worker may still be running while it schedules its next
      // wake. Append behind it; replacing this unique work could cancel it.
      existingWorkPolicy: ExistingWorkPolicy.append,
      inputData: {'expectedAt': expected.toIso8601String()},
    );
  }

  static Map<String, Object?> _summary(DueRunSummary result) => {
    'processed': result.processed,
    'succeeded': result.succeeded,
    'failed': result.failed,
    'retrying': result.retrying,
    'backfilled': result.backfilled,
    'recovered': result.recovered,
    'dueBacklog': result.dueBacklog,
    'waitingDependency': result.waitingDependency,
    'dependencyFailed': result.dependencyFailed,
    'nextWakeup': result.nextWakeup?.toUtc().toIso8601String(),
  };

  static Future<void> _writeDiagnostic(Map<String, Object?> entry) async {
    final support = await getApplicationSupportDirectory();
    final directory = Directory(path.join(support.path, 'unwritten_diary'));
    await directory.create(recursive: true);
    final file = File(path.join(directory.path, 'wakeup-diagnostics.jsonl'));
    await file.writeAsString(
      '${jsonEncode(entry)}\n',
      mode: FileMode.append,
      flush: true,
    );
  }

  static Future<void> _logQuietly(Map<String, Object?> entry) async {
    try {
      await _writeDiagnostic(entry);
    } on Object {
      // A full disk or inaccessible log must not make a completed core run
      // look like failed work to WorkManager.
    }
  }

  static Future<bool> _runFromWorker(
    String taskName,
    Map<String, dynamic>? inputData,
  ) async {
    if (taskName != _taskName) return true;
    WidgetsFlutterBinding.ensureInitialized();
    DartPluginRegistrant.ensureInitialized();
    final observedAt = DateTime.now().toUtc();
    DiaryApi? api;
    try {
      api = await createProductionDiaryApi();
      final opened = await api.open();
      if (!_hasJobs(opened)) {
        await _logQuietly({
          'trigger': 'wake',
          'observedAt': observedAt.toIso8601String(),
          'error': 'installed_core_missing_job_capabilities',
        });
        return true;
      }
      final result = await api.runDueJobs(budgetMs: _budgetMs, trigger: 'wake');
      await _scheduleNext(result.nextWakeup);
      await _logQuietly({
        'trigger': 'wake',
        'observedAt': observedAt.toIso8601String(),
        'expectedAt': inputData?['expectedAt'],
        'completedAt': DateTime.now().toUtc().toIso8601String(),
        ..._summary(result),
      });
      return true;
    } on Object catch (error) {
      await _logQuietly({
        'trigger': 'wake',
        'observedAt': observedAt.toIso8601String(),
        'expectedAt': inputData?['expectedAt'],
        'error': diagnosticErrorCode(error),
      });
      // Version/capability mismatches cannot heal through WorkManager retry.
      return error is DiaryException &&
          {
            DiaryErrorCode.invalidArgument,
            DiaryErrorCode.invalidState,
            DiaryErrorCode.notFound,
            DiaryErrorCode.unsupportedFormat,
          }.contains(error.code);
    } finally {
      try {
        await api?.close();
      } on Object {
        // The next worker opens a fresh session regardless.
      }
    }
  }
}

@pragma('vm:entry-point')
void coreWakeupDispatcher() {
  Workmanager().executeTask(CoreWakeupScheduler._runFromWorker);
}

/// Stable machine code for the wakeup diagnostics log.
///
/// The diagnostics bundle only keeps values from an exact machine-code
/// whitelist. `error.runtimeType.toString()` produces `DiaryException` /
/// `PlatformException`, which are Dart type names — never in that whitelist —
/// so every real failure was logged as `<redacted>` and the bundle could not
/// answer "which error was it?". Log the wire code instead:
///
/// * `DiaryException` -> `code.wire` (contract §7 vocabulary);
/// * `PlatformException` -> its `code`, which is `ERR_DIARY_*` for the NAPI
///   layer (unknown plugin codes are still redacted by the exporter);
/// * anything else -> a fixed literal, never a runtime type name.
@visibleForTesting
String diagnosticErrorCode(Object error) {
  if (error is DiaryException) return error.code.wire;
  if (error is PlatformException) return error.code;
  return 'unclassified_error';
}
