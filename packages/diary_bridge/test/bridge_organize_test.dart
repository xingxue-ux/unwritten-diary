import 'dart:io';

import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

import 'expected_schema.dart';

/// B4e（issue #92）：整理与自动化的桥接端到端——**只经桥接会话**走真核心 + 真 SQLite。
///
/// 覆盖：模板清单（三类内置、参数范围与默认值）、设置默认**关**、校验与乐观锁、
/// 状态里「什么时候会跑 / 会跑哪些材料」、以及**未开启时驱动一整轮不产出任何版本**。
///
/// 运行前先构建宿主产物：
///   cargo build --release            （仓库根目录）
///   dart test                        （本包目录）
///
/// **不联网、不需要任何服务**：这一轮里没有可用的远端服务，配置闸门在领取前就把
/// 任务停在「等待配置」，不会发出请求（`providerCode` 就是 `provider_not_configured`）。
void main() {
  late Directory workDir;
  late BridgeSession session;

  setUpAll(() async {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(File(path).existsSync(), isTrue,
        reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release');
    await RustLib.init(externalLibrary: ExternalLibrary.open(path));

    workDir = Directory.systemTemp.createTempSync('diary_bridge_organize_test');
    session = await BridgeSession.open(
      libraryPath: '${workDir.path}/library.sqlite',
    );
  });

  tearDownAll(() async {
    session.dispose();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删除失败不该让整个测试变成失败；临时目录留给系统清理。
    }
  });

  /// 造一条「昨天」的记录（本地时区东八区），返回它的 dayKey。
  Future<String> capture(String text, String operationId) async {
    final at = DateTime.now().toUtc().subtract(const Duration(days: 1));
    final draft = await session.createDraft(
      occurredAt: at,
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: '$operationId-create',
    );
    await session.saveDraft(
      captureId: draft.id,
      text: text,
      expectedRevision: draft.revision,
      operationId: operationId,
    );
    await session.commit(
      captureId: draft.id,
      expectedRevision: draft.revision + 1,
      operationId: '$operationId-commit',
    );
    return draft.dayKey;
  }

  test('能力与默认值：schema 到 v11，整理默认关', () async {
    final info = await session.info();
    expect(info.dataSchemaVersion, expectedDataSchemaVersion);
    // 整理是桥接自己的入口，不在契约能力清单里（清单只列契约方法名）——
    // 但能力清单本身照旧如实。
    expect(info.capabilities, contains('diary.generate'));

    final settings = await session.organizeSettings();
    expect(settings.enabled, isFalse, reason: '自动整理必须默认关');
    expect(settings.templateId, 'merge_by_theme');
    expect(settings.templateParams, isEmpty);
    expect(settings.autoOrganizeTime, '22:30');
    expect(settings.supplementOnNewMaterials, isTrue);
    expect(settings.revision, 1);
    // 上限的默认值来自核心常量：这里只钉住「有一组明确的上限」。
    expect(settings.maxMaterials, 50);
    expect(settings.maxChars, 20000);
    expect(settings.maxCallsPerRun, 8);
    expect(settings.maxCallsPerDay, 30);
    expect(settings.maxFailureRate, 0.5);

    final templates = await session.organizeTemplates();
    expect(templates.map((t) => t.id), ['merge_by_theme', 'proofread', 'compress']);
    for (final template in templates) {
      expect(template.source, 'builtin');
      expect(template.pluginId, isNull);
      expect(template.rule, isNotEmpty);
      expect(template.params, isNotEmpty);
    }
    final compress = templates.firstWhere((t) => t.id == 'compress');
    final target = compress.params.firstWhere((p) => p.key == 'targetChars');
    expect(target.kind, 'integer');
    expect(target.minimum, 50);
    expect(target.maximum, 2000);
    expect(target.defaultValue, '300');
  });

  test('校验与乐观锁：未知模板、越界参数、过期 revision 都零改动', () async {
    final current = await session.organizeSettings();

    // 未知模板 → not_found。
    await expectLater(
      session.updateOrganizeSettings(
        settings: _copy(
          current,
          templateId: 'plugin_style_x',
        ),
        expectedRevision: current.revision,
      ),
      throwsA(isA<BridgeError>().having((e) => e.code, 'code', 'not_found')),
    );

    // 参数越界 → invalid_argument（值没变）。
    await expectLater(
      session.updateOrganizeSettings(
        settings: _copy(
          current,
          templateId: 'compress',
          templateParams: {'targetChars': '9'},
        ),
        expectedRevision: current.revision,
      ),
      throwsA(
          isA<BridgeError>().having((e) => e.code, 'code', 'invalid_argument')),
    );

    // 过期 revision → revision_conflict（零改动）。
    await expectLater(
      session.updateOrganizeSettings(
        settings: _copy(current, enabled: true),
        expectedRevision: current.revision + 5,
      ),
      throwsA(isA<BridgeError>()
          .having((e) => e.code, 'code', 'revision_conflict')),
    );

    final after = await session.organizeSettings();
    expect(after.enabled, isFalse);
    expect(after.templateId, current.templateId);
    expect(after.revision, current.revision);
  });

  test('开启后：状态里能看出会跑哪些材料；驱动一轮产出候选、不改当前正文', () async {
    final dayKey = await capture('今天去公园散步，风有点大。', 'op-org');

    // 开启整理（把计划时刻设成 00:00：这一天一定已经到点）。
    final before = await session.organizeSettings();
    final enabled = await session.updateOrganizeSettings(
      settings: _copy(
        before,
        enabled: true,
        autoOrganizeTime: '00:00',
      ),
      expectedRevision: before.revision,
    );
    expect(enabled.enabled, isTrue);
    expect(enabled.revision, before.revision + 1);

    // 「什么时候会跑、会跑哪些材料」：状态里逐日列出条数与字数。
    final status = await session.organizeStatus();
    expect(status.settings.enabled, isTrue);
    expect(status.template.id, 'merge_by_theme');
    final pending = status.dueDays.map((d) => d.dayKey).toList();
    expect(pending, contains(dayKey));
    final day = status.dueDays.firstWhere((d) => d.dayKey == dayKey);
    expect(day.materials, 1);
    expect(day.chars, greaterThan(0));
    expect(day.reason, 'first');
    expect(day.currentVersionId, isNull);
    expect(status.quota.callsLast24H, 0);
    expect(status.quota.callsRemaining, 30);
    // 没有可用服务时如实说明（任务会停在等待配置，不会发出请求）。
    expect(status.providerCode, 'provider_not_configured');
    expect(status.providerNextStep, isNotNull);
    final notes = status.notes.join('\n');
    expect(notes, contains('最近 7 天'), reason: '状态要说清观察窗');

    // 跑一轮驱动：任务被排出来，但停在「等待配置」——**没有服务就绝不发请求**。
    final summary = await session.runDueJobs(budgetMs: 30000, trigger: 'test');
    expect(summary.succeeded, 0);
    expect(summary.dueBacklog, 0);

    final jobs = await session.listJobs(states: null, limit: 20);
    final organizeJob = jobs.firstWhere((job) => job.kind == 'diary_generate');
    expect(organizeJob.state, JobState.waitingConfiguration);
    expect(organizeJob.errorCode, 'provider_not_configured');
    expect(organizeJob.requiresUserAction, isTrue);

    // 自动整理的结果**一律进候选**：这里没有版本可产（没服务），但状态如实：
    // 最近一次自动整理记着它、没有版本、也没有任何调用。
    final afterStatus = await session.organizeStatus();
    final lastRun = afterStatus.lastRun;
    expect(lastRun, isNotNull);
    expect(lastRun!.dayKey, dayKey);
    expect(lastRun.versionId, isNull);
    expect(lastRun.calls, 0);
    expect(afterStatus.quota.callsLast24H, 0);

    // 这一天还没有任何版本（连候选都没有）：不谎报成功。
    final afterDrive = await session.getDiaryDay(dayKey: dayKey);
    expect(afterDrive.currentVersionId, isNull);
    expect(afterDrive.candidateVersionIds, isEmpty);

    // 关掉整理：排期不再排新任务（默认关的口径在运行期同样成立）。
    final off = await session.updateOrganizeSettings(
      settings: _copy(enabled, enabled: false),
      expectedRevision: enabled.revision,
    );
    expect(off.enabled, isFalse);
  });

  test('未开启时驱动一整轮：没有新版本、没有调用（默认关的硬要求）', () async {
    // 先显式关掉（不依赖上一个用例留下的状态）。
    final current = await session.organizeSettings();
    if (current.enabled) {
      await session.updateOrganizeSettings(
        settings: _copy(current, enabled: false),
        expectedRevision: current.revision,
      );
    }
    final dayKey = await capture('这一条不该被自动整理碰到。', 'op-off');
    final status = await session.organizeStatus();
    expect(status.settings.enabled, isFalse);
    // 关着也照实列出「开启后会跑什么」。
    expect(status.dueDays.map((d) => d.dayKey), contains(dayKey));

    await session.runDueJobs(budgetMs: 30000, trigger: 'test');
    final afterDrive = await session.getDiaryDay(dayKey: dayKey);
    expect(afterDrive.currentVersionId, isNull);
    expect(
      afterDrive.candidateVersionIds,
      isEmpty,
      reason: '未开启时绝不产出任何版本',
    );
    final after = await session.organizeStatus();
    expect(after.quota.callsLast24H, 0);
  });
}

/// 生成的对象没有 `copyWith`：测试里手动拷一份再改，避免漏字段。
OrganizeSettings _copy(
  OrganizeSettings source, {
  bool? enabled,
  String? templateId,
  Map<String, String>? templateParams,
  String? autoOrganizeTime,
}) =>
    OrganizeSettings(
      enabled: enabled ?? source.enabled,
      templateId: templateId ?? source.templateId,
      templateParams: templateParams ?? source.templateParams,
      maxMaterials: source.maxMaterials,
      maxChars: source.maxChars,
      maxCallsPerRun: source.maxCallsPerRun,
      maxCallsPerDay: source.maxCallsPerDay,
      maxFailureRate: source.maxFailureRate,
      autoOrganizeTime: autoOrganizeTime ?? source.autoOrganizeTime,
      supplementOnNewMaterials: source.supplementOnNewMaterials,
      revision: source.revision,
      updatedAt: source.updatedAt,
    );

String _defaultLibraryPath() {
  if (Platform.isLinux) return '../../target/release/libdiary_bridge.so';
  if (Platform.isWindows) {
    return r'..\..\target\x86_64-pc-windows-msvc\release\diary_bridge.dll';
  }
  if (Platform.isMacOS) return '../../target/release/libdiary_bridge.dylib';
  throw UnsupportedError('未知平台：${Platform.operatingSystem}');
}