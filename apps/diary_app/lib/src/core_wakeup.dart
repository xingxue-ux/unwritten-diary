import 'dart:convert';
import 'dart:io';
import 'dart:ui' show DartPluginRegistrant;

import 'package:diary_api/diary_api.dart';
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
  static const _oneOffNames = ['diary-core-wakeup-a', 'diary-core-wakeup-b'];
  static const _budgetMs = 20000;

  static Future<void> initialize() async {
    if (!Platform.isAndroid) return;
    await Workmanager().initialize(coreWakeupDispatcher);
    // A periodic safety net catches jobs created by system share/import entry
    // points before they have had a chance to schedule their exact next wake.
    await Workmanager().registerPeriodicTask(
      _periodicName,
      _taskName,
      frequency: const Duration(minutes: 15),
      existingWorkPolicy: ExistingPeriodicWorkPolicy.keep,
      inputData: const {'slot': -1},
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
      await _scheduleNext(result.nextWakeup, fromSlot: 1);
      await _logQuietly({
        'trigger': 'startup',
        'observedAt': DateTime.now().toUtc().toIso8601String(),
        ..._summary(result),
      });
    } on Object catch (error) {
      await _logQuietly({
        'trigger': 'startup',
        'observedAt': DateTime.now().toUtc().toIso8601String(),
        'error': error.runtimeType.toString(),
      });
    }
  }

  /// Re-read the core clock after a foreground mutation schedules new work.
  static Future<void> afterCoreMutation(DiaryApi api) async {
    if (!Platform.isAndroid) return;
    try {
      await _scheduleNext(await api.nextWakeup(), fromSlot: 1);
    } on Object catch (error) {
      await _logQuietly({
        'trigger': 'foreground-schedule',
        'observedAt': DateTime.now().toUtc().toIso8601String(),
        'error': error.runtimeType.toString(),
      });
    }
  }

  static bool _hasJobs(CoreSnapshot opened) =>
      opened.coreInfo.capabilities.contains('jobs.runDue') &&
      opened.coreInfo.capabilities.contains('jobs.nextWakeup');

  static Future<void> _scheduleNext(
    DateTime? nextWakeup, {
    required int fromSlot,
  }) async {
    if (nextWakeup == null) return;
    final slot = (fromSlot + 1) % _oneOffNames.length;
    final now = DateTime.now().toUtc();
    final expected = nextWakeup.toUtc();
    final remaining = expected.difference(now);
    await Workmanager().registerOneOffTask(
      _oneOffNames[slot],
      _taskName,
      initialDelay: remaining.isNegative ? Duration.zero : remaining,
      existingWorkPolicy: ExistingWorkPolicy.replace,
      inputData: {'slot': slot, 'expectedAt': expected.toIso8601String()},
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
    final fromSlot = inputData?['slot'] is int ? inputData!['slot'] as int : -1;
    DiaryApi? api;
    try {
      api = await createProductionDiaryApi();
      final opened = await api.open();
      if (!_hasJobs(opened)) {
        throw StateError('The installed core does not expose job execution.');
      }
      final result = await api.runDueJobs(budgetMs: _budgetMs, trigger: 'wake');
      await _scheduleNext(result.nextWakeup, fromSlot: fromSlot);
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
        'error': error.runtimeType.toString(),
      });
      // WorkManager retries transient startup, storage, and core failures.
      return false;
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
